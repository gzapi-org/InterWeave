// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Swarm-owned Kademlia driver (kademlia-integration.md §20).
//!
//! Consumes [`KademliaCommand`]s from the provider's side of the port
//! and answers with [`KademliaEvent`]s; owns everything libp2p-shaped —
//! `kad::Behaviour`, the §7 admission pipeline, the §4 namespace — so
//! the provider crate never sees a libp2p type.
//!
//! Kademlia here is **peer routing only** (ADR-0009): records are
//! filtered at the behaviour (`StoreInserts::FilterBoth`), inbound
//! write attempts are counted and dropped (§12), and nothing in this
//! module can read or write a record — the port has no command for it.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::num::NonZeroUsize;
use std::time::Duration;

use libp2p::kad;
use libp2p::kad::store::MemoryStore;
use libp2p::{Multiaddr, PeerId, StreamProtocol, identify};
use sha2::{Digest, Sha256};

use interweave_kademlia_control_api::{
    KademliaCommand, KademliaEvent, KademliaMode, MAX_RESULTS_PER_QUERY, QueryClass, QueryFailure,
    QueryHandle, QueryOrigin,
};
use interweave_transport_api::TransportIdentity;
use interweave_transport_runtime::{ConnectionClass, ConnectionManager};

use super::to_transport_identity;

use libp2p::swarm::SwarmEvent as Libp2pSwarmEvent;

/// The identity-multihash envelope of a libp2p Ed25519 public-key
/// protobuf. A `12D3KooW…` PeerId is exactly this envelope around the
/// 32 key bytes, which is what lets a targeted lookup key rebuild the
/// full identity.
const ED25519_ENVELOPE: [u8; 6] = [0x00, 0x24, 0x08, 0x01, 0x12, 0x20];

/// RFC 4648 base32, lower-cased, for the §4 namespace tag.
const BASE32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// Peers whose Identify advertisement is remembered, at most.
///
/// Bounded because the advertisement map is written by whoever
/// connects: the table it feeds holds `max_routing_peers`, so evidence
/// about four times that is already more than admission can ever use.
const MAX_OBSERVED_PEERS: usize = 1024;

/// Library-started queries tracked at once, at most.
///
/// The bound this repository owns. `max_concurrent_queries` governs
/// COMMANDED work and an implicit query never enters that map, so
/// without this the only thing limiting them is `bootstrap::Status`
/// declining a second automatic bootstrap while one runs — true of the
/// pinned libp2p, and not a property of this code. Generous relative to
/// what the library actually starts: this is a ceiling, not a budget.
const MAX_IMPLICIT_QUERIES: usize = 16;

/// Offers waiting for Identify evidence, at most.
const MAX_PENDING_OFFERS: usize = 256;

/// `^[a-z0-9][a-z0-9._-]{0,63}$`, per §4.
#[must_use]
pub fn network_id_is_legal(id: &str) -> bool {
    let mut bytes = id.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    if id.len() > 64 {
        return false;
    }
    bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
}

/// `SHA-256("interweave/kad-network/v1\0" || ASCII(network_id))`,
/// base32 of the first 16 bytes, lower case, unpadded (§4).
///
/// The 16-byte TRUNCATION is load-bearing: hashing to a full digest
/// yields a plausible namespace nobody else computes. Checked against
/// every vector in `fixtures/kademlia/kad-network-namespace-v1.json`.
#[must_use]
pub fn network_hash(network_id: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"interweave/kad-network/v1\0");
    h.update(network_id.as_bytes());
    let digest = h.finalize();

    let mut out = String::with_capacity(26);
    let mut acc: u32 = 0;
    let mut bits = 0_u32;
    for &b in &digest[..16] {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(BASE32[((acc >> bits) & 0x1f) as usize]));
        }
    }
    if bits > 0 {
        out.push(char::from(BASE32[((acc << (5 - bits)) & 0x1f) as usize]));
    }
    out
}

/// `/interweave/kad/1.0.0/<network-hash>` (§4).
#[must_use]
pub fn kad_protocol(network_id: &str) -> String {
    format!("/interweave/kad/1.0.0/{}", network_hash(network_id))
}

/// Rebuild the full PeerId a 32-byte targeted-lookup key names.
///
/// The port's key space is the identifier space: for InterWeave
/// identities that is the Ed25519 public key, and the rest of the
/// PeerId is this constant envelope.
///
/// Takes bytes that are KNOWN to be a public key. Only
/// [`interweave_kademlia_control_api::LookupKey::Ed25519PublicKey`]
/// yields them, so an exploration point cannot arrive here: any 32
/// bytes make a syntactically valid PeerId, and one built from a
/// key-space point names a peer that does not exist.
#[must_use]
pub fn peer_from_lookup_key(key: [u8; 32]) -> Option<PeerId> {
    let mut bytes = [0_u8; 38];
    bytes[..6].copy_from_slice(&ED25519_ENVELOPE);
    bytes[6..].copy_from_slice(&key);
    PeerId::from_bytes(&bytes).ok()
}

/// Static driver configuration, validated before the Swarm exists.
#[derive(Debug, Clone)]
pub struct KademliaSettings {
    /// Client or server, explicit (§5). Never `auto`.
    pub mode: KademliaMode,
    /// The non-secret deployment namespace (§4).
    pub network_id: String,
    /// K-bucket width (§13).
    pub kbucket_size: NonZeroUsize,
    /// Per-query timeout.
    pub query_timeout: Duration,
    /// Query fan-out.
    pub parallelism: NonZeroUsize,
    /// Whether query paths must be disjoint.
    pub disjoint_query_paths: bool,
    /// Project-level routing-table ceiling, enforced BEFORE manual
    /// insertion (§11) — a population bound, never an address freeze.
    pub max_routing_peers: usize,
    /// Results accepted from one query.
    pub max_results_per_query: NonZeroUsize,
    /// Concurrent query ceiling (§13's `max_concurrent_queries`),
    /// enforced by the DRIVER as well as the provider's budgets: this
    /// port is public, and a caller that bypassed the provider could
    /// otherwise pump the bounded command channel into unbounded
    /// long-lived queries.
    pub max_concurrent_queries: NonZeroUsize,
}

impl KademliaSettings {
    /// Refuse a configuration the driver cannot honour.
    ///
    /// THE SAME CEILINGS THE CANONICAL CONFIGURATION ENFORCES (§13, and
    /// `profile-config`'s per-field ranges), because this boundary is
    /// reachable without it: `SubstrateConfig` is public, and a caller
    /// that bypassed profile validation could otherwise install a
    /// routing table or query fan-out past every documented memory and
    /// work-amplification bound. Values are refused, never clamped — a
    /// caller learns its configuration was wrong instead of quietly
    /// getting another.
    ///
    /// # Errors
    /// A static description of the first violated rule.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !network_id_is_legal(&self.network_id) {
            return Err("kademlia network_id is not ^[a-z0-9][a-z0-9._-]{0,63}$");
        }
        if !(8..=20).contains(&self.kbucket_size.get()) {
            return Err("kademlia kbucket_size must be 8..=20");
        }
        if !(20..=1024).contains(&self.max_routing_peers) {
            return Err("kademlia max_routing_peers must be 20..=1024");
        }
        if self.parallelism.get() > 10 {
            return Err("kademlia parallelism must be 1..=10");
        }
        if self.max_concurrent_queries.get() > 8 {
            return Err("kademlia max_concurrent_queries must be 1..=8");
        }
        if !(5_000..=120_000).contains(&self.query_timeout.as_millis()) {
            return Err("kademlia query_timeout must be 5s..=120s");
        }
        if self.max_results_per_query.get() > MAX_RESULTS_PER_QUERY {
            return Err("kademlia max_results_per_query exceeds the port ceiling");
        }
        // THE CROSS-FIELD RULE, which the independent ceilings above
        // cannot express. Review finding on PR #61: every field passed
        // its own range while `kbucket_size = 8` with
        // `max_results_per_query = 20` sailed through — a query result
        // limit the selected bucket width is not allowed to support.
        // `kademlia-control-api::validate_limits` is the canonical
        // statement of it, and this boundary is reachable without it
        // because `SubstrateConfig` is public. The same hole the
        // provider's own constructor had, one crate over.
        if self.max_results_per_query.get() > self.kbucket_size.get() {
            return Err("kademlia max_results_per_query must not exceed kbucket_size");
        }
        Ok(())
    }
}

/// Build the behaviour with the §11 mapping, verbatim from what
/// SPIKE-003 validated against this exact libp2p version.
///
/// # Errors
/// A static description when the derived protocol is not a legal
/// stream protocol — unreachable for settings `validate` accepted, and
/// answered rather than panicked in a transport daemon.
pub fn build_behaviour(
    settings: &KademliaSettings,
    local: PeerId,
) -> Result<kad::Behaviour<MemoryStore>, &'static str> {
    let Ok(protocol) = StreamProtocol::try_from_owned(kad_protocol(&settings.network_id)) else {
        return Err("the derived kademlia protocol is not a legal stream protocol");
    };
    let mut config = kad::Config::new(protocol);
    // MANUAL: merely connecting to a peer must not put it in the DHT
    // routing table (§7); the admission pipeline below is the only way
    // in.
    config.set_kbucket_inserts(kad::BucketInserts::Manual);
    config.set_kbucket_size(settings.kbucket_size);
    config.set_query_timeout(settings.query_timeout);
    config.set_parallelism(settings.parallelism);
    config.disjoint_query_paths(settings.disjoint_query_paths);
    // The provider's scheduler owns bootstrap pacing; inheriting the
    // library's periodic default would run a second, unbudgeted one.
    config.set_periodic_bootstrap_interval(None);
    config.set_caching(kad::Caching::Disabled);
    // Peer routing only: inbound record writes are filtered at the
    // behaviour, before any code of ours could mishandle them (§12).
    config.set_record_filtering(kad::StoreInserts::FilterBoth);
    config.set_publication_interval(None);
    config.set_replication_interval(None);
    config.set_provider_publication_interval(None);
    let mut behaviour = kad::Behaviour::with_config(local, MemoryStore::new(local), config);
    behaviour.set_mode(Some(match settings.mode {
        KademliaMode::Client => kad::Mode::Client,
        KademliaMode::Server => kad::Mode::Server,
    }));
    Ok(behaviour)
}

/// The driver's own bookkeeping beside the behaviour.
#[derive(Debug)]
pub(super) struct KademliaState {
    /// The exact server protocol this network speaks, for Identify
    /// comparison — the FULL string, never a prefix.
    protocol: String,
    max_routing_peers: usize,
    max_results_per_query: usize,
    max_concurrent_queries: usize,
    /// Peers this driver has admitted to the routing table.
    routed: BTreeSet<PeerId>,
    /// The driver's implicit-handle counter. Names the queries the
    /// LIBRARY starts, in the half of the handle space the provider
    /// does not use — so the two minters cannot collide.
    next_implicit: u64,
    /// Commanded queries in flight, by the class each was issued for.
    queries: HashMap<kad::QueryId, (QueryClass, QueryHandle)>,
    /// Queries the LIBRARY started that this driver has already seen
    /// and announced (F2).
    ///
    /// THE FACT NO GUARD COULD SUPPLY. An implicit bootstrap is absent
    /// from `queries` by construction, so "not commanded" cannot tell a
    /// query that has just appeared from one that has been running since
    /// an earlier insertion — and four review rounds on PR #61 got that
    /// wrong one direction at a time. Remembering which ones are already
    /// known makes "new" answerable by looking.
    implicit: HashMap<kad::QueryId, QueryHandle>,
    /// Candidates accumulated per query across progress steps.
    results: HashMap<kad::QueryId, Vec<interweave_discovery_api::CandidatePeer>>,
    /// Whether each recently seen peer advertises the exact server
    /// protocol, REPLACED on every Identify — a handler that unioned
    /// could never observe a withdrawal (F5) — with the time it was
    /// last observed, so the cap displaces the stalest rather than
    /// refusing the newest.
    advertises: BTreeMap<PeerId, (bool, u64)>,
    /// Offered addresses waiting for Identify evidence, with the time
    /// the stash was last written, for the same reason.
    pending_offers: BTreeMap<PeerId, (BTreeSet<Multiaddr>, u64)>,
    /// Seats claimed optimistically that the behaviour has not yet
    /// confirmed with `RoutingUpdated`. An `add_address` answering
    /// `Pending` is queued behind a disconnected occupant and may never
    /// land; without this, an abandoned insertion left a seat nothing
    /// could remove.
    unconfirmed: BTreeSet<PeerId>,
    /// Inbound record writes dropped, counted never stored (§12).
    record_writes_dropped: u64,
    /// Shutting down: new queries are refused.
    stopping: bool,
}

impl KademliaState {
    /// Fresh bookkeeping for one configured driver.
    pub(super) fn new(settings: &KademliaSettings) -> Self {
        Self {
            protocol: kad_protocol(&settings.network_id),
            max_routing_peers: settings.max_routing_peers,
            max_results_per_query: settings.max_results_per_query.get(),
            max_concurrent_queries: settings.max_concurrent_queries.get(),
            routed: BTreeSet::new(),
            next_implicit: 0,
            queries: HashMap::new(),
            implicit: HashMap::new(),
            results: HashMap::new(),
            advertises: BTreeMap::new(),
            pending_offers: BTreeMap::new(),
            unconfirmed: BTreeSet::new(),
            record_writes_dropped: 0,
            stopping: false,
        }
    }

    /// Queries this driver has outstanding, commanded or implicit.
    ///
    /// The progress slack a settlement may use: each one holds a
    /// provider budget permit that only a completion releases — and
    /// that is as true of a library-started query as of a commanded
    /// one, so counting only `queries` understated the slack by up to
    /// [`MAX_IMPLICIT_QUERIES`]. The doc said "commanded or implicit"
    /// while the body returned commanded; the body was wrong.
    ///
    /// Bounded by `max_concurrent_queries + MAX_IMPLICIT_QUERIES`, both
    /// of which this crate owns, and each half has the test that fails
    /// if its refusal goes away:
    /// `the_driver_caps_concurrent_queries_and_settles_the_refusal` for
    /// the commanded half, `a_stopping_driver_announces_no_query_and_the_population_is_bounded`
    /// for the other. An earlier version of this sentence said the pool
    /// bounds the implicit half — that was the same mistaken claim
    /// corrected in `kademlia-integration.md` §11 and in the provider,
    /// and this was its third site.
    pub(super) fn outstanding_queries(&self) -> usize {
        self.queries.len() + self.implicit.len()
    }

    /// Events a shutdown right now would EMIT, not queries it would
    /// settle.
    ///
    /// **Includes the ones it has not met yet.** The sweep enumerates
    /// live queries in NEITHER map and settles those too, and the slack
    /// is computed before the command runs — so a sweep could emit more
    /// than the outbox would admit, and a dropped settlement is a permit
    /// held for the life of the process. The third population has to be
    /// counted before it is discovered.
    ///
    /// AND AN UNMET QUERY COSTS TWO EVENTS, not one. Review finding on
    /// PR #64: this counted queries while the sweep emits a
    /// `QueryStarted` AND a `QueryFailed` for each one it has never
    /// announced, so the arithmetic held only while at most one such
    /// query existed. Beyond that the surplus was dropped silently, and
    /// because the sweep pushed every announcement before any
    /// settlement, what fell off the end was releases for charges the
    /// provider had just taken — the exact leak this PR exists to
    /// remove, reintroduced by the sweep's own pairing. The emission
    /// order is fixed too, so a truncated flush cannot separate a pair;
    /// this function counts what is emitted so the outbox can hold it.
    pub(super) fn settleable_queries(
        &self,
        behaviour: Option<&kad::Behaviour<MemoryStore>>,
    ) -> usize {
        let unknown = behaviour.map_or(0, |b| {
            b.iter_queries()
                .filter(|q| {
                    !self.queries.contains_key(&q.id()) && !self.implicit.contains_key(&q.id())
                })
                .count()
        });
        self.outstanding_queries() + unknown.saturating_mul(2)
    }

