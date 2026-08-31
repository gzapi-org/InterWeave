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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::num::NonZeroUsize;
use std::time::Duration;

use libp2p::kad;
use libp2p::kad::store::MemoryStore;
use libp2p::{Multiaddr, PeerId, StreamProtocol, identify};
use sha2::{Digest, Sha256};

use interweave_kademlia_control_api::{
    KademliaCommand, KademliaEvent, KademliaMode, MAX_RESULTS_PER_QUERY, QueryClass, QueryFailure,
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
    /// Commanded queries in flight, by the class each was issued for.
    queries: HashMap<kad::QueryId, QueryClass>,
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
            queries: HashMap::new(),
            results: HashMap::new(),
            advertises: BTreeMap::new(),
            pending_offers: BTreeMap::new(),
            unconfirmed: BTreeSet::new(),
            record_writes_dropped: 0,
            stopping: false,
        }
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
        if let KademliaCommand::StartQuery { class, .. } = command {
            out.push(KademliaEvent::QueryFailed {
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
        KademliaCommand::StartQuery { class, key } => {
            // THE DRIVER'S OWN CEILING. The provider budgets its
            // commands, but the port is public: a caller pumping the
            // command channel faster than queries time out would grow
            // `queries`, `results` and the network work without bound.
            // Refused, and SETTLED as refused — a silent drop would
            // leave the caller's accounting waiting forever.
            if state.queries.len() >= state.max_concurrent_queries {
                out.push(KademliaEvent::QueryFailed {
                    class,
                    reason: QueryFailure::BudgetExhausted,
                });
                return out;
            }
            match class {
                QueryClass::Bootstrap => match behaviour.bootstrap() {
                    Ok(id) => {
                        state.queries.insert(id, class);
                    }
                    Err(_) => out.push(KademliaEvent::QueryFailed {
                        class,
                        reason: QueryFailure::NoRoutingPeers,
                    }),
                },
                QueryClass::Targeted => match peer_from_lookup_key(key) {
                    Some(target) => {
                        let id = behaviour.get_closest_peers(target);
                        state.queries.insert(id, class);
                    }
                    // The provider refuses untargetable identities
                    // upstream; a key that decodes to no PeerId here is
                    // defence in depth, and the settlement must still
                    // arrive or the budget slot waits forever. "No
                    // routing peers" is the nearest bounded truth: there
                    // is nothing at that key to route toward.
                    None => out.push(KademliaEvent::QueryFailed {
                        class,
                        reason: QueryFailure::NoRoutingPeers,
                    }),
                },
                QueryClass::Exploration => {
                    let results =
                        NonZeroUsize::new(state.max_results_per_query).unwrap_or(NonZeroUsize::MIN);
                    let id = behaviour.get_n_closest_peers(key.to_vec(), results);
                    state.queries.insert(id, class);
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
            for (_, class) in state.queries.drain() {
                out.push(KademliaEvent::QueryFailed {
                    class,
                    reason: QueryFailure::ShuttingDown,
                });
            }
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
            handle_kad_event(state, manager, kad_event, now_ms, out);
            KadHandled::Consumed
        }
        other => KadHandled::Passed(Box::new(other)),
    }
}

/// Fold one `kad::Event` onto the port.
fn handle_kad_event(
    state: &mut KademliaState,
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
                let eligible = to_transport_identity(&peer).ok().filter(|identity| {
                    may_hold_a_seat(manager, identity)
                        && matches!(state.advertises.get(&peer), Some((true, _)))
                });
                // The claim is settled either way; only an ELIGIBLE
                // peer keeps the seat. Not an early return: the
                // `old_peer` eviction below belongs to this same event
                // and must still be reported.
                state.unconfirmed.remove(&peer);
                if let Some(admitted) = eligible {
                    state.routed.insert(peer);
                    out.push(KademliaEvent::RoutingPeerAdded { peer: admitted });
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
                    let Some(class) = state.queries.remove(&id) else {
                        return;
                    };
                    let found = state.results.remove(&id).unwrap_or_default();
                    if found.is_empty() && timed_out {
                        out.push(KademliaEvent::QueryFailed {
                            class,
                            reason: QueryFailure::TimedOut,
                        });
                    } else if let Ok(candidates) =
                        interweave_kademlia_control_api::ObservedCandidates::new(found)
                    {
                        out.push(KademliaEvent::QueryResults { candidates, class });
                    }
                }
            }
            kad::QueryResult::Bootstrap(outcome) if step.last => {
                {
                    // An UNKNOWN id here is the library's implicit
                    // bootstrap — the one F2 measured, started by a
                    // routing insertion nobody scheduled. Reporting it
                    // as a bootstrap completion is what lets the
                    // provider settle the charge it took for it.
                    let class = state.queries.remove(&id).unwrap_or(QueryClass::Bootstrap);
                    match outcome {
                        // Empty is always within the bound; a bootstrap
                        // completion carries no candidates of its own.
                        Ok(_) => {
                            if let Ok(candidates) =
                                interweave_kademlia_control_api::ObservedCandidates::new([])
                            {
                                out.push(KademliaEvent::QueryResults { candidates, class });
                            }
                        }
                        Err(_) => out.push(KademliaEvent::QueryFailed {
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
        handle_kad_event(&mut state, &manager, put, 0, &mut out);
        let add = kad::Event::InboundRequest {
            request: kad::InboundRequest::AddProvider { record: None },
        };
        handle_kad_event(&mut state, &manager, add, 0, &mut out);
        let read = kad::Event::InboundRequest {
            request: kad::InboundRequest::FindNode {
                num_closer_peers: 1,
            },
        };
        handle_kad_event(&mut state, &manager, read, 0, &mut out);
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

        let mut out = Vec::new();
        handle_kad_event(
            &mut state,
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
        assert!(out.is_empty(), "and no addition is announced");

        // The CONTROL: a still-eligible peer is admitted, so this test
        // cannot pass for a handler that accepts nobody.
        handle_kad_event(
            &mut state,
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
                    class: QueryClass::Exploration,
                    key: [i; 32],
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
                class: QueryClass::Exploration,
                key: [9; 32],
            },
            0,
        );
        assert_eq!(
            refused,
            vec![KademliaEvent::QueryFailed {
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
                class: QueryClass::Exploration,
                key: [1; 32],
            },
            2,
        );
        assert_eq!(
            refused,
            vec![KademliaEvent::QueryFailed {
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