    /// Inbound record writes dropped so far (§12: counted, never stored).
    ///
    /// Test-gated until Stage 12: the §16 diagnostics snapshot is the
    /// production reader, and it was deferred by owner decision until a
    /// consumer exists. The counter itself is maintained regardless.
    #[cfg(test)]
    pub(super) const fn record_writes_dropped(&self) -> u64 {
        self.record_writes_dropped
    }
}

/// Keep finishing every live query while the driver is stopping.
///
/// **One `finish` does not stop a bootstrap, it advances one.** The call
/// marks the peer-iterator finished; `query_finished` then re-inserts
/// the SAME `QueryId` with a fresh iterator for the next bucket, and
/// only the exhaustion of `remaining` sets `step.last`. So the shutdown
/// sweep's single pass left a drained node issuing FIND_NODE and
/// attempting query dials through every remaining bucket — the one
/// thing the drain exists to stop.
///
/// Re-finishing on each event is what actually terminates it: a
/// finished iterator yields no peers, so each re-entered bucket
/// sub-query completes on the following poll having done no network
/// work, and `remaining` drains in a few polls instead of a few
/// timeouts.
///
/// `iter_queries` is not evidence either way — it filters finished
/// queries out, so it reports the mark rather than the end. That is why
/// a test asserting termination through it must use a bootstrap and not
/// a closest-peers walk, which is the one class where `finish` really
/// is terminal.
fn finish_all_while_stopping(behaviour: &mut kad::Behaviour<MemoryStore>) {
    for mut running in behaviour.iter_queries_mut() {
        running.finish();
    }
}

/// Notice every query the LIBRARY started, and announce it once.
///
/// **The fact four review rounds could not guess.** An implicit
/// bootstrap (SPIKE-003 F2) never passes through `handle_command`, so it
/// is absent from `queries` by construction — and "absent from
/// `queries`" was therefore true of a query that had just appeared AND
/// of one that had been running since an earlier insertion. PR #61
/// round 8 established that cancelling all of them is wrong; round 9
/// established that cancelling none is also wrong. Both are the same
/// missing fact, and no guard could supply it.
///
/// Remembering which ones are already known makes "new" answerable by
/// looking. A newly seen id is announced so the provider can charge it;
/// one that has vanished is settled, because a query that ended without
/// a completion still holds a permit.
///
/// Called from the Kademlia behaviour-event arm and from `Shutdown` —
/// NOT from every Swarm event, and the library does not start a query
/// at an event anyway: `Behaviour::poll` triggers its automatic
/// bootstrap silently, behind a throttle. So this catches a
/// library-started query only while one is still in the pool when some
/// Kademlia event arrives. The query that produces no event but its own
/// completion is caught there instead, in the Bootstrap arm, which
/// announces an unknown completion before settling it.
///
/// Two paths for one obligation, because the library gives no single
/// moment that covers both.
fn reconcile_implicit(
    state: &mut KademliaState,
    behaviour: &kad::Behaviour<MemoryStore>,
    out: &mut Vec<KademliaEvent>,
) {
    let live: HashSet<kad::QueryId> = behaviour.iter_queries().map(|q| q.id()).collect();
    for id in &live {
        if state.queries.contains_key(id) || state.implicit.contains_key(id) {
            continue;
        }
        // A STOPPING DRIVER ANNOUNCES NOTHING. Review finding on PR
        // #64: `QueryMut::finish` does not cancel a bootstrap — the
        // pool calls `continue_iter_closest`, which re-inserts the SAME
        // `QueryId` for the next bucket. The re-entered query then
        // reached here, matched neither map, and was announced again
        // under a fresh handle: a `QueryStarted` for a query already
        // reported as shut down.
        if state.stopping {
            continue;
        }
        // AND THE POPULATION IS BOUNDED. `max_concurrent_queries` is
        // tested against `queries` — commanded work — and an implicit
        // query never enters that map, so before this nothing in THIS
        // repository bounded them. What did was `bootstrap::Status`
        // refusing a second automatic bootstrap while one is
        // outstanding, which is a pinned dependency's private state
        // machine rather than a property of this code.
        if state.implicit.len() >= MAX_IMPLICIT_QUERIES {
            continue;
        }
        state.next_implicit = state.next_implicit.wrapping_add(1);
        let handle = QueryHandle::implicit(state.next_implicit);
        state.implicit.insert(*id, handle);
        out.push(KademliaEvent::QueryStarted {
            handle,
            class: QueryClass::Bootstrap,
            origin: QueryOrigin::Implicit,
        });
    }
    // GONE WITHOUT A COMPLETION still owes a settlement: the permit the
    // announcement created is released by nothing else.
    let vanished: Vec<kad::QueryId> = state
        .implicit
        .keys()
        .filter(|id| !live.contains(id))
        .copied()
        .collect();
    for id in vanished {
        if let Some(handle) = state.implicit.remove(&id) {
            out.push(KademliaEvent::QueryFailed {
                handle,
                class: QueryClass::Bootstrap,
                reason: QueryFailure::TimedOut,
            });
        }
    }
}

/// Apply one provider command to the behaviour.
///
/// Returns the port events the command produced immediately; most
/// answers arrive later through [`handle_kademlia`].
pub(super) fn handle_command(
    state: &mut KademliaState,
    behaviour: &mut kad::Behaviour<MemoryStore>,
    manager: &ConnectionManager,
    command: KademliaCommand,
    now_ms: u64,
) -> Vec<KademliaEvent> {
    let mut out = Vec::new();
    // STOPPED MEANS STOPPED, for every command. The guard lived only on
    // the two obvious arms, and a SetMode { Server } arriving after
    // Shutdown re-enabled serving and re-advertised the DHT protocol —
    // the one thing the lifecycle's "disable participation" exists to
    // prevent. A refused query is still SETTLED, or the caller's
    // accounting waits forever.
    if state.stopping {
        if let KademliaCommand::StartQuery { handle, class, .. } = command {
            out.push(KademliaEvent::QueryFailed {
                handle,
                class,
                reason: QueryFailure::ShuttingDown,
            });
        }
        return out;
    }
    match command {
        KademliaCommand::SetMode { mode } => {
            // Explicit, never inferred (§5) — and never `auto`.
            behaviour.set_mode(Some(match mode {
                KademliaMode::Client => kad::Mode::Client,
                KademliaMode::Server => kad::Mode::Server,
            }));
        }
        KademliaCommand::OfferRoutingPeer { addresses, peer } => {
            let Ok(pid) = peer.as_str().parse::<PeerId>() else {
                return out;
            };
            if manager.is_local_peer(&peer) {
                return out;
            }
            // TRUST FIRST, then the waiting room (§7's pipeline order).
            // An offer for a peer that can never hold a seat is not
            // stashed at all: `try_admit` would refuse it below, but
            // only after it had taken one of the bounded slots, and it
            // never gives them back.
            if !may_hold_a_seat(manager, &peer) {
                return out;
            }
            // STASHED, not inserted: an offer is a hint, and §7's
            // pipeline requires authenticated Identify evidence of the
            // exact server protocol before anything reaches the routing
            // table. The addresses wait, bounded, for that evidence.
            if !state.pending_offers.contains_key(&pid) {
                make_room(&mut state.pending_offers, MAX_PENDING_OFFERS, |(_, at)| *at);
            }
            let (stash, seen) = state.pending_offers.entry(pid).or_default();
            *seen = now_ms;
            for offered in addresses.as_slice() {
                if stash.len() >= 64 {
                    break;
                }
                if let Some(addr) = suffix_checked_str(offered.as_str(), &pid) {
                    stash.insert(addr);
                }
            }
            out.extend(try_admit(state, behaviour, manager, pid, &peer, now_ms));
        }
        KademliaCommand::StartQuery { handle, class, key } => {
            // THE DRIVER'S OWN CEILING. The provider budgets its
            // commands, but the port is public: a caller pumping the
            // command channel faster than queries time out would grow
            // `queries`, `results` and the network work without bound.
            // Refused, and SETTLED as refused — a silent drop would
            // leave the caller's accounting waiting forever.
            if state.queries.len() >= state.max_concurrent_queries {
                out.push(KademliaEvent::QueryFailed {
                    handle,
                    class,
                    reason: QueryFailure::BudgetExhausted,
                });
                return out;
            }
            match class {
                QueryClass::Bootstrap => match behaviour.bootstrap() {
                    Ok(id) => {
                        state.queries.insert(id, (class, handle));
                    }
                    Err(_) => out.push(KademliaEvent::QueryFailed {
                        handle,
                        class,
                        reason: QueryFailure::NoRoutingPeers,
                    }),
                },
                // ONLY AN IDENTITY'S KEY REBUILDS AN IDENTITY. The
                // key used to be a bare `[u8; 32]` and this wrapped
                // whatever arrived in the Ed25519 envelope — any 32
                // bytes make a syntactically valid PeerId, so a key
                // that was not one named a peer that does not exist and
                // nothing here could tell. The type answers it now.
                QueryClass::Targeted => {
                    match key.as_public_key().copied().and_then(peer_from_lookup_key) {
                        Some(target) => {
                            let id = behaviour.get_closest_peers(target);
                            state.queries.insert(id, (class, handle));
                        }
                        // The provider refuses untargetable identities
                        // upstream; a key that decodes to no PeerId here is
                        // defence in depth, and the settlement must still
                        // arrive or the budget slot waits forever. "No
                        // routing peers" is the nearest bounded truth: there
                        // is nothing at that key to route toward.
                        None => out.push(KademliaEvent::QueryFailed {
                            handle,
                            class,
                            reason: QueryFailure::NoRoutingPeers,
                        }),
                    }
                }
                QueryClass::Exploration => {
                    let results =
                        NonZeroUsize::new(state.max_results_per_query).unwrap_or(NonZeroUsize::MIN);
                    let id = behaviour.get_n_closest_peers(key.bytes().to_vec(), results);
                    state.queries.insert(id, (class, handle));
                }
            }
        }
        KademliaCommand::Shutdown => {
            state.stopping = true;
            // §Lifecycle: stop new queries, settle in-flight work.
            // Each outstanding commanded query settles as shutting
            // down; a completion arriving later finds no entry and is
            // ignored. Client mode withdraws the server protocol.
            behaviour.set_mode(Some(kad::Mode::Client));
            // CANCELLED, not merely forgotten. Review finding on PR
            // #61: draining the map reported each query as
            // `ShuttingDown` and left the libp2p query running, so the
            // behaviour went on sending requests over existing
            // connections and attempting query dials until its own
            // timeout — after the provider had been told the work
            // ended. Under `Drain` that is plainly visible, because the
            // Swarm task stays alive; the lifecycle's "disable
            // participation" has to mean the queries too, not only the
            // mode.
            //
            // `finish` does NOT end a bootstrap. It marks the
            // peer-iterator finished; `query_finished` then calls
            // `continue_iter_closest` with the SAME id and a fresh
            // iterator for the next bucket, and only sets `step.last`
            // once `remaining` is exhausted. One call therefore
            // ADVANCES a bootstrap rather than stopping it — which is
            // why `finish_all_while_stopping` re-finishes on every
            // event until the pool drains, and why `iter_queries()`
            // going quiet is not evidence of termination: it filters
            // finished queries out, so it reports the mark, not the
            // end.
            // THE IMPLICIT ONES TOO, and now they have names. A
            // library-started bootstrap (F2) never passes through this
            // function, so it is absent from `queries`; `reconcile_implicit`
            // is what noticed it and told the provider to charge it, and
            // `state.implicit` is what remembers the handle that charge
            // is keyed by. Draining only `queries` left those queries
            // running and their charges held for the life of the
            // provider.
            //
            // Collected before finishing, because `iter_queries` borrows
            // the behaviour that `query_mut` needs.
            // DISCOVERY, NOT ANNOUNCEMENT. The sweep used to call
            // `reconcile_implicit` to find library-started queries so it
            // could settle them, and that stopped working the moment
            // reconcile learned to stay silent while stopping — a guard
            // added because `QueryMut::finish` does not cancel a
            // bootstrap (the pool re-inserts the same id for the next
            // bucket) and the re-entered query was being announced
            // again after shutdown.
            //
            // The two needs are different: reconcile must not announce
            // NEW work during a drain, and the sweep must still
            // enumerate what is live in order to settle it. So the
            // sweep enumerates for itself, and a query it has never
            // announced is announced and settled in the same pass — the
            // charge and its release still one object, as everywhere
            // else.
            // ANNOUNCED AND SETTLED ADJACENTLY, and never tracked.
            // Review finding on PR #64 against the first version of
            // this sweep: it announced every unmet query, then settled
            // every query, in two passes. So a truncated outbox flush —
            // `flush_outbox` stops at the first full channel and
            // discards the rest — dropped releases for charges it had
            // just delivered. Emitting each pair together means a
            // truncation can lose a whole pair, which costs nothing,
            // but cannot separate one.
            //
            // These queries also do not enter `state.implicit`. The map
            // is drained by this same command, so inserting into it
            // bought nothing and made `MAX_IMPLICIT_QUERIES` — a bound
            // on what the driver TRACKS at once — momentarily false. A
            // query announced and settled in one pass is never tracked.
            let unknown: Vec<kad::QueryId> = behaviour
                .iter_queries()
                .map(|q| q.id())
                .filter(|id| !state.queries.contains_key(id) && !state.implicit.contains_key(id))
                .collect();
            let mut settling: Vec<(kad::QueryId, QueryClass, QueryHandle, bool)> = state
                .queries
                .drain()
                .map(|(id, (class, handle))| (id, class, handle, false))
                .chain(
                    state
                        .implicit
                        .drain()
                        .map(|(id, handle)| (id, QueryClass::Bootstrap, handle, false)),
                )
                .collect();
            for id in unknown {
                state.next_implicit = state.next_implicit.wrapping_add(1);
                let handle = QueryHandle::implicit(state.next_implicit);
                settling.push((id, QueryClass::Bootstrap, handle, true));
            }
            for (id, class, handle, announce) in settling {
                if let Some(mut running) = behaviour.query_mut(&id) {
                    running.finish();
                }
                if announce {
                    out.push(KademliaEvent::QueryStarted {
                        handle,
                        class,
                        origin: QueryOrigin::Implicit,
                    });
                }
                out.push(KademliaEvent::QueryFailed {
                    handle,
                    class,
                    reason: QueryFailure::ShuttingDown,
                });
            }
            state.queries.clear();
            state.results.clear();
            state.pending_offers.clear();
        }
    }
    out
}

/// Whether this peer could ever hold a routing seat.
///
/// §7's pipeline puts `PeerTrustPolicy authorization` BEFORE the
/// authenticated Identify step, and the stashes below are that step's
/// waiting room. Asked here rather than only in [`try_admit`] because a
/// bounded table filled by peers that can never pass the later check is
/// a table an adversary closes: mDNS on a shared LAN offers a candidate
/// for every node it sees, and the first `MAX_PENDING_OFFERS` of them
/// held their slots for the life of the process.
fn may_hold_a_seat(manager: &ConnectionManager, identity: &TransportIdentity) -> bool {
    manager.classify(identity) == ConnectionClass::DataPlaneTrusted
}

/// Make room in a bounded observation map by dropping its STALEST entry.
///
/// Displacement, not refusal. Refusing the newcomer at the cap lets
/// whatever arrived first hold its slot forever, so a table that filled
/// once never admitted anyone again — a permanent denial dressed as a
/// bound. Dropping the stalest costs the least useful entry, and the
/// evidence it held is re-learned from the next Identify.
fn make_room<V>(map: &mut BTreeMap<PeerId, V>, cap: usize, at: impl Fn(&V) -> u64) {
    while map.len() >= cap {
        let Some(stalest) = map
            .iter()
            .min_by_key(|(peer, v)| (at(v), **peer))
            .map(|(peer, _)| *peer)
        else {
            return;
        };
        map.remove(&stalest);
    }
}

/// Forget what a closed connection was evidence for.
///
/// An Identify advertisement is an observation ABOUT A CONNECTION, so it
/// does not outlive one: keeping it made `advertises` grow monotonically
/// toward its cap with entries no reconnect would refresh. The stash
/// goes with it, because an offer waiting on evidence from a connection
/// that has ended is waiting on nothing.
///
/// AN UNCONFIRMED SEAT GOES TOO. Review finding on PR #61: the
/// phantom-seat rollback claimed "a disconnect reclaims it if it does
/// not [land]", and this function did not touch `routed` at all, so the
/// sentence described a mechanism that did not exist. `add_address`
/// answering `Pending` queues the peer behind a disconnected occupant;
/// if that insertion is abandoned — the candidate goes away before any
/// `RoutingUpdated` — nothing removed the optimistic seat, and churn
/// filled `max_routing_peers` with entries the table never held.
///
/// A CONFIRMED seat is left alone: `RoutingUpdated { is_new_peer }` is
/// the behaviour's own report, and a routing entry deliberately outlives
/// the connection that produced it. Only the unconfirmed claim is the
/// caller's to withdraw.
pub(super) fn forget_disconnected(state: &mut KademliaState, peer: &PeerId) {
    state.advertises.remove(peer);
    state.pending_offers.remove(peer);
    if state.unconfirmed.remove(peer) {
        state.routed.remove(peer);
    }
}

/// §7's admission pipeline, driver side: trust, Identify evidence of
/// the EXACT server protocol, the population bound, then — and only
/// then — `add_address`.
fn try_admit(
    state: &mut KademliaState,
    behaviour: &mut kad::Behaviour<MemoryStore>,
    manager: &ConnectionManager,
    pid: PeerId,
    identity: &TransportIdentity,
    now_ms: u64,
) -> Vec<KademliaEvent> {
    if state.stopping {
        return Vec::new();
    }
    // Routing peers are held to data-plane trust (§13's
    // routing_peer_policy). Discovery of anyone else is legal; RETAINING
    // them is not.
    if !may_hold_a_seat(manager, identity) {
        return Vec::new();
    }
    if !matches!(state.advertises.get(&pid), Some((true, _))) {
        // No authenticated Identify evidence of the exact server
        // protocol yet: the offer stays stashed until it arrives.
        return Vec::new();
    }
    // A POPULATION bound, never an address freeze (§11): a peer already
    // routed may always refresh its addresses.
    if !state.routed.contains(&pid) && state.routed.len() >= state.max_routing_peers {
        return Vec::new();
    }
    let mut addresses: BTreeSet<Multiaddr> = state
        .pending_offers
        .remove(&pid)
        .map(|(stash, _)| stash)
        .unwrap_or_default();
    for known in manager.dial_candidates(identity, now_ms) {
        if let Some(addr) = suffix_checked_str(&known, &pid) {
            addresses.insert(addr);
        }
    }
    if addresses.is_empty() {
        return Vec::new();
    }
    // OPTIMISTIC population accounting: `RoutingUpdated` is the truth
    // and arrives via poll, but two admissions inside one command batch
    // would both read the stale count, so the seat is claimed here.
    state.routed.insert(pid);
    // AND THE CLAIM IS ROLLED BACK WHEN THE TABLE REFUSED IT. The
    // return of `add_address` was discarded, and `RoutingUpdated` only
    // ever ADDS (`is_new_peer`) or removes an evicted `old_peer` — so a
    // peer whose every address came back `Failed` was never named by
    // any later event and its phantom seat was permanent. The count
    // then ratcheted monotonically toward `max_routing_peers`, after
    // which nothing new could be admitted at all. `Pending` is not a
    // refusal: the peer is queued and `RoutingUpdated` follows if it
    // lands, and a disconnect reclaims it if it does not.
    let mut accepted = false;
    let mut confirmed = false;
    for addr in addresses {
        match behaviour.add_address(&pid, addr) {
            kad::RoutingUpdate::Success => {
                accepted = true;
                confirmed = true;
            }
            // QUEUED, NOT HELD. `Pending` puts the peer behind a
            // disconnected occupant; `RoutingUpdated` follows only if
            // that occupant fails to respond, so the seat is a claim
            // this driver must be able to take back.
            kad::RoutingUpdate::Pending => accepted = true,
            kad::RoutingUpdate::Failed => {}
        }
    }
    if !accepted {
        state.routed.remove(&pid);
        state.unconfirmed.remove(&pid);
    } else if confirmed {
        state.unconfirmed.remove(&pid);
    } else {
        state.unconfirmed.insert(pid);
    }
    Vec::new()
}

/// Strip trailing `/p2p/…` components, REJECTING a foreign identity.
///
/// Every address this driver consumes arrives inside an observation
/// that names a peer, and a trailing `/p2p/B` on an address offered
/// for peer A is the observation contradicting itself. Silently
/// stripping B published the transport route as A's: dial capacity
/// spent on it, and after Noise refused, the quarantine landed under
/// the WRONG peer. The contradiction is rejected — `None` drops the
/// address, never the observation. `a_foreign_peer_suffix_rejects_the_address`
/// holds it for every caller, because every caller is this function.
fn suffix_checked(address: &Multiaddr, expected: &PeerId) -> Option<Multiaddr> {
    let mut parts: Vec<_> = address.iter().collect();
    while let Some(libp2p::multiaddr::Protocol::P2p(claimed)) = parts.last() {
        if claimed != expected {
            return None;
        }
        parts.pop();
    }
    Some(parts.into_iter().collect())
}

/// [`suffix_checked`] from an offered STRING, whose parse can also fail.
fn suffix_checked_str(address: &str, expected: &PeerId) -> Option<Multiaddr> {
    suffix_checked(&address.parse::<Multiaddr>().ok()?, expected)
}

/// What [`handle_kademlia`] did with an event.
pub(super) enum KadHandled {
    /// A Kademlia behaviour event: translated onto the port, nothing
    /// further to do.
    Consumed,
    /// Anything else — including Identify, which this module PEEKS at
    /// and passes on, because the settlement path and the consumer
    /// translation still need it.
    Passed(Box<Libp2pSwarmEvent<crate::behaviour::SubstrateBehaviourEvent>>),
}

/// The driver's view of one Swarm event.
pub(super) fn handle_kademlia(
    event: Libp2pSwarmEvent<crate::behaviour::SubstrateBehaviourEvent>,
    swarm: &mut crate::gated_swarm::GatedSwarm,
    state: &mut KademliaState,
    manager: &ConnectionManager,
    now_ms: u64,
    out: &mut Vec<KademliaEvent>,
) -> KadHandled {
    let handled = classify_swarm_event(event, swarm, state, manager, now_ms, out);
    // AFTER EVERY EVENT, not only a Kademlia one. Review finding on PR
    // #64: the library starts its automatic bootstrap inside
    // `Behaviour::poll` and emits no Kademlia event for it, so under
    // `BucketInserts::Manual` its FIRST and only Kademlia event is the
    // final completion. Reconciling only on Kademlia events therefore
    // never met such a query while it was running: the completion arm
    // announced and settled it in one pass, and for the whole time it
    // was dialling or waiting out its timeout the provider held no
    // charge against it — so commanded queries could spend the entire
    // concurrency and rate budget beside work nobody was counting.
    //
    // That query DIALS, and a dial produces `Dialing`,
    // `ConnectionEstablished` or `OutgoingConnectionError` — none of
    // them Kademlia events, all of them arriving before the
    // completion. Reconciling here narrows the unaccounted window from
    // the query's whole lifetime to a single event.
    //
    // It cannot be closed entirely: the dial is issued from inside the
    // same `Behaviour::poll` that starts the query, so no observer in
    // this process can announce it beforehand. What is achievable is
    // that the charge is held while the network work happens, and that
    // is what this does.
    if let Some(behaviour) = swarm.kademlia_mut() {
        if state.stopping {
            // The sweep's single `finish` only advanced each bootstrap;
            // this is what ends them.
            finish_all_while_stopping(behaviour);
        }
        reconcile_implicit(state, behaviour, out);
    }
    handled
}

/// Fold one Swarm event onto the port, without reconciliation.
///
/// Split out so [`handle_kademlia`] can reconcile on the way out of
/// every path, including the two that pass an event on early.
fn classify_swarm_event(
    event: Libp2pSwarmEvent<crate::behaviour::SubstrateBehaviourEvent>,
    swarm: &mut crate::gated_swarm::GatedSwarm,
    state: &mut KademliaState,
    manager: &ConnectionManager,
    now_ms: u64,
    out: &mut Vec<KademliaEvent>,
) -> KadHandled {
    use crate::behaviour::SubstrateBehaviourEvent;

    // PEEKED BY REFERENCE, passed on whole: Identify feeds three
    // consumers — this pipeline (F3), the address book, and the
    // consumer's `Identified` event — and consuming it here would
    // starve the other two.
    if let Libp2pSwarmEvent::Behaviour(SubstrateBehaviourEvent::Identify(
        identify::Event::Received { peer_id, info, .. },
    )) = &event
    {
        observe_identify(
            state,
            swarm.kademlia_mut(),
            manager,
            *peer_id,
            &info.protocols,
            &info.listen_addrs,
            now_ms,
            out,
        );
        return KadHandled::Passed(Box::new(event));
    }
    // A CLOSED CONNECTION TAKES ITS EVIDENCE WITH IT. An Identify
    // advertisement is an observation about a connection, so holding it
    // afterwards let `advertises` fill monotonically with entries
    // nothing would ever refresh — and once full, no new peer could be
    // admitted. Passed on, not consumed: the settlement path below owns
    // the slot accounting.
    if let Libp2pSwarmEvent::ConnectionClosed {
        peer_id,
        num_established: 0,
        ..
    } = &event
    {
        forget_disconnected(state, peer_id);
        return KadHandled::Passed(Box::new(event));
    }
    match event {
        Libp2pSwarmEvent::Behaviour(SubstrateBehaviourEvent::Kad(kad_event)) => {
            handle_kad_event(state, swarm.kademlia_mut(), manager, kad_event, now_ms, out);
            // The reconciliation that used to live here now runs in
            // `handle_kademlia`, after EVERY event. It is still the
            // single place "which implicit queries are new" is decided,
            // which is the whole point — four review rounds on PR #61
            // each answered that question differently at a different
            // call site.
            KadHandled::Consumed
        }
        other => KadHandled::Passed(Box::new(other)),
    }
}

/// Fold one `kad::Event` onto the port.
fn handle_kad_event(
    state: &mut KademliaState,
    behaviour: Option<&mut kad::Behaviour<MemoryStore>>,
    manager: &ConnectionManager,
    event: kad::Event,
    now_ms: u64,
    out: &mut Vec<KademliaEvent>,
) {
    match event {
        kad::Event::RoutingUpdated {
            peer,
            is_new_peer,
            old_peer,
            ..
        } => {
            // THE BEHAVIOUR'S OWN REPORT is the truth the optimistic
            // count in `try_admit` reconciles against.
            if is_new_peer {
                // REVALIDATED, not merely recorded. Review finding on
                // PR #61: `add_address` queues this event, and trust can
                // be revoked — or the peer can withdraw the server
                // protocol — between the queueing and the poll. Both of
                // those paths remove the peer from `routed` and tell the
                // behaviour to drop it, and this handler then inserted
                // it straight back and announced it, resurrecting a seat
                // the behaviour no longer holds and handing the provider
                // an addition that is already false.
                //
                // The same shape as the outbound gate's establishment
                // check, one layer down: admission answered once, at
                // queue time, and the state it depended on can move
                // before the answer is used.
                // AND THE DRIVER MUST STILL BE RUNNING. Review finding
                // on PR #61, against the revalidation itself: a
                // `Pending` insertion can land after `Drain` has shut
                // the driver down, and checking only trust and the
                // advertisement accepted it — recreating a routing seat
                // after shutdown and, when it is an empty-to-nonempty
                // transition, letting libp2p start a fresh implicit
                // bootstrap AFTER the shutdown sweep already ran. Query
                // dials during a drained lifetime is the one thing the
                // drain exists to stop.
                // EVERY CONDITION `try_admit` REQUIRED, asked again.
                // Issue #63 item 2: this rechecked trust, the
                // advertisement and the lifecycle, and not the
                // POPULATION — so if a pending peer lost its optimistic
                // seat on disconnect and another filled the table
                // before the queued update landed, a reconnect that
                // re-advertised first was declared eligible and the
                // insertion below grew `routed` past
                // `max_routing_peers`. A ceiling is not a conjunct you
                // can leave out of a revalidation that exists because
                // the state moved.
                //
                // `contains` first, because a peer already holding its
                // seat is refreshing rather than taking a new one —
                // §11's population bound is not an address freeze.
                //
                // AND A REPLACEMENT IS NOT A GROWTH. Review finding on
                // PR #64: the ceiling was tested against the population
                // BEFORE this event, while the eviction below belongs
                // to the same event and frees a seat. So a same-bucket
                // swap on a full table refused the newcomer for a seat
                // that was about to be vacated, and then vacated it —
                // shrinking a full table by one, and shrinking the real
                // one too, since `remove_peer` drops the newcomer the
                // behaviour had already inserted. What the bound
                // forbids is exceeding `max_routing_peers` after the
                // event, so the seat `old_peer` is giving up counts.
                let replacing = old_peer
                    .as_ref()
                    .is_some_and(|old| *old != peer && state.routed.contains(old));
                let eligible = to_transport_identity(&peer).ok().filter(|identity| {
                    !state.stopping
                        && may_hold_a_seat(manager, identity)
                        && matches!(state.advertises.get(&peer), Some((true, _)))
                        && (state.routed.contains(&peer)
                            || state.routed.len()
                                < state.max_routing_peers + usize::from(replacing))
                });
                // The claim is settled either way; only an ELIGIBLE
                // peer keeps the seat. Not an early return: the
                // `old_peer` eviction below belongs to this same event
                // and must still be reported.
                state.unconfirmed.remove(&peer);
                if let Some(admitted) = eligible {
                    state.routed.insert(peer);
                    out.push(KademliaEvent::RoutingPeerAdded { peer: admitted });
                } else if let Some(behaviour) = behaviour {
                    // THE BEHAVIOUR HAS ALREADY INSERTED IT. Review
                    // finding on PR #61, against the previous fix:
                    // `RoutingUpdated` is emitted AFTER the insertion,
                    // so declining to record it suppressed the
                    // `RoutingPeerAdded` and left the real routing entry
                    // in place — and an empty-to-nonempty insertion can
                    // still start an implicit bootstrap, which is the
                    // post-drain query activity the check was added to
                    // prevent. Refusing the bookkeeping is not refusing
                    // the seat.
                    behaviour.remove_peer(&peer);
                    // ONLY WHILE STOPPING, and the previous version of
                    // this sweep was not so restricted. An implicit
                    // bootstrap is absent from `queries` by
                    // construction, so "every query not in `queries`"
                    // cannot distinguish one THIS insertion started from
                    // one an earlier, perfectly eligible insertion did —
                    // and on a running driver it cancelled the latter
                    // and settled it falsely, releasing a charge for
                    // work that was still needed.
                    //
                    // The distinction cannot be made here: the insertion
                    // already happened before this event was emitted, so
                    // any query it started is indistinguishable by id.
                    // What CAN be said is when cancelling is right at
                    // all — during a drain, where the shutdown sweep may
                    // already have run and every uncommanded query is
                    // work the drain exists to stop. On a running driver
                    // a stray bootstrap completes and settles itself.
                    // AND NOTHING SPECIAL ABOUT ITS QUERIES. Rounds
                    // 8 and 9 of PR #61 argued over whether to cancel
                    // the bootstrap such an insertion may have started:
                    // cancelling every uncommanded query killed
                    // unrelated ones, and cancelling none left one
                    // running that the provider had never charged. Both
                    // were consequences of not being able to name it.
                    //
                    // Whatever this insertion started is charged like
                    // any other library query: announced by
                    // `reconcile_implicit` if a Kademlia event finds it
                    // still in the pool, and otherwise by its own
                    // completion in the Bootstrap arm, which names an
                    // unknown query before settling it. Either way the
                    // charge and its release are the same object, so
                    // there is no cancellation decision left here to get
                    // wrong.
                }
            }
            if let Some(evicted) = old_peer
                && state.routed.remove(&evicted)
                && let Ok(departed) = to_transport_identity(&evicted)
            {
                out.push(KademliaEvent::RoutingPeerRemoved { peer: departed });
            }
        }
        kad::Event::InboundRequest { request } => {
            // §12: dropped by `StoreInserts::FilterBoth` before this
            // code runs; COUNTED here, because "dropped and counted" is
            // the contract and a drop nobody can observe is not a
            // policy.
            if matches!(
                request,
                kad::InboundRequest::PutRecord { .. } | kad::InboundRequest::AddProvider { .. }
            ) {
                state.record_writes_dropped += 1;
            }
        }
        kad::Event::OutboundQueryProgressed {
            id, result, step, ..
        } => match result {
            kad::QueryResult::GetClosestPeers(outcome) => {
                if !state.queries.contains_key(&id) {
                    // A closest-peers walk this driver never commanded;
                    // nothing upstream is waiting on it.
                    return;
                }
                let (peers, timed_out) = match &outcome {
                    Ok(ok) => (&ok.peers, false),
                    Err(kad::GetClosestPeersError::Timeout { peers, .. }) => (peers, true),
                };
                accumulate(state, manager, id, peers, now_ms);
                if step.last {
                    let Some((class, handle)) = state.queries.remove(&id) else {
                        return;
                    };
                    let found = state.results.remove(&id).unwrap_or_default();
                    if found.is_empty() && timed_out {
                        out.push(KademliaEvent::QueryFailed {
                            handle,
                            class,
                            reason: QueryFailure::TimedOut,
                        });
                    } else if let Ok(candidates) =
                        interweave_kademlia_control_api::ObservedCandidates::new(found)
                    {
                        out.push(KademliaEvent::QueryResults {
                            handle,
                            candidates,
                            class,
                        });
                    }
                }
            }
            kad::QueryResult::Bootstrap(outcome) if step.last => {
                {
                    // AN UNKNOWN ID IS NOT AN ANONYMOUS ONE any more.
                    // The library's implicit bootstrap (F2) was reported
                    // as a bare bootstrap-class completion, and the
                    // provider then had to guess which charge it
                    // settled. `reconcile_implicit` has already given
                    // this query a handle and told the provider to
                    // charge it, so the completion names the same query
                    // and settles that charge and no other.
                    // A COMPLETION IS ALSO A BEGINNING, when it is the
                    // first this driver has heard of the query. Review
                    // finding on PR #64: the library starts its
                    // automatic bootstrap inside `Behaviour::poll` and
                    // emits NOTHING (`behaviour.rs`
                    // `poll_next_bootstrap`), 500ms behind
                    // `DEFAULT_AUTOMATIC_THROTTLE`, which this driver
                    // does not configure. Under `BucketInserts::Manual`
                    // discovered peers never enter the buckets, so
                    // `remaining` is empty, `step.last` is set on the
                    // first completion, and that completion is the ONLY
                    // Kademlia event the query produces in its life.
                    //
                    // `reconcile_implicit` cannot see such a query: by
                    // the time anything wakes it, the query is already
                    // gone from the pool. Returning here left the work
                    // uncharged — which the inference this PR replaced
                    // did charge, so it was a regression rather than an
                    // unimproved case.
                    //
                    // So an unknown completion is announced and settled
                    // in one pass. The charge and its release are still
                    // the same object, which is the property this design
                    // exists for; they simply arrive together.
                    //
                    // WHAT MAKES THAT SAFE IS THE `step.last` GUARD ON
                    // THIS ARM, and it is a property of the pinned
                    // dependency rather than of this code. In
                    // libp2p-kad 0.48 `query_finished` and
                    // `query_timeout` both set `step.last` only in the
                    // branch that does NOT call
                    // `continue_iter_closest`, so a last-step bootstrap
                    // completion is one that has left the pool. If a
                    // future version emitted a last step for a query it
                    // then re-entered, `reconcile_implicit` would meet
                    // that query alive and announce it a SECOND time
                    // under a second handle — one charge per bucket
                    // walked. The check that would catch it is
                    // `behaviour.query(&id).is_none()`; it is not
                    // written because today it is constant, and a guard
                    // that cannot fail teaches a reader the wrong thing
                    // about where the safety comes from.
                    let known = state.queries.remove(&id).or_else(|| {
                        state
                            .implicit
                            .remove(&id)
                            .map(|h| (QueryClass::Bootstrap, h))
                    });
                    // AND THIS DOOR CLOSES WHILE STOPPING TOO. The
                    // guard went on `reconcile_implicit` and not here,
                    // so a drained driver still announced — by the very
                    // path a re-entered bootstrap walks through. Safe by
                    // construction: while stopping, an unknown id is
                    // either one the sweep already settled or one that
                    // began after it and was never charged, so there is
                    // no permit to release and nothing to announce.
                    if state.stopping && known.is_none() {
                        return;
                    }
                    let (class, handle) = match known {
                        Some(pair) => pair,
                        None => {
                            state.next_implicit = state.next_implicit.wrapping_add(1);
                            let handle = QueryHandle::implicit(state.next_implicit);
                            out.push(KademliaEvent::QueryStarted {
                                handle,
                                class: QueryClass::Bootstrap,
                                origin: QueryOrigin::Implicit,
                            });
                            (QueryClass::Bootstrap, handle)
                        }
                    };
                    match outcome {
                        // Empty is always within the bound; a bootstrap
                        // completion carries no candidates of its own.
                        Ok(_) => {
                            if let Ok(candidates) =
                                interweave_kademlia_control_api::ObservedCandidates::new([])
                            {
                                out.push(KademliaEvent::QueryResults {
                                    handle,
                                    candidates,
                                    class,
                                });
                            }
                        }
                        Err(_) => out.push(KademliaEvent::QueryFailed {
                            handle,
                            class,
                            reason: QueryFailure::TimedOut,
                        }),
                    }
                }
            }
            // No command exists that could start any other query kind
            // (§12: the port has no record verbs).
            _ => {}
        },
        _ => {}
    }
}

/// Accumulate one progress step's peers as bounded candidates.
fn accumulate(
    state: &mut KademliaState,
    manager: &ConnectionManager,
    id: kad::QueryId,
    peers: &[kad::PeerInfo],
    now_ms: u64,
) {
    let cap = state.max_results_per_query;
    let found = state.results.entry(id).or_default();
    for info in peers {
        if found.len() >= cap {
            break;
        }
        let Ok(candidate) = to_transport_identity(&info.peer_id) else {
            continue;
        };
        if manager.is_local_peer(&candidate) {
            // §10: discard self.
            continue;
        }
        let addresses = candidate_addresses(info);
        found.push(interweave_discovery_api::CandidatePeer {
            peer_id: candidate,
            addresses,
            source: "kademlia".to_owned(),
            observed_at: now_ms,
            expires_at: None,
            protocol_observations: std::collections::BTreeSet::new(),
        });
    }
}

/// A query result's addresses, each held to the identity it claims —
/// and to the discovery contract's bounds WHILE being read, because the
/// list is remote-authored: collecting first and capping after would
/// let one response hold an oversized candidate in the accumulator and
/// emit a value downstream validation refuses.
fn candidate_addresses(info: &kad::PeerInfo) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for address in &info.addrs {
        if out.len() >= interweave_discovery_api::MAX_ADDRESSES {
            break;
        }
        let Some(bare) = suffix_checked(address, &info.peer_id) else {
            continue;
        };
        let bare = bare.to_string();
        if bare.is_empty() || bare.len() > interweave_discovery_api::MAX_ADDRESS_BYTES {
            continue;
        }
        out.insert(bare);
    }
    out
}

/// What one Identify observation means for the §7 pipeline.
enum Advertisement {
    /// Not eligible, or not this driver's business.
    Ignored,
    /// Advertises the exact server protocol; its addresses are stashed
    /// and the caller should run the admission pipeline.
    Serving(TransportIdentity),
    /// Withdrew the protocol. If it held a seat the caller must evict.
    Withdrawn,
}

/// The bookkeeping half of [`observe_identify`], with no Swarm in it.
///
/// Split out because it holds the two bounded tables and the trust gate
/// that guards them, and the only way to test THAT — rather than a copy
/// of it — is to be able to call it without a running Swarm.
fn remember_advertisement(
    state: &mut KademliaState,
    manager: &ConnectionManager,
    pid: PeerId,
    protocols: &[StreamProtocol],
    listen_addrs: &[Multiaddr],
    now_ms: u64,
) -> Advertisement {
    let advertises = protocols.iter().any(|p| p.as_ref() == state.protocol);
    let Ok(identity) = to_transport_identity(&pid) else {
        return Advertisement::Ignored;
    };
    // TRUST FIRST (§7's pipeline order puts authorization before the
    // Identify step). A peer that can never hold a seat is not
    // remembered at all: this map is bounded and `try_admit` consults
    // it, so entries that can never satisfy it were pure
    // denial-of-admission once the cap was reached.
    if !may_hold_a_seat(manager, &identity) {
        // Anything held from when it WAS eligible goes now, rather than
        // waiting for a revocation path to name it.
        forget_disconnected(state, &pid);
        return Advertisement::Ignored;
    }
    // REPLACED, not merged: a handler that unioned advertisements could
    // never observe a withdrawal, and SPIKE-003's F5 measured the
    // withdrawal happening on a real mode change.
    if !state.advertises.contains_key(&pid) {
        make_room(&mut state.advertises, MAX_OBSERVED_PEERS, |(_, at)| *at);
    }
    state.advertises.insert(pid, (advertises, now_ms));
    if !advertises {
        return Advertisement::Withdrawn;
    }
    // The peer's own listen addresses are candidate routes — this is
    // F3's substance: an INBOUND connection's Identify is as much a
    // routing observation as one this node dialled for. Peer-asserted,
    // so they go through the same stash the offers use, never straight
    // to the table.
    if !state.pending_offers.contains_key(&pid) {
        make_room(&mut state.pending_offers, MAX_PENDING_OFFERS, |(_, at)| *at);
    }
    let (stash, seen) = state.pending_offers.entry(pid).or_default();
    *seen = now_ms;
    for addr in listen_addrs {
        if stash.len() >= 64 {
            break;
        }
        if let Some(bare) = suffix_checked(addr, &pid) {
            stash.insert(bare);
        }
    }
    Advertisement::Serving(identity)
}

/// One authenticated Identify observation enters the §7 pipeline (F3).
#[allow(clippy::too_many_arguments)]
fn observe_identify(
    state: &mut KademliaState,
    behaviour: Option<&mut kad::Behaviour<MemoryStore>>,
    manager: &ConnectionManager,
    pid: PeerId,
    protocols: &[StreamProtocol],
    listen_addrs: &[Multiaddr],
    now_ms: u64,
    out: &mut Vec<KademliaEvent>,
) {
    match remember_advertisement(state, manager, pid, protocols, listen_addrs, now_ms) {
        Advertisement::Ignored => {}
        Advertisement::Withdrawn => {
            // Fresh evidence supersedes: a routed peer that stopped
            // advertising the exact server protocol leaves the table.
            if state.routed.remove(&pid) {
                if let Some(behaviour) = behaviour {
                    behaviour.remove_peer(&pid);
                }
                if let Ok(departed) = to_transport_identity(&pid) {
                    out.push(KademliaEvent::RoutingPeerRemoved { peer: departed });
                }
            }
        }
        Advertisement::Serving(identity) => {
            if let Some(behaviour) = behaviour {
                out.extend(try_admit(state, behaviour, manager, pid, &identity, now_ms));
            }
        }
    }
}

/// Trust left some peers: routing follows it, immediately (§11).
/// Trust left some peers: routing follows it, immediately (§11).
///
/// SCANNED FROM THE ROUTING TABLE, not from the revocation list.
/// Review finding on PR #61: `revoked` is computed against the set of
/// peers with LIVE CONNECTIONS, and a Kademlia routing entry outlives
/// the connection that produced it. So a peer that was routed and had
/// since disconnected was named by no revocation entry, kept its seat
/// in both this table and the behaviour's, and went on consuming
/// `max_routing_peers` with no authority behind it — the one direction
/// §11 exists to close.
///
/// Every routed peer is therefore re-classified against the policy the
/// manager has just published. That subsumes the revocation list rather
/// than complementing it: a revoked peer that is still routed fails the
/// same check, and a peer whose trust never changed passes it, so the
/// list is not needed to know who must go. `advertises` and
/// `pending_offers` are swept the same way, because a stash that can
/// never be admitted is the pin `make_room` exists to prevent.
pub(super) fn apply_revocations(
    state: &mut KademliaState,
    behaviour: Option<&mut kad::Behaviour<MemoryStore>>,
    manager: &ConnectionManager,
    out: &mut Vec<KademliaEvent>,
) {
    let mut behaviour = behaviour;
    let holders: Vec<PeerId> = state
        .routed
        .iter()
        .chain(state.advertises.keys())
        .chain(state.pending_offers.keys())
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    for pid in holders {
        let Ok(identity) = to_transport_identity(&pid) else {
            continue;
        };
        // A routing peer is held to DATA-PLANE trust (§13); a peer
        // demoted to infrastructure-only keeps its reachability role
        // and loses its routing seat.
        if may_hold_a_seat(manager, &identity) {
            continue;
        }
        forget_disconnected(state, &pid);
        if state.routed.remove(&pid) {
            if let Some(behaviour) = behaviour.as_deref_mut() {
                behaviour.remove_peer(&pid);
            }
            out.push(KademliaEvent::RoutingPeerRemoved { peer: identity });
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    use interweave_kademlia_control_api::LookupKey;

    #[test]
    fn the_namespace_matches_every_frozen_vector() {
        // Derived from the SPECIFICATION and checked against the frozen
        // fixture, so a derivation that merely agrees with itself — or
        // one that hashes to the full digest instead of the 16-byte
        // truncation — cannot pass.
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../fixtures/kademlia/kad-network-namespace-v1.json"
        ))
        .expect("fixture parses");
        let vectors = fixture["vectors"].as_array().expect("vectors");
        assert!(!vectors.is_empty(), "an empty fixture proves nothing");
        for vector in vectors {
            let id = vector["network_id"].as_str().expect("network_id");
            let hash = vector["network_hash"].as_str().expect("hash");
            let protocol = vector["protocol"].as_str().expect("protocol");
            assert_eq!(network_hash(id), hash, "{id}");
            assert_eq!(kad_protocol(id), protocol, "{id}");
        }
    }

    #[test]
    fn the_network_id_grammar_is_exact() {
        for legal in ["a", "example-private-network", "net-2.staging_7", "0x"] {
            assert!(network_id_is_legal(legal), "{legal}");
        }
        for illegal in [
            "",
            "A",
            "-leading",
            ".leading",
            "_leading",
            "has space",
            "ünïcode",
        ] {
            assert!(!network_id_is_legal(illegal), "{illegal:?}");
        }
        assert!(network_id_is_legal(&"n".repeat(64)));
        assert!(!network_id_is_legal(&"n".repeat(65)));
    }

    #[test]
    fn a_lookup_key_rebuilds_the_exact_peer() {
        // The other half of the encoding the provider writes: the key
        // is the Ed25519 public key, and the envelope is constant, so
        // reconstruction must be exact and reversible.
        let key: [u8; 32] = core::array::from_fn(|i| u8::try_from(i).expect("fits") ^ 0x5a);
        let peer = peer_from_lookup_key(key).expect("a legal identity multihash");
        let bytes = peer.to_bytes();
        assert_eq!(bytes.len(), 38);
        // LITERALS, not the module's own constant: a test that compares
        // against ED25519_ENVELOPE agrees with any mutation of it for
        // free — the self-referential trap CLAUDE.md's testing rule
        // names. These six bytes are the identity-multihash envelope as
        // the spec fixes it, written out independently.
        assert_eq!(bytes[..6], [0x00, 0x24, 0x08, 0x01, 0x12, 0x20]);
        assert_eq!(bytes[6..], key);
        // And the INDEPENDENT parser agrees: TransportIdentity decodes
        // the base58 and checks the multihash itself, so a bent
        // envelope fails here even if libp2p happens to tolerate it.
        let identity = TransportIdentity::parse(peer.to_base58())
            .expect("the rebuilt identity satisfies the neutral grammar");
        assert!(identity.as_str().starts_with("12D3KooW"));
    }

    #[test]
    fn settings_outside_the_bounds_are_refused() {
        let good = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 256,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        good.validate().expect("the canonical defaults validate");
        let nz = |n: usize| NonZeroUsize::new(n).expect("nonzero");
        let cases: Vec<(&str, KademliaSettings)> = vec![
            (
                "illegal network id",
                KademliaSettings {
                    network_id: "Not-Legal".to_owned(),
                    ..good.clone()
                },
            ),
            (
                "kbucket below the floor",
                KademliaSettings {
                    kbucket_size: nz(7),
                    ..good.clone()
                },
            ),
            (
                "kbucket above K",
                KademliaSettings {
                    kbucket_size: nz(21),
                    ..good.clone()
                },
            ),
            (
                "routing ceiling below the floor",
                KademliaSettings {
                    max_routing_peers: 19,
                    ..good.clone()
                },
            ),
            (
                "routing ceiling above the port bound",
                KademliaSettings {
                    max_routing_peers: 1_025,
                    ..good.clone()
                },
            ),
            (
                "parallelism amplification",
                KademliaSettings {
                    parallelism: nz(11),
                    ..good.clone()
                },
            ),
            (
                "concurrency amplification",
                KademliaSettings {
                    max_concurrent_queries: nz(9),
                    ..good.clone()
                },
            ),
            (
                "timeout below the floor",
                KademliaSettings {
                    query_timeout: Duration::from_secs(4),
                    ..good.clone()
                },
            ),
            (
                "timeout above the ceiling",
                KademliaSettings {
                    query_timeout: Duration::from_secs(121),
                    ..good.clone()
                },
            ),
            (
                "results above the port ceiling",
                KademliaSettings {
                    max_results_per_query: nz(MAX_RESULTS_PER_QUERY + 1),
                    ..good
                },
            ),
        ];
        for (what, bad) in cases {
            assert!(bad.validate().is_err(), "{what} must be refused");
        }
    }

    #[test]
    fn inbound_record_writes_are_counted_never_stored() {
        // §12: FilterBoth already refused the store; the count is the
        // contract's other half — a drop nobody can observe is not a
        // policy.
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 256,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let mut state = KademliaState::new(&settings);
        let manager = interweave_transport_runtime::ConnectionManager::new(
            interweave_transport_runtime::ConnectionPolicy::default(),
            8,
        );
        let mut out = Vec::new();
        let put = kad::Event::InboundRequest {
            request: kad::InboundRequest::PutRecord {
                source: PeerId::random(),
                connection: libp2p::swarm::ConnectionId::new_unchecked(1),
                record: None,
            },
        };
        handle_kad_event(&mut state, None, &manager, put, 0, &mut out);
        let add = kad::Event::InboundRequest {
            request: kad::InboundRequest::AddProvider { record: None },
        };
        handle_kad_event(&mut state, None, &manager, add, 0, &mut out);
        let read = kad::Event::InboundRequest {
            request: kad::InboundRequest::FindNode {
                num_closer_peers: 1,
            },
        };
        handle_kad_event(&mut state, None, &manager, read, 0, &mut out);
        assert_eq!(
            state.record_writes_dropped(),
            2,
            "both write kinds are counted; a routing read is not a write"
        );
        assert!(out.is_empty(), "counting is not a port event");
    }

    #[test]
    fn the_population_bound_is_not_an_address_freeze() {
        // §11: max_routing_peers binds the POPULATION before manual
        // insertion; a peer already routed may always refresh its
        // addresses. Driven through try_admit with a real behaviour and
        // a ceiling of one.
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 1,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let mut state = KademliaState::new(&settings);
        let local = PeerId::random();
        let mut behaviour = build_behaviour(&settings, local).expect("buildable");
        let mut manager = interweave_transport_runtime::ConnectionManager::new(
            interweave_transport_runtime::ConnectionPolicy::default(),
            8,
        );
        // Real Ed25519 identities: PeerId::random() mints a digest-form
        // id the neutral grammar refuses.
        let a = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let b = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let a_id = to_transport_identity(&a).expect("canonical");
        let b_id = to_transport_identity(&b).expect("canonical");
        let _ = manager.set_trust(
            interweave_transport_runtime::TrustSources::new(
                interweave_trust_api::PeerTrustPolicy::new([a_id.clone(), b_id.clone()])
                    .expect("small"),
                interweave_trust_api::InfrastructureSet::default(),
            ),
            &[],
        );
        for peer in [a, b] {
            state.advertises.insert(peer, (true, 0));
            state
                .pending_offers
                .entry(peer)
                .or_default()
                .0
                .insert("/ip4/127.0.0.1/tcp/1".parse().expect("valid"));
        }

        let _ = try_admit(&mut state, &mut behaviour, &manager, a, &a_id, 0);
        assert!(state.routed.contains(&a), "the first peer takes the seat");
        let _ = try_admit(&mut state, &mut behaviour, &manager, b, &b_id, 0);
        assert!(
            !state.routed.contains(&b),
            "the ceiling binds BEFORE insertion: no second seat exists"
        );
        assert!(
            state.pending_offers.contains_key(&b),
            "the refused offer stays stashed rather than being consumed"
        );

        // The routed peer's ADDRESS UPDATE still passes: a new offer for
        // it is consumed, not frozen out by the full table.
        state
            .pending_offers
            .entry(a)
            .or_default()
            .0
            .insert("/ip4/127.0.0.1/tcp/2".parse().expect("valid"));
        let _ = try_admit(&mut state, &mut behaviour, &manager, a, &a_id, 1);
        assert!(
            !state.pending_offers.contains_key(&a),
            "a population bound is not an address freeze (§11)"
        );
    }

    #[test]
    fn a_table_full_of_unusable_peers_still_admits_a_trusted_server() {
        // THE PIN. Both stashes were refuse-the-newcomer at the cap and
        // had no eviction outside trust revocation, so the first 256
        // offers and the first 1024 Identify advertisements held their
        // slots for the life of the process — and `try_admit` requires
        // an `advertises` entry, so a pinned table is a permanent
        // refusal to route anybody new. Nothing here is trusted, which
        // is the point: mDNS on a shared LAN offers a candidate for
        // every node it sees.
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 20,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let mut state = KademliaState::new(&settings);
        let local = PeerId::random();
        let mut behaviour = build_behaviour(&settings, local).expect("buildable");
        let mut manager = interweave_transport_runtime::ConnectionManager::new(
            interweave_transport_runtime::ConnectionPolicy::default(),
            8,
        );
        let server = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let server_id = to_transport_identity(&server).expect("canonical");
        // ONLY the server is trusted. Every stranger below is refused
        // by `try_admit` on trust, which is exactly why its slot was
        // never worth taking.
        let _ = manager.set_trust(
            interweave_transport_runtime::TrustSources::new(
                interweave_trust_api::PeerTrustPolicy::new([server_id.clone()]).expect("small"),
                interweave_trust_api::InfrastructureSet::default(),
            ),
            &[],
        );

        let addresses = interweave_kademlia_control_api::OfferedAddresses::parse_all([
            "/ip4/198.51.100.7/tcp/4001",
        ])
        .expect("bounded");
        for i in 0..(MAX_PENDING_OFFERS * 2) {
            let stranger = libp2p::identity::Keypair::generate_ed25519()
                .public()
                .to_peer_id();
            let stranger_id = to_transport_identity(&stranger).expect("canonical");
            let _ = handle_command(
                &mut state,
                &mut behaviour,
                &manager,
                KademliaCommand::OfferRoutingPeer {
                    addresses: addresses.clone(),
                    peer: stranger_id,
                },
                i as u64,
            );
            // The advertisement half of the pin, driven directly: a
            // stranger that completed Identify used to take a slot here
            // too, and nothing gave it back.
            let serving = StreamProtocol::try_from_owned(state.protocol.clone()).expect("legal");
            let _ =
                remember_advertisement(&mut state, &manager, stranger, &[serving], &[], i as u64);
        }
        assert!(
            state.pending_offers.len() <= MAX_PENDING_OFFERS,
            "the bound still holds"
        );
        assert!(
            state.advertises.len() <= MAX_OBSERVED_PEERS,
            "and so does this one"
        );

        // NOW the trusted server arrives, last.
        let _ = handle_command(
            &mut state,
            &mut behaviour,
            &manager,
            KademliaCommand::OfferRoutingPeer {
                addresses,
                peer: server_id.clone(),
            },
            10_000,
        );
        assert!(
            state.pending_offers.contains_key(&server),
            "a trusted peer's offer is stashed even after strangers filled the table"
        );
        let serving = StreamProtocol::try_from_owned(state.protocol.clone()).expect("legal");
        let _ = remember_advertisement(&mut state, &manager, server, &[serving], &[], 10_001);
        assert!(
            matches!(state.advertises.get(&server), Some((true, _))),
            "and its advertisement is remembered"
        );
        let _ = try_admit(
            &mut state,
            &mut behaviour,
            &manager,
            server,
            &server_id,
            10_002,
        );
        assert!(
            state.routed.contains(&server),
            "the seat is reachable: a table full of peers that can never hold one \
             must not be a permanent refusal to route anybody"
        );
    }

    #[test]
    fn a_peer_that_stops_advertising_the_server_protocol_loses_its_seat() {
        // Review finding on PR #61: "REPLACED, not merged" had no test.
        // `remember_advertisement` was never called twice for one peer
        // with differing protocol lists, so unioning the advertisement —
        // the exact regression the comment warns against, and the one
        // SPIKE-003 F5 measured on a real mode change — passed every
        // test in the tree, as did deleting the `Withdrawn` arm whole.
        //
        // §7's rule: "if a peer no longer advertises the exact server
        // protocol, stale positive evidence is removed/replaced." A peer
        // that switches to client mode and keeps its seat is a route
        // queries keep targeting that no longer serves.
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 20,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let mut state = KademliaState::new(&settings);
        let mut manager = interweave_transport_runtime::ConnectionManager::new(
            interweave_transport_runtime::ConnectionPolicy::default(),
            8,
        );
        let server = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let server_id = to_transport_identity(&server).expect("canonical");
        let _ = manager.set_trust(
            interweave_transport_runtime::TrustSources::new(
                interweave_trust_api::PeerTrustPolicy::new([server_id]).expect("small"),
                interweave_trust_api::InfrastructureSet::default(),
            ),
            &[],
        );
        let serving = StreamProtocol::try_from_owned(state.protocol.clone()).expect("legal");

        // It advertises, and holds a seat.
        assert!(matches!(
            remember_advertisement(
                &mut state,
                &manager,
                server,
                std::slice::from_ref(&serving),
                &[],
                0
            ),
            Advertisement::Serving(_)
        ));
        assert_eq!(state.advertises.get(&server).map(|(a, _)| *a), Some(true));
        state.routed.insert(server);

        // THE MODE CHANGE: a fresh Identify, same peer, without the
        // server protocol. Another protocol is present, so this is a
        // real advertisement rather than an empty one.
        //
        // DRIVEN THROUGH `observe_identify`, not `remember_advertisement`
        // alone. Review finding on the first version of this test:
        // asserting the returned value and the stored flag covers
        // replacement-versus-merge and nothing else, so deleting the
        // `Withdrawn` arm's seat removal — the consequence that actually
        // matters — still left it green.
        let other =
            StreamProtocol::try_from_owned("/interweave/direct/2.0.0".to_owned()).expect("legal");
        let mut behaviour = build_behaviour(&settings, PeerId::random()).expect("buildable");
        let mut out = Vec::new();
        observe_identify(
            &mut state,
            Some(&mut behaviour),
            &manager,
            server,
            &[other],
            &[],
            1_000,
            &mut out,
        );
        assert_eq!(
            state.advertises.get(&server).map(|(a, _)| *a),
            Some(false),
            "the stored advertisement is REPLACED, not merged"
        );
        assert!(
            !state.routed.contains(&server),
            "and the seat goes with it: a peer that stopped serving is a route \
             queries would otherwise keep targeting"
        );
        assert!(
            matches!(out.as_slice(), [KademliaEvent::RoutingPeerRemoved { .. }]),
            "the withdrawal is reported, not merely performed"
        );
    }

    #[test]
    fn untrusted_churn_cannot_displace_a_trusted_offer_still_awaiting_evidence() {
        // Review finding on PR #61: the pre-stash trust gate had no test
        // that isolated it. `a_table_full_of_unusable_peers_still_admits
        // _a_trusted_server` passes with the gate deleted, because
        // `make_room` bounds the map either way and the trusted arrival
        // there is the NEWEST — it would displace something regardless.
        //
        // The scenario the gate actually exists for is the opposite
        // order: a trusted peer's offer is stashed FIRST and waits for
        // its Identify evidence, while untrusted offers keep arriving
        // with newer timestamps. Without the gate they are stashed, and
        // stalest-displacement then evicts the one entry that could
        // have become a routing seat. mDNS on a shared LAN produces
        // exactly this shape.
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 20,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let mut state = KademliaState::new(&settings);
        let local = PeerId::random();
        let mut behaviour = build_behaviour(&settings, local).expect("buildable");
        let mut manager = interweave_transport_runtime::ConnectionManager::new(
            interweave_transport_runtime::ConnectionPolicy::default(),
            8,
        );
        let waiting = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let waiting_id = to_transport_identity(&waiting).expect("canonical");
        let _ = manager.set_trust(
            interweave_transport_runtime::TrustSources::new(
                interweave_trust_api::PeerTrustPolicy::new([waiting_id.clone()]).expect("small"),
                interweave_trust_api::InfrastructureSet::default(),
            ),
            &[],
        );

        let addresses = interweave_kademlia_control_api::OfferedAddresses::parse_all([
            "/ip4/198.51.100.7/tcp/4001",
        ])
        .expect("bounded");
        // The trusted offer lands FIRST and stays pending: no Identify
        // evidence, so `try_admit` leaves it stashed.
        let _ = handle_command(
            &mut state,
            &mut behaviour,
            &manager,
            KademliaCommand::OfferRoutingPeer {
                addresses: addresses.clone(),
                peer: waiting_id,
            },
            0,
        );
        assert!(
            state.pending_offers.contains_key(&waiting),
            "the control: it is stashed and waiting"
        );

        // Then untrusted churn, every one of them NEWER.
        for i in 1..=(MAX_PENDING_OFFERS as u64 * 2) {
            let stranger = libp2p::identity::Keypair::generate_ed25519()
                .public()
                .to_peer_id();
            let _ = handle_command(
                &mut state,
                &mut behaviour,
                &manager,
                KademliaCommand::OfferRoutingPeer {
                    addresses: addresses.clone(),
                    peer: to_transport_identity(&stranger).expect("canonical"),
                },
                i,
            );
        }

        assert!(
            state.pending_offers.contains_key(&waiting),
            "a peer that can never hold a seat must not be able to evict one that can"
        );
        assert_eq!(
            state.pending_offers.len(),
            1,
            "and nothing untrusted was stashed at all"
        );
    }

    #[test]
    fn a_stash_full_of_trusted_peers_displaces_the_stalest() {
        // The trust gate is not the whole answer: `PeerTrustPolicy`
        // admits up to 4096 peers, which is sixteen times the offer
        // stash and four times the advertisement map. So a profile with
        // a large trust set fills both LEGITIMATELY, and under
        // refuse-the-newcomer the first 256 offers held their slots for
        // the life of the process — a bound that had become a wall.
        // Every peer here is trusted, so only displacement can pass it.
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 20,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let mut state = KademliaState::new(&settings);
        let local = PeerId::random();
        let mut behaviour = build_behaviour(&settings, local).expect("buildable");
        let mut manager = interweave_transport_runtime::ConnectionManager::new(
            interweave_transport_runtime::ConnectionPolicy::default(),
            8,
        );

        let crowd: Vec<PeerId> = (0..=MAX_PENDING_OFFERS)
            .map(|_| {
                libp2p::identity::Keypair::generate_ed25519()
                    .public()
                    .to_peer_id()
            })
            .collect();
        let ids: Vec<TransportIdentity> = crowd
            .iter()
            .map(|p| to_transport_identity(p).expect("canonical"))
            .collect();
        let _ = manager.set_trust(
            interweave_transport_runtime::TrustSources::new(
                interweave_trust_api::PeerTrustPolicy::new(ids.iter().cloned())
                    .expect("well under the 4096 cap"),
                interweave_trust_api::InfrastructureSet::default(),
            ),
            &[],
        );

        let addresses = interweave_kademlia_control_api::OfferedAddresses::parse_all([
            "/ip4/198.51.100.7/tcp/4001",
        ])
        .expect("bounded");
        // Ascending timestamps, so "stalest" is unambiguous.
        for (i, id) in ids.iter().enumerate() {
            let _ = handle_command(
                &mut state,
                &mut behaviour,
                &manager,
                KademliaCommand::OfferRoutingPeer {
                    addresses: addresses.clone(),
                    peer: id.clone(),
                },
                i as u64,
            );
        }

        assert!(
            state.pending_offers.len() <= MAX_PENDING_OFFERS,
            "the bound holds: {} slots",
            state.pending_offers.len()
        );
        let newest = crowd.last().expect("non-empty");
        assert!(
            state.pending_offers.contains_key(newest),
            "the LAST trusted offer is stashed — refusing the newcomer at the cap \
             would have dropped exactly this one"
        );
        assert!(
            !state.pending_offers.contains_key(&crowd[0]),
            "and the stalest is what made room, rather than the newest being refused"
        );
    }

    #[test]
    fn a_refused_insert_does_not_leave_a_phantom_seat() {
        // The seat is claimed optimistically so two admissions in one
        // batch cannot both read a stale count. The return of
        // `add_address` was then DISCARDED — and `RoutingUpdated` only
        // ever adds (`is_new_peer`) or removes an evicted `old_peer`,
        // so a peer the table refused was never named by any later
        // event and its phantom was permanent. `routed` then ratcheted
        // toward `max_routing_peers`, after which nothing new could be
        // admitted at all.
        //
        // The local peer is the reliable refusal: `add_address` looks
        // up its own key, finds no bucket, and answers `Failed`. The
        // manager here never binds a local identity, so classification
        // does not intercept before the behaviour can refuse.
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 20,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let local = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let local_id = to_transport_identity(&local).expect("canonical");
        let mut state = KademliaState::new(&settings);
        let mut behaviour = build_behaviour(&settings, local).expect("buildable");
        let mut manager = interweave_transport_runtime::ConnectionManager::new(
            interweave_transport_runtime::ConnectionPolicy::default(),
            8,
        );
        let _ = manager.set_trust(
            interweave_transport_runtime::TrustSources::new(
                interweave_trust_api::PeerTrustPolicy::new([local_id.clone()]).expect("small"),
                interweave_trust_api::InfrastructureSet::default(),
            ),
            &[],
        );
        state.advertises.insert(local, (true, 0));
        state
            .pending_offers
            .entry(local)
            .or_default()
            .0
            .insert("/ip4/127.0.0.1/tcp/1".parse().expect("valid"));

        let _ = try_admit(&mut state, &mut behaviour, &manager, local, &local_id, 0);
        assert!(
            !state.routed.contains(&local),
            "a seat the table refused is given back, not held forever"
        );
        assert!(
            state.routed.is_empty(),
            "so the population count still describes the table"
        );
    }

    #[test]
    fn revocation_reaches_a_routed_peer_that_is_no_longer_connected() {
        // Review finding on PR #61. The revocation list is computed
        // against peers with LIVE CONNECTIONS, and a Kademlia routing
        // entry outlives the connection that produced it. So a peer
        // that was routed and had since disconnected was named by no
        // revocation entry, kept its seat in this table and the
        // behaviour's, and went on consuming `max_routing_peers` with
        // no authority behind it.
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 20,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let mut state = KademliaState::new(&settings);
        let local = PeerId::random();
        let mut behaviour = build_behaviour(&settings, local).expect("buildable");
        let mut manager = interweave_transport_runtime::ConnectionManager::new(
            interweave_transport_runtime::ConnectionPolicy::default(),
            8,
        );
        let departed = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let departed_id = to_transport_identity(&departed).expect("canonical");

        // It held a seat while trusted, and it is NOT connected now.
        // `advertises` is deliberately EMPTY: the disconnect reclaimed
        // its advertisement, and the routing seat is what outlived the
        // connection. So `routed` is the only place this peer appears,
        // which is exactly why scanning the revocation list — or any
        // other table — could not reach it.
        state.routed.insert(departed);
        // Trust is set to a policy that does NOT contain it, and the
        // revocation list this call used to take is empty, because
        // nothing about it is live.
        let other = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let _ = manager.set_trust(
            interweave_transport_runtime::TrustSources::new(
                interweave_trust_api::PeerTrustPolicy::new([
                    to_transport_identity(&other).expect("canonical")
                ])
                .expect("small"),
                interweave_trust_api::InfrastructureSet::default(),
            ),
            &[],
        );
        assert_ne!(
            manager.classify(&departed_id),
            ConnectionClass::DataPlaneTrusted,
            "the control: it really is unauthorized now"
        );

        let mut out = Vec::new();
        apply_revocations(&mut state, Some(&mut behaviour), &manager, &mut out);
        assert!(
            !state.routed.contains(&departed),
            "an unauthorized peer does not keep a routing seat because it happened \
             to be offline when trust changed"
        );
        assert_eq!(out.len(), 1, "the withdrawal is reported once");

        // A STILL-TRUSTED routed peer is untouched, or this test would
        // pass for a build that revokes everything.
        let kept = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let kept_id = to_transport_identity(&kept).expect("canonical");
        let _ = manager.set_trust(
            interweave_transport_runtime::TrustSources::new(
                interweave_trust_api::PeerTrustPolicy::new([kept_id]).expect("small"),
                interweave_trust_api::InfrastructureSet::default(),
            ),
            &[],
        );
        state.routed.insert(kept);
        let mut second = Vec::new();
        apply_revocations(&mut state, Some(&mut behaviour), &manager, &mut second);
        assert!(state.routed.contains(&kept), "trust intact, seat intact");
        assert!(second.is_empty(), "and nothing is reported");
    }

    /// A `GatedSwarm` with Kademlia enabled, for the one test that must
    /// drive `handle_kademlia` rather than its parts.
    fn gated_swarm_with_kad(settings: &KademliaSettings) -> crate::gated_swarm::GatedSwarm {
        let keypair = libp2p::identity::Keypair::generate_ed25519();
        let local_pid = PeerId::from_public_key(&keypair.public());
        let manager = interweave_transport_runtime::ConnectionManager::new(
            interweave_transport_runtime::ConnectionPolicy::default(),
            8,
        );
        let outbound = crate::outbound_gate::OutboundAdmission::new(
            manager.handle(),
            crate::outbound_gate::InFlightTickets::default(),
            tokio::time::Instant::now(),
        );
        let kad = libp2p::swarm::behaviour::toggle::Toggle::from(Some(
            build_behaviour(settings, local_pid).expect("buildable"),
        ));
        let swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                libp2p::tcp::Config::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )
            .expect("tcp")
            .with_behaviour(|key| {
                crate::behaviour::SubstrateBehaviour::new(
                    key,
                    interweave_transport_runtime::preauth::PreAuthLimits::default(),
                    outbound,
                    kad,
                )
                .map_err(Box::<dyn std::error::Error + Send + Sync>::from)
            })
            .expect("behaviour")
            .build();
        crate::gated_swarm::GatedSwarm::new(swarm)
    }

    #[tokio::test]
    async fn a_non_kademlia_event_still_announces_a_silent_query() {
        // Review finding on PR #64. The library starts its automatic
        // bootstrap inside `Behaviour::poll` and emits no Kademlia
        // event for it, so under `BucketInserts::Manual` its first and
        // only Kademlia event is the final completion. Reconciling only
        // on Kademlia events therefore never met such a query while it
        // ran, and the provider held no charge for the whole time it
        // was dialling — while commanded queries could spend the entire
        // budget beside it.
        //
        // The query dials, and a dial produces non-Kademlia events. One
        // of those must be enough to announce it.
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 20,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let mut swarm = gated_swarm_with_kad(&settings);
        let mut state = KademliaState::new(&settings);
        let manager = interweave_transport_runtime::ConnectionManager::new(
            interweave_transport_runtime::ConnectionPolicy::default(),
            8,
        );

        // A query nobody commanded, live in the pool — what a silent
        // automatic bootstrap leaves behind.
        let _ = swarm
            .kademlia_mut()
            .expect("kad enabled")
            .get_closest_peers(PeerId::random());

        let mut out = Vec::new();
        let handled = handle_kademlia(
            Libp2pSwarmEvent::Dialing {
                peer_id: None,
                connection_id: libp2p::swarm::ConnectionId::new_unchecked(0),
            },
            &mut swarm,
            &mut state,
            &manager,
            0,
            &mut out,
        );
        assert!(
            matches!(handled, KadHandled::Passed(_)),
            "a dial event is not the driver\u{2019}s to consume"
        );
        assert!(
            out.iter().any(|e| matches!(
                e,
                KademliaEvent::QueryStarted {
                    origin: interweave_kademlia_control_api::QueryOrigin::Implicit,
                    ..
                }
            )),
            "the query is announced on a non-Kademlia event, while it is still running"
        );
        assert_eq!(state.implicit.len(), 1, "and tracked exactly once");
    }

    #[test]
    fn a_shutdown_pairs_every_announcement_with_its_settlement() {
        // Review finding on PR #64. The sweep announced every unmet
        // query, then settled every query, in two passes — while
        // `settleable_queries` counted one event per query. So with two
        // unmet queries and a full outbox the surplus was dropped, and
        // because announcements went first what fell off the end were
        // RELEASES for charges the provider had just taken: a permit
        // held for the life of the provider, which is the leak this
        // whole change removes.
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 20,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let mut state = KademliaState::new(&settings);
        let mut behaviour = build_behaviour(&settings, PeerId::random()).expect("buildable");

        // THREE queries the driver has never met — more than the one
        // the old arithmetic happened to survive.
        for _ in 0..3 {
            let _ = behaviour.get_closest_peers(PeerId::random());
        }
        assert_eq!(
            state.settleable_queries(Some(&behaviour)),
            6,
            "an unmet query costs two events, and the slack must say so"
        );

        let manager = interweave_transport_runtime::ConnectionManager::new(
            interweave_transport_runtime::ConnectionPolicy::default(),
            8,
        );
        let out = handle_command(
            &mut state,
            &mut behaviour,
            &manager,
            KademliaCommand::Shutdown,
            0,
        );

        // Every announcement has its settlement, and they are adjacent
        // — so a truncated flush can lose a whole pair but never split
        // one.
        let mut announced = 0_usize;
        for pair in out.windows(2) {
            if let KademliaEvent::QueryStarted { handle, .. } = &pair[0] {
                announced += 1;
                assert!(
                    matches!(
                        &pair[1],
                        KademliaEvent::QueryFailed {
                            handle: settled,
                            reason: QueryFailure::ShuttingDown,
                            ..
                        } if settled == handle
                    ),
                    "an announcement is followed immediately by its own settlement"
                );
            }
        }
        assert_eq!(announced, 3, "all three unmet queries were announced");
        assert!(
            out.len() <= state.max_routing_peers.saturating_mul(2) + 6,
            "and the sweep emits no more than the slack counted"
        );

        // The sweep tracks none of them: `MAX_IMPLICIT_QUERIES` bounds
        // what the driver holds at once, and a query announced and
        // settled in one pass is never held.
        assert!(
            state.implicit.is_empty(),
            "a settled query is not left in the tracked map"
        );
    }

    #[test]
    fn a_same_bucket_swap_does_not_shrink_a_full_table() {
        // Review finding on PR #64. The ceiling was evaluated against
        // the population before the event, but the `old_peer` eviction
        // belongs to the same event — so a swap on a full table refused
        // the newcomer for a seat it was itself about to free, then
        // freed it. One seat lost per swap, and the real table lost it
        // too: `remove_peer` drops the peer the behaviour had already
        // inserted.
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 4,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let mut state = KademliaState::new(&settings);
        let mut behaviour = build_behaviour(&settings, PeerId::random()).expect("buildable");
        let mut manager = interweave_transport_runtime::ConnectionManager::new(
            interweave_transport_runtime::ConnectionPolicy::default(),
            8,
        );

        // A full table, and one of its members is the peer being
        // evicted by this very update.
        let seats: Vec<PeerId> = (0..settings.max_routing_peers)
            .map(|_| {
                libp2p::identity::Keypair::generate_ed25519()
                    .public()
                    .to_peer_id()
            })
            .collect();
        for seat in &seats {
            state.routed.insert(*seat);
        }
        let evicted = seats[0];

        let newcomer = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let newcomer_id = to_transport_identity(&newcomer).expect("canonical");
        let _ = manager.set_trust(
            interweave_transport_runtime::TrustSources::new(
                interweave_trust_api::PeerTrustPolicy::new([newcomer_id]).expect("small"),
                interweave_trust_api::InfrastructureSet::default(),
            ),
            &[],
        );
        state.advertises.insert(newcomer, (true, 0));

        let mut out = Vec::new();
        handle_kad_event(
            &mut state,
            Some(&mut behaviour),
            &manager,
            kad::Event::RoutingUpdated {
                peer: newcomer,
                is_new_peer: true,
                addresses: kad::Addresses::new("/ip4/127.0.0.1/tcp/1".parse().expect("valid")),
                bucket_range: (
                    kad::KBucketDistance::default(),
                    kad::KBucketDistance::default(),
                ),
                old_peer: Some(evicted),
            },
            0,
            &mut out,
        );

        assert!(
            state.routed.contains(&newcomer),
            "the replacement takes the seat the eviction frees"
        );
        assert!(!state.routed.contains(&evicted), "and the evicted leaves");
        assert_eq!(
            state.routed.len(),
            settings.max_routing_peers,
            "a swap holds the population exactly at the cap, it does not shrink it"
        );

        // A swap is still not a way past the cap: an INELIGIBLE
        // replacement is refused and the eviction still happens, so
        // this cannot pass for a predicate that simply stopped
        // counting.
        let stranger = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        state.advertises.insert(stranger, (true, 0));
        let mut again = Vec::new();
        handle_kad_event(
            &mut state,
            Some(&mut behaviour),
            &manager,
            kad::Event::RoutingUpdated {
                peer: stranger,
                is_new_peer: true,
                addresses: kad::Addresses::new("/ip4/127.0.0.1/tcp/2".parse().expect("valid")),
                bucket_range: (
                    kad::KBucketDistance::default(),
                    kad::KBucketDistance::default(),
                ),
                old_peer: Some(seats[1]),
            },
            0,
            &mut again,
        );
        assert!(
            !state.routed.contains(&stranger),
            "an untrusted replacement is refused however many seats open"
        );
        assert_eq!(
            state.routed.len(),
            settings.max_routing_peers - 1,
            "and its eviction still lands, so the seat is simply given up"
        );
    }

    #[test]
    fn a_queued_update_cannot_grow_the_table_past_its_ceiling() {
        // Issue #63 item 2. The revalidation rechecked trust, the
        // advertisement and the lifecycle — and not the POPULATION. So
        // a pending peer that lost its optimistic seat on disconnect,
        // while another peer filled the table, was declared eligible on
        // reconnect and the insertion grew `routed` past
        // `max_routing_peers`. A ceiling is not a conjunct you may omit
        // from a revalidation that exists because the state moved.
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 20,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let mut state = KademliaState::new(&settings);
        // The table is exactly full of peers that are not the subject.
        // Real Ed25519 identities: `PeerId::random()` is digest-form,
        // which the neutral grammar refuses.
        for _ in 0..settings.max_routing_peers {
            state.routed.insert(
                libp2p::identity::Keypair::generate_ed25519()
                    .public()
                    .to_peer_id(),
            );
        }
        let mut behaviour = build_behaviour(&settings, PeerId::random()).expect("buildable");
        let mut manager = interweave_transport_runtime::ConnectionManager::new(
            interweave_transport_runtime::ConnectionPolicy::default(),
            8,
        );
        let late = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let late_id = to_transport_identity(&late).expect("canonical");
        let _ = manager.set_trust(
            interweave_transport_runtime::TrustSources::new(
                interweave_trust_api::PeerTrustPolicy::new([late_id]).expect("small"),
                interweave_trust_api::InfrastructureSet::default(),
            ),
            &[],
        );
        // Trusted and advertising: every OTHER conjunct holds, so only
        // the ceiling can refuse it.
        state.advertises.insert(late, (true, 0));

        let mut out = Vec::new();
        handle_kad_event(
            &mut state,
            Some(&mut behaviour),
            &manager,
            kad::Event::RoutingUpdated {
                peer: late,
                is_new_peer: true,
                addresses: kad::Addresses::new("/ip4/127.0.0.1/tcp/1".parse().expect("valid")),
                bucket_range: (
                    kad::KBucketDistance::default(),
                    kad::KBucketDistance::default(),
                ),
                old_peer: None,
            },
            0,
            &mut out,
        );
        assert_eq!(
            state.routed.len(),
            settings.max_routing_peers,
            "the population bound holds against a queued update too"
        );
        assert!(!state.routed.contains(&late));

        // A PEER ALREADY HOLDING ITS SEAT still passes: §11's bound is
        // a population bound, not an address freeze, so this test
        // cannot pass for a predicate that refuses everyone at the cap.
        let held = *state.routed.iter().next().expect("non-empty");
        let held_id = to_transport_identity(&held).expect("canonical");
        let _ = manager.set_trust(
            interweave_transport_runtime::TrustSources::new(
                interweave_trust_api::PeerTrustPolicy::new([held_id]).expect("small"),
                interweave_trust_api::InfrastructureSet::default(),
            ),
            &[],
        );
        state.advertises.insert(held, (true, 0));
        let mut again = Vec::new();
        handle_kad_event(
            &mut state,
            Some(&mut behaviour),
            &manager,
            kad::Event::RoutingUpdated {
                peer: held,
                is_new_peer: true,
                addresses: kad::Addresses::new("/ip4/127.0.0.1/tcp/2".parse().expect("valid")),
                bucket_range: (
                    kad::KBucketDistance::default(),
                    kad::KBucketDistance::default(),
                ),
                old_peer: None,
            },
            0,
            &mut again,
        );
        assert!(
            state.routed.contains(&held),
            "a peer already routed is refreshing, not taking a new seat"
        );
    }

    #[test]
    fn a_stopping_driver_announces_nothing_by_either_door() {
        // The `stopping` guard went on `reconcile_implicit` and not on
        // the completion fallback — and the fallback is the door a
        // re-entered bootstrap walks through. A drained driver
        // therefore still announced: a fresh handle, a `QueryStarted`,
        // and a `charge_unscheduled` that bypasses both ceilings and
        // spends rate `finish` does not refund, plus a
        // `last_query_success_ms` reporting a healthy query after the
        // drain.
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 20,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let mut state = KademliaState::new(&settings);
        let mut behaviour = build_behaviour(&settings, PeerId::random()).expect("buildable");
        let manager = interweave_transport_runtime::ConnectionManager::new(
            interweave_transport_runtime::ConnectionPolicy::default(),
            8,
        );
        let unknown = behaviour.get_closest_peers(PeerId::random());

        let completion = |id| kad::Event::OutboundQueryProgressed {
            id,
            result: kad::QueryResult::Bootstrap(Ok(kad::BootstrapOk {
                peer: PeerId::random(),
                num_remaining: 0,
            })),
            stats: kad::QueryStats::empty(),
            step: kad::ProgressStep {
                count: NonZeroUsize::new(1).expect("nonzero"),
                last: true,
            },
        };

        // RUNNING: the fallback announces, which is the fix that closed
        // the uncharged-query hole — so this test cannot pass for a
        // driver that never announces.
        let mut running = Vec::new();
        handle_kad_event(
            &mut state,
            Some(&mut behaviour),
            &manager,
            completion(unknown),
            0,
            &mut running,
        );
        assert!(
            running
                .iter()
                .any(|e| matches!(e, KademliaEvent::QueryStarted { .. })),
            "the control: a running driver charges the work it discovers"
        );

        // STOPPING: the same event announces nothing at all.
        state.stopping = true;
        let another = behaviour.get_closest_peers(PeerId::random());
        let mut drained = Vec::new();
        handle_kad_event(
            &mut state,
            Some(&mut behaviour),
            &manager,
            completion(another),
            0,
            &mut drained,
        );
        assert!(
            drained.is_empty(),
            "a drained driver announces by neither door: {drained:?}"
        );
    }

    #[test]
    fn a_draining_driver_issues_no_dial_from_a_re_entered_bootstrap() {
        // THE OBSERVABLE IS THE HARM, not the bookkeeping. Two earlier
        // versions of this test asserted through `iter_queries`, which
        // FILTERS finished queries — so once `finish_all_while_stopping`
        // had marked them, the break condition was evaluated on a set
        // the previous statement had just emptied. It exited on
        // iteration one for any table, working drain or not. A dial is
        // what the drain exists to prevent, so a dial is what is
        // counted, and the production helpers are what drive it.
        //
        // The layout IS constructible; a previous comment here said
        // otherwise and was wrong. `query_finished` builds `remaining`
        // from every bucket farther than the first non-empty one, so a
        // peer in bucket k leaves 255-k to walk — empty only if it lands
        // in the last bucket, which is a coin flip rather than a rule.
        // Choosing a near peer makes the re-entry deterministic.
        use futures::task::noop_waker;
        use libp2p::kad::KBucketKey;
        use libp2p::swarm::{NetworkBehaviour, ToSwarm};
        use std::task::{Context, Poll};

        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 20,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let local = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let local_key = KBucketKey::from(local);
        let peer = std::iter::repeat_with(|| {
            libp2p::identity::Keypair::generate_ed25519()
                .public()
                .to_peer_id()
        })
        .find(|p| {
            local_key
                .distance(&KBucketKey::from(*p))
                .ilog2()
                .is_some_and(|i| i <= 250)
        })
        .expect("a near peer within a few dozen tries");

        let mut state = KademliaState::new(&settings);
        let mut behaviour = build_behaviour(&settings, local).expect("buildable");
        let manager = interweave_transport_runtime::ConnectionManager::new(
            interweave_transport_runtime::ConnectionPolicy::default(),
            8,
        );
        let _ = behaviour.add_address(&peer, "/ip4/127.0.0.1/tcp/1".parse().expect("valid"));
        let id = behaviour.bootstrap().expect("a routing peer exists");
        state
            .queries
            .insert(id, (QueryClass::Bootstrap, QueryHandle::commanded(1)));

        let _ = handle_command(
            &mut state,
            &mut behaviour,
            &manager,
            KademliaCommand::Shutdown,
            0,
        );
        assert!(state.stopping, "the control: the driver is draining");

        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let mut dials = 0_usize;
        let mut out = Vec::new();
        for _ in 0..512 {
            match NetworkBehaviour::poll(&mut behaviour, &mut cx) {
                Poll::Ready(ToSwarm::Dial { .. }) => dials += 1,
                Poll::Ready(ToSwarm::GenerateEvent(event)) => {
                    handle_kad_event(&mut state, None, &manager, event, 0, &mut out);
                    // Exactly what the event path does while draining.
                    finish_all_while_stopping(&mut behaviour);
                    reconcile_implicit(&mut state, &behaviour, &mut out);
                }
                Poll::Ready(_) => {}
                Poll::Pending => break,
            }
        }
        assert_eq!(
            dials, 0,
            "a drained driver dialled: the bootstrap re-entered and kept walking buckets"
        );
    }

    #[test]
    fn a_query_whose_only_event_is_its_completion_is_still_charged() {
        // Review finding on PR #64 (P1). The library starts its
        // automatic bootstrap inside `Behaviour::poll` and emits
        // nothing, behind a throttle this driver does not configure;
        // under `BucketInserts::Manual` its `remaining` set is empty, so
        // `step.last` is set on the first completion and that
        // completion is the ONLY Kademlia event the query ever
        // produces. `reconcile_implicit` cannot see such a query — by
        // the time anything wakes it, the query has left the pool — and
        // returning here left the work uncharged, which the inference
        // this design replaced DID charge.
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 20,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let mut state = KademliaState::new(&settings);
        let mut behaviour = build_behaviour(&settings, PeerId::random()).expect("buildable");
        let manager = interweave_transport_runtime::ConnectionManager::new(
            interweave_transport_runtime::ConnectionPolicy::default(),
            8,
        );
        // A completion for a query this driver never saw begin: no
        // entry in either map, which is the shape of the F2 bootstrap.
        let unseen = behaviour.get_closest_peers(PeerId::random());
        state.queries.remove(&unseen);
        state.implicit.remove(&unseen);

        let mut out = Vec::new();
        handle_kad_event(
            &mut state,
            Some(&mut behaviour),
            &manager,
            kad::Event::OutboundQueryProgressed {
                id: unseen,
                result: kad::QueryResult::Bootstrap(Ok(kad::BootstrapOk {
                    peer: PeerId::random(),
                    num_remaining: 0,
                })),
                stats: kad::QueryStats::empty(),
                step: kad::ProgressStep {
                    count: NonZeroUsize::new(1).expect("nonzero"),
                    last: true,
                },
            },
            0,
            &mut out,
        );

        let started: Vec<QueryHandle> = out
            .iter()
            .filter_map(|e| match e {
                KademliaEvent::QueryStarted { handle, origin, .. } => {
                    assert_eq!(*origin, QueryOrigin::Implicit);
                    Some(*handle)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            started.len(),
            1,
            "a query the driver never saw begin is announced by its completion, \
             or the work it did goes uncharged: {out:?}"
        );
        let settled: Vec<QueryHandle> = out
            .iter()
            .filter_map(|e| match e {
                KademliaEvent::QueryResults { handle, .. }
                | KademliaEvent::QueryFailed { handle, .. } => Some(*handle),
                KademliaEvent::QueryStarted { .. } => None,
                _ => None,
            })
            .collect();
        assert_eq!(
            settled, started,
            "and the charge and its release name the SAME query — arriving together \
             is not the same as being unrelated"
        );
    }

    #[test]
    fn a_stopping_driver_announces_no_query_and_the_population_is_bounded() {
        // Two review findings on PR #64, one guard each.
        //
        // `QueryMut::finish` does not cancel a bootstrap: the pool calls
        // `continue_iter_closest`, which re-inserts the SAME id for the
        // next bucket, and the re-entered query was then announced again
        // under a fresh handle — a `QueryStarted` for a query already
        // reported as shut down.
        //
        // And the claimed bound was not this repository's. Implicit
        // queries never enter `queries`, so `max_concurrent_queries`
        // never saw them; what limited them was a pinned dependency's
        // private state machine.
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 20,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let mut state = KademliaState::new(&settings);
        let mut behaviour = build_behaviour(&settings, PeerId::random()).expect("buildable");

        // STOPPING: nothing is announced, however many are live.
        let _ = behaviour.get_closest_peers(PeerId::random());
        state.stopping = true;
        let mut out = Vec::new();
        reconcile_implicit(&mut state, &behaviour, &mut out);
        assert!(
            out.is_empty(),
            "a drained driver does not announce the query its own `finish` re-entered"
        );
        assert!(state.implicit.is_empty());

        // RUNNING: the population has a ceiling this crate owns.
        state.stopping = false;
        for _ in 0..(MAX_IMPLICIT_QUERIES + 4) {
            let _ = behaviour.get_closest_peers(PeerId::random());
        }
        let mut running = Vec::new();
        reconcile_implicit(&mut state, &behaviour, &mut running);
        assert_eq!(
            state.implicit.len(),
            MAX_IMPLICIT_QUERIES,
            "the bound is this repository's, not a dependency's internal throttle"
        );
    }

    #[test]
    fn an_implicit_query_is_announced_once_and_settled_by_name() {
        // THE FACT FOUR REVIEW ROUNDS COULD NOT GUESS. An implicit
        // bootstrap never passes through `handle_command`, so "absent
        // from `queries`" was equally true of one that had just started
        // and one running since an earlier insertion. Round 8 of PR #61
        // concluded cancelling all of them was wrong; round 9 concluded
        // cancelling none was also wrong. Remembering which are known
        // makes "new" answerable by looking, and the answer is a name.
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 20,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let mut state = KademliaState::new(&settings);
        let mut behaviour = build_behaviour(&settings, PeerId::random()).expect("buildable");

        // One query the library started; nobody commanded it.
        let first = behaviour.get_closest_peers(PeerId::random());
        let mut out = Vec::new();
        reconcile_implicit(&mut state, &behaviour, &mut out);
        let announced: Vec<QueryHandle> = out
            .iter()
            .filter_map(|e| match e {
                KademliaEvent::QueryStarted { handle, origin, .. } => {
                    assert_eq!(*origin, QueryOrigin::Implicit);
                    Some(*handle)
                }
                _ => None,
            })
            .collect();
        assert_eq!(announced.len(), 1, "the new query is announced");
        assert_eq!(
            state.implicit.get(&first),
            Some(&announced[0]),
            "and remembered under the name it was given"
        );

        // ANNOUNCED ONCE. A second pass must not charge it again — that
        // is the half "cancel everything uncommanded" got wrong.
        out.clear();
        reconcile_implicit(&mut state, &behaviour, &mut out);
        assert!(
            out.is_empty(),
            "a query already known is not new, however many times it is looked at"
        );

        // A SECOND one starts. Only it is new.
        let second = behaviour.get_closest_peers(PeerId::random());
        out.clear();
        reconcile_implicit(&mut state, &behaviour, &mut out);
        assert_eq!(out.len(), 1, "only the newcomer is announced");
        assert_ne!(
            state.implicit.get(&second),
            state.implicit.get(&first),
            "and it is a different query, not the same one twice"
        );
    }

    #[test]
    fn shutdown_cancels_the_query_it_reports_as_settled() {
        // Review finding on PR #61: draining `queries` reported each as
        // `ShuttingDown` and left the libp2p query RUNNING, so the
        // behaviour kept sending requests and attempting query dials
        // until its own timeout — after the provider had been told the
        // work ended. Under `Drain` the Swarm task stays alive, so that
        // is not academic.
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 20,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let local = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let mut state = KademliaState::new(&settings);
        let mut behaviour = build_behaviour(&settings, local).expect("buildable");
        let manager = interweave_transport_runtime::ConnectionManager::new(
            interweave_transport_runtime::ConnectionPolicy::default(),
            8,
        );

        let id = behaviour.get_closest_peers(PeerId::random());
        state
            .queries
            .insert(id, (QueryClass::Exploration, QueryHandle::commanded(1)));
        // FOR A CLOSEST-PEERS WALK ONLY, `finish` really is terminal:
        // `query_finished` has no continuation for that class. So
        // `iter_queries` going quiet here means the query ended. It does
        // NOT mean that for a bootstrap, which re-enters for the next
        // bucket — see `a_draining_driver_issues_no_dial_from_a_re_entered_bootstrap`,
        // which counts dials because this observable cannot see that.
        assert!(
            behaviour.iter_queries().any(|q| q.id() == id),
            "the control: the query is running before the shutdown"
        );

        let out = handle_command(
            &mut state,
            &mut behaviour,
            &manager,
            KademliaCommand::Shutdown,
            0,
        );
        assert!(
            out.iter().any(|e| matches!(
                e,
                KademliaEvent::QueryFailed {
                    reason: QueryFailure::ShuttingDown,
                    ..
                }
            )),
            "the settlement is still reported"
        );
        assert!(
            !behaviour.iter_queries().any(|q| q.id() == id),
            "and the query it settled is actually finished, not left running to \
             send requests and dial until its own timeout"
        );
    }

    #[test]
    fn shutdown_settles_the_implicit_bootstrap_it_never_commanded() {
        // Review finding on PR #61. A library-started bootstrap (F2) is
        // deliberately ABSENT from `queries` — the completion path
        // treats an unknown id as implicit — so draining that map
        // neither finished the query nor emitted the settlement that
        // releases the provider's unscheduled charge. It kept querying
        // past the shutdown, and the charge was held for the life of
        // the provider, permanently narrowing its budget.
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 20,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let local = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let mut state = KademliaState::new(&settings);
        let mut behaviour = build_behaviour(&settings, local).expect("buildable");
        let manager = interweave_transport_runtime::ConnectionManager::new(
            interweave_transport_runtime::ConnectionPolicy::default(),
            8,
        );

        // Started by the library, never recorded in `queries` — the
        // shape F2 measured.
        let implicit = behaviour.get_closest_peers(PeerId::random());
        assert!(
            !state.queries.contains_key(&implicit),
            "the control: this is exactly the id the driver did not command"
        );

        let out = handle_command(
            &mut state,
            &mut behaviour,
            &manager,
            KademliaCommand::Shutdown,
            0,
        );
        assert!(
            out.iter().any(|e| matches!(
                e,
                KademliaEvent::QueryFailed {
                    class: QueryClass::Bootstrap,
                    reason: QueryFailure::ShuttingDown,
                    ..
                }
            )),
            "it settles as the bootstrap class, which is the completion the \
             provider's unscheduled charge keys on"
        );
        assert!(
            !behaviour.iter_queries().any(|q| q.id() == implicit),
            "and it is actually finished — true for a closest-peers walk, \
             whose class has no continuation. The bootstrap case is held by \
             the dial-counting test, not by this observable."
        );
    }

    #[test]
    fn a_queued_routing_update_for_a_revoked_peer_is_not_accepted() {
        // Review finding on PR #61. `add_address` QUEUES this event, and
        // trust can be revoked — or the protocol withdrawn — between the
        // queueing and the poll. Both paths remove the peer and tell the
        // behaviour to drop it; this handler then inserted it straight
        // back and announced it, resurrecting a seat the behaviour no
        // longer holds and handing the provider an addition already
        // false when it was sent.
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 20,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let mut state = KademliaState::new(&settings);
        let mut behaviour = build_behaviour(&settings, PeerId::random()).expect("buildable");
        let mut manager = interweave_transport_runtime::ConnectionManager::new(
            interweave_transport_runtime::ConnectionPolicy::default(),
            8,
        );
        let revoked = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let trusted = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let trusted_id = to_transport_identity(&trusted).expect("canonical");
        let _ = manager.set_trust(
            interweave_transport_runtime::TrustSources::new(
                interweave_trust_api::PeerTrustPolicy::new([trusted_id]).expect("small"),
                interweave_trust_api::InfrastructureSet::default(),
            ),
            &[],
        );
        // Both advertise; only one is still trusted.
        state.advertises.insert(revoked, (true, 0));
        state.advertises.insert(trusted, (true, 0));
        // An implicit bootstrap already running, started by earlier
        // eligible work — the query the sweep must not touch.
        let unrelated = behaviour.get_closest_peers(PeerId::random());

        let mut out = Vec::new();
        handle_kad_event(
            &mut state,
            Some(&mut behaviour),
            &manager,
            kad::Event::RoutingUpdated {
                peer: revoked,
                is_new_peer: true,
                addresses: kad::Addresses::new("/ip4/127.0.0.1/tcp/1".parse().expect("valid")),
                bucket_range: (
                    kad::KBucketDistance::default(),
                    kad::KBucketDistance::default(),
                ),
                old_peer: None,
            },
            0,
            &mut out,
        );
        assert!(
            !state.routed.contains(&revoked),
            "a seat queued before the revocation is not granted after it"
        );
        // AND AN UNRELATED BOOTSTRAP IS LEFT ALONE. An implicit query
        // is absent from `queries` by construction, so "every query not
        // in `queries`" cannot tell one THIS insertion started from one
        // an earlier eligible insertion did. On a running driver the
        // earlier one is still needed; cancelling it settled a charge
        // for work that had not finished.
        assert!(
            !out.iter().any(|e| matches!(
                e,
                KademliaEvent::QueryFailed {
                    reason: QueryFailure::ShuttingDown,
                    ..
                }
            )),
            "a running driver cancels no query when it declines a seat"
        );
        assert!(
            behaviour.iter_queries().any(|q| q.id() == unrelated),
            "and the unrelated bootstrap is still running"
        );
        assert!(out.is_empty(), "and no addition is announced");

        // The CONTROL: a still-eligible peer is admitted, so this test
        // cannot pass for a handler that accepts nobody.
        handle_kad_event(
            &mut state,
            Some(&mut behaviour),
            &manager,
            kad::Event::RoutingUpdated {
                peer: trusted,
                is_new_peer: true,
                addresses: kad::Addresses::new("/ip4/127.0.0.1/tcp/2".parse().expect("valid")),
                bucket_range: (
                    kad::KBucketDistance::default(),
                    kad::KBucketDistance::default(),
                ),
                old_peer: None,
            },
            0,
            &mut out,
        );
        assert!(state.routed.contains(&trusted));
        assert_eq!(out.len(), 1, "the eligible peer is announced");

        // AND A STOPPING DRIVER ACCEPTS NEITHER. A `Pending` insertion
        // can land after `Drain` shut the driver down; recreating the
        // seat there would also let an empty-to-nonempty transition
        // start a fresh implicit bootstrap after the shutdown sweep had
        // already run — query dials during a drained lifetime.
        state.stopping = true;
        state.routed.remove(&trusted);
        // ACTUALLY IN THE TABLE FIRST. Synthesising the event alone
        // proved nothing: with the peer absent, `remove_peer` is a
        // no-op and the assertion below held whether or not it ran.
        let _ = behaviour.add_address(&trusted, "/ip4/127.0.0.1/tcp/9".parse().expect("valid"));
        assert!(
            behaviour
                .kbuckets()
                .flat_map(|b| b.iter().map(|e| *e.node.key.preimage()).collect::<Vec<_>>())
                .any(|p| p == trusted),
            "the control: the behaviour really holds it before the event"
        );
        let mut after = Vec::new();
        handle_kad_event(
            &mut state,
            Some(&mut behaviour),
            &manager,
            kad::Event::RoutingUpdated {
                peer: trusted,
                is_new_peer: true,
                addresses: kad::Addresses::new("/ip4/127.0.0.1/tcp/3".parse().expect("valid")),
                bucket_range: (
                    kad::KBucketDistance::default(),
                    kad::KBucketDistance::default(),
                ),
                old_peer: None,
            },
            0,
            &mut after,
        );
        assert!(
            !state.routed.contains(&trusted),
            "a seat queued before the drain is not granted after it"
        );
        assert!(
            !after
                .iter()
                .any(|e| matches!(e, KademliaEvent::RoutingPeerAdded { .. })),
            "and nothing is announced"
        );
        // AND THE BEHAVIOUR'S OWN ENTRY IS GONE. Review finding on the
        // previous version of this fix: `RoutingUpdated` is emitted
        // AFTER the insertion, so declining the bookkeeping left the
        // real routing entry in place — and an empty-to-nonempty
        // insertion can still start an implicit bootstrap, which is
        // exactly the post-drain query activity this check exists to
        // prevent. Refusing the bookkeeping is not refusing the seat.
        assert!(
            behaviour
                .kbuckets()
                .flat_map(|b| b.iter().map(|e| *e.node.key.preimage()).collect::<Vec<_>>())
                .all(|p| p != trusted),
            "the peer is removed from the routing table itself, not only from `routed`"
        );
    }

    #[test]
    fn the_results_to_bucket_cross_field_rule_is_enforced() {
        // Review finding on PR #61. Every field passed its own range
        // while the PAIR was illegal: `kbucket_size = 8` with
        // `max_results_per_query = 20` asks for more results than the
        // selected bucket width is allowed to support, which
        // `kademlia-control-api::validate_limits` refuses. This
        // boundary is reachable without that validator because
        // `SubstrateConfig` is public.
        let base = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(8).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 20,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        assert!(
            base.validate().is_err(),
            "20 results out of an 8-wide bucket is refused"
        );
        assert!(
            KademliaSettings {
                max_results_per_query: NonZeroUsize::new(8).expect("nonzero"),
                ..base.clone()
            }
            .validate()
            .is_ok(),
            "and equal to the bucket width is the boundary that is allowed"
        );
    }

    #[test]
    fn an_abandoned_pending_seat_is_reclaimed_by_the_disconnect() {
        // Review finding on PR #61, against a comment written while
        // fixing the same class. The phantom-seat rollback said
        // "`Pending` is not a refusal: ... a disconnect reclaims it if
        // it does not [land]" — and `forget_disconnected` did not touch
        // `routed` at all, so the sentence named a mechanism that did
        // not exist. An `add_address` answering `Pending` queues the
        // peer behind a disconnected occupant; if that insertion is
        // abandoned, nothing removed the optimistic seat and churn
        // filled `max_routing_peers` with entries the table never held.
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 20,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let mut state = KademliaState::new(&settings);
        let provisional = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let held = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();

        // One seat the behaviour only QUEUED, one it CONFIRMED.
        state.routed.insert(provisional);
        state.unconfirmed.insert(provisional);
        state.routed.insert(held);

        forget_disconnected(&mut state, &provisional);
        assert!(
            !state.routed.contains(&provisional),
            "an unconfirmed claim is the caller's to withdraw, and the disconnect \
             is what withdraws it"
        );

        forget_disconnected(&mut state, &held);
        assert!(
            state.routed.contains(&held),
            "but a CONFIRMED seat outlives its connection — `RoutingUpdated` is the \
             behaviour's own report, and a routing entry deliberately survives"
        );
    }

    #[test]
    fn a_closed_connection_takes_its_advertisement_with_it() {
        // An Identify advertisement is an observation ABOUT a
        // connection. Held past the close, `advertises` filled with
        // entries nothing would refresh and the cap became a wall.
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 20,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let mut state = KademliaState::new(&settings);
        let peer = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        state.advertises.insert(peer, (true, 0));
        state
            .pending_offers
            .entry(peer)
            .or_default()
            .0
            .insert("/ip4/127.0.0.1/tcp/1".parse().expect("valid"));

        forget_disconnected(&mut state, &peer);
        assert!(
            state.advertises.is_empty(),
            "the advertisement did not outlive its connection"
        );
        assert!(
            state.pending_offers.is_empty(),
            "and an offer waiting on evidence from a closed connection waits on nothing"
        );
    }

    #[test]
    fn a_foreign_peer_suffix_rejects_the_address() {
        let subject = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let other = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let own: Multiaddr = format!("/ip4/192.0.2.1/tcp/1/p2p/{subject}")
            .parse()
            .expect("valid");
        assert_eq!(
            suffix_checked(&own, &subject).map(|a| a.to_string()),
            Some("/ip4/192.0.2.1/tcp/1".to_owned()),
            "the peer's own suffix strips"
        );
        let foreign: Multiaddr = format!("/ip4/192.0.2.1/tcp/1/p2p/{other}")
            .parse()
            .expect("valid");
        assert_eq!(
            suffix_checked(&foreign, &subject),
            None,
            "an address claiming another identity is the observation \
             contradicting itself — rejected, not relabelled"
        );
        // And through the query-result path, which every walk feeds.
        let info = kad::PeerInfo {
            peer_id: subject,
            addrs: vec![own, foreign, "/ip4/192.0.2.9/tcp/9".parse().expect("valid")],
        };
        let got = candidate_addresses(&info);
        assert!(got.contains("/ip4/192.0.2.1/tcp/1"));
        assert!(got.contains("/ip4/192.0.2.9/tcp/9"));
        assert_eq!(got.len(), 2, "the misattributed route is dropped");
    }

    #[test]
    fn the_driver_caps_concurrent_queries_and_settles_the_refusal() {
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 256,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let mut state = KademliaState::new(&settings);
        let mut behaviour = build_behaviour(&settings, PeerId::random()).expect("buildable");
        let manager = interweave_transport_runtime::ConnectionManager::new(
            interweave_transport_runtime::ConnectionPolicy::default(),
            8,
        );
        for i in 0..2_u8 {
            let out = handle_command(
                &mut state,
                &mut behaviour,
                &manager,
                KademliaCommand::StartQuery {
                    handle: QueryHandle::commanded(1),
                    class: QueryClass::Exploration,
                    key: LookupKey::KeySpacePoint { point: [i; 32] },
                },
                0,
            );
            assert!(out.is_empty(), "within the ceiling a query just runs");
        }
        let refused = handle_command(
            &mut state,
            &mut behaviour,
            &manager,
            KademliaCommand::StartQuery {
                handle: QueryHandle::commanded(1),
                class: QueryClass::Exploration,
                key: LookupKey::KeySpacePoint { point: [9; 32] },
            },
            0,
        );
        assert_eq!(
            refused,
            vec![KademliaEvent::QueryFailed {
                handle: QueryHandle::commanded(1),
                class: QueryClass::Exploration,
                reason: QueryFailure::BudgetExhausted,
            }],
            "the third is refused AND settled — a silent drop would leave \
             the caller's accounting waiting forever"
        );
        assert_eq!(state.queries.len(), 2, "nothing past the ceiling exists");
    }

    #[test]
    fn a_stopped_driver_ignores_every_command_and_settles_queries() {
        let settings = KademliaSettings {
            mode: KademliaMode::Client,
            network_id: "example-private-network".to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(30),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 256,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        };
        let mut state = KademliaState::new(&settings);
        let mut behaviour = build_behaviour(&settings, PeerId::random()).expect("buildable");
        let manager = interweave_transport_runtime::ConnectionManager::new(
            interweave_transport_runtime::ConnectionPolicy::default(),
            8,
        );
        let none = handle_command(
            &mut state,
            &mut behaviour,
            &manager,
            KademliaCommand::Shutdown,
            0,
        );
        assert!(none.is_empty(), "nothing was outstanding");

        // A SetMode { Server } after shutdown must NOT re-enable serving
        // and re-advertise the protocol.
        let none = handle_command(
            &mut state,
            &mut behaviour,
            &manager,
            KademliaCommand::SetMode {
                mode: KademliaMode::Server,
            },
            1,
        );
        assert!(none.is_empty());
        assert_eq!(
            behaviour.mode(),
            kad::Mode::Client,
            "stopped means stopped: the DHT protocol is not re-advertised"
        );
        // And a refused query is still settled.
        let refused = handle_command(
            &mut state,
            &mut behaviour,
            &manager,
            KademliaCommand::StartQuery {
                handle: QueryHandle::commanded(1),
                class: QueryClass::Exploration,
                key: LookupKey::KeySpacePoint { point: [1; 32] },
            },
            2,
        );
        assert_eq!(
            refused,
            vec![KademliaEvent::QueryFailed {
                handle: QueryHandle::commanded(1),
                class: QueryClass::Exploration,
                reason: QueryFailure::ShuttingDown,
            }]
        );
    }

    #[test]
    fn a_result_peers_addresses_are_bounded_while_read() {
        let subject = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let mut addrs: Vec<Multiaddr> = (0..interweave_discovery_api::MAX_ADDRESSES + 8)
            .map(|i| {
                format!("/ip4/198.51.100.{}/tcp/{}", i % 250, 1_000 + i)
                    .parse()
                    .expect("valid")
            })
            .collect();
        // One address past the byte bound, which must be skipped
        // without costing a slot.
        let oversized: Multiaddr = format!(
            "/dns4/{}.example.net/tcp/4001",
            "x".repeat(interweave_discovery_api::MAX_ADDRESS_BYTES)
        )
        .parse()
        .expect("valid");
        addrs.insert(0, oversized.clone());
        let info = kad::PeerInfo {
            peer_id: subject,
            addrs,
        };
        let got = candidate_addresses(&info);
        assert_eq!(
            got.len(),
            interweave_discovery_api::MAX_ADDRESSES,
            "the remote-authored list is capped while being read"
        );
        assert!(
            !got.contains(&oversized.to_string()),
            "an address past the byte bound is skipped, not carried"
        );
    }
}
