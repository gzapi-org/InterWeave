// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Swarm side of mDNS: announcements in, candidates out.
//!
//! `crates/discovery/mdns` is the NORMALIZATION half -- PeerId grammar,
//! address bounds, dedup, expiry -- driven by pushed observations. This
//! is the half that hears the packets, and it is the Swarm's for the
//! reason the Kademlia driver is: every mutation stays in the Swarm
//! task, and the provider crate keeps no libp2p type.
//!
//! # The boundary runs HERE, and that is ADR-0052's own placement
//!
//! A discovered candidate is an address this runtime would dial because
//! a peer supplied it, so it is inside ADR-0052's boundary (rule 1), and
//! `providers/mdns.md` §Address class states this provider's instance of
//! rules 3 and 4. What makes the site unusual is rule 5: mDNS emits no
//! `ToSwarm::Dial` -- measured, the crate has none -- so there is no
//! crate dial to deny and reissue. The boundary binds where the
//! candidate is LEARNED, which is rule 5's last clause, "what this node
//! learns and offers is held inside the boundary too".
//!
//! An address refused on class never becomes an observation. Not
//! filtered downstream, not carried with a flag: this is the only door,
//! and past it the pair is an ordinary candidate.
//!
//! Together with [`crate::mdns_scope::MdnsScope`], which swallows the
//! crate's `NewExternalAddrOfPeer` and answers its pending-dial hook with
//! nothing, that is ADR-0011 §Discovery never writes the address book
//! seen from both sides -- nothing the crate hears reaches a book or a
//! dial except through this filter and the pipeline.
//!
//! # What it does not do
//!
//! It holds no provider. `crates/transport/libp2p` depends on
//! `discovery-api` and not on a concrete provider crate, the way the
//! Kademlia driver does, so this emits `CandidatePeer` values and the
//! composition root (plan §15) is where a `DiscoveryManager` receives
//! them. Until then the manager is a library composed in tests, which
//! is the state the Stage 9 record already describes.

use std::collections::{BTreeMap, BTreeSet};

use interweave_transport_api::TransportIdentity;
use libp2p::{Multiaddr, PeerId, mdns};

use super::to_transport_identity;
use crate::mdns_scope::MdnsScope;

/// The mDNS field's type in the composed behaviour.
///
/// `Toggle`, `None` by default: mDNS is present only when a profile
/// configured it, the shape every optional behaviour here takes.
///
/// NO `Attributing` AND NO `ClassGated`, and both absences are
/// decisions rather than omissions. `Attributing` announces the origin
/// of a dial, and mDNS originates none -- the crate has no
/// `ToSwarm::Dial` at all, measured. `ClassGated` decides which peers
/// are offered a protocol on a connection, and mDNS opens no substream
/// to a peer: it listens on a multicast group and reports. What it
/// needs instead is [`MdnsScope`], which is about what it may push INTO
/// the Swarm rather than what it may be asked for.
pub type MdnsField = libp2p::swarm::behaviour::toggle::Toggle<MdnsScope<mdns::tokio::Behaviour>>;

/// What a profile sets when it turns mDNS on.
///
/// Every field is a bound the crate takes as a `Duration`; they are
/// milliseconds here because a settings struct that is validated by
/// enumeration cannot carry a type whose invalid values it cannot name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdnsSettings {
    /// TTL announced for this node's own records.
    pub ttl_ms: u64,
    /// How often to re-query, so a lost initial packet is not a silent
    /// failure until the next peer joins.
    pub query_interval_ms: u64,
    /// Announce over IPv6 rather than IPv4.
    pub enable_ipv6: bool,
}

impl Default for MdnsSettings {
    fn default() -> Self {
        // The crate's own TTL default, restated rather than borrowed: a
        // default that moves with a dependency bump is a configuration
        // change nobody reviewed. THE QUERY INTERVAL IS NOT the crate's
        // 5 min: a receiver keeps a record at most `MAX_RECORD_TTL`
        // (120 s, ADR-0053 rule 3), so a 5 min interval let a quiet LAN
        // forget a peer and rediscover it every exchange. 90 s is three
        // quarters of the clamp, RFC 6762 section 5.2's cache-maintenance
        // point (ADR-0053 rule 3, #112 blind review F6).
        Self {
            ttl_ms: 6 * 60 * 1000,
            query_interval_ms: 90 * 1000,
            enable_ipv6: false,
        }
    }
}

/// The shortest re-query interval a profile may set (RFC 6762 §5.2).
pub const MIN_QUERY_INTERVAL_MS: u64 = 1_000;

/// The most the mDNS crate adds to the query interval as jitter:
/// `rand::random_range(0..100)` milliseconds in the vendored
/// `InterfaceState::new` (`third_party/libp2p-mdns/src/behaviour/iface.rs`),
/// restated here because the crate does not export it.
pub const QUERY_JITTER_MAX_MS: u64 = 99;

impl MdnsSettings {
    /// # Errors
    /// The first bound that is not usable, named.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.ttl_ms == 0 {
            return Err("mdns ttl_ms must be non-zero");
        }
        // A FLOOR, not merely non-zero: every query goes out as multicast
        // on every interface, so `ttl_ms: 2, query_interval_ms: 1` was a
        // node flooding its LAN once a millisecond (#111 mDNS review,
        // minor P3). One second is RFC 6762 §5.2's own floor for the
        // interval between continuous queries.
        if self.query_interval_ms < MIN_QUERY_INTERVAL_MS {
            return Err("mdns query_interval_ms must be at least one second");
        }
        // A QUERY INTERVAL PAST THE TTL IS A PROVIDER THAT FORGETS ITSELF.
        // Records lapse before the next query refreshes them, so a peer
        // that never left is announced, expired and re-announced on the
        // interval -- churn the normalization half then has to absorb,
        // and a health signal that flaps for a network that is fine.
        if self.query_interval_ms >= self.ttl_ms {
            return Err("mdns query_interval_ms must be below ttl_ms");
        }
        // AND BELOW WHAT A RECEIVER KEEPS, which since ADR-0053 rule 3 is
        // not our `ttl_ms` but the clamp every receiver built from this
        // crate applies: past it the same churn happens, on the other
        // side of the wire. Refused, not churned.
        // WITH ITS JITTER: the crate adds up to `QUERY_JITTER_MAX_MS` to
        // the interval (`InterfaceState::new`), so 119 999 ms passed and
        // could still reach the clamp (#112, the automated review's P2 on
        // 34fd3ad). The largest interval the crate can actually use is
        // what is bounded. Widened BEFORE the addition: an interval near
        // `u64::MAX` passes every check above, and the u64 sum would
        // overflow -- a panic under `overflow-checks`, not an `Err` (#112,
        // both reviews on 9f56dd83).
        if u128::from(self.query_interval_ms) + u128::from(QUERY_JITTER_MAX_MS)
            >= MAX_RECORD_TTL.as_millis()
        {
            return Err(
                "mdns query_interval_ms, with its jitter, must be below the 120 s record clamp",
            );
        }
        Ok(())
    }
}

/// Build the wrapped behaviour.
///
/// # Errors
/// The crate's interface watcher could not be created
/// (`libp2p-mdns 0.49.0` `behaviour.rs:172`, `P::new_watcher()`) -- the
/// ONE failure here, and an environment one. Not the multicast socket:
/// the crate binds that per interface inside its own `poll`, and since
/// ADR-0053 rule 5 a failure there is `Event::InterfaceFailed`, surfaced
/// as `SwarmEvent::MdnsInterfaceFailed` -- as released it was only
/// logged. The caller degrades rather than failing
/// (`providers/mdns.md` §Failure).
pub fn build_behaviour(
    settings: &MdnsSettings,
    local_pid: PeerId,
) -> std::io::Result<MdnsScope<mdns::tokio::Behaviour>> {
    let config = mdns::Config {
        ttl: std::time::Duration::from_millis(settings.ttl_ms),
        query_interval: std::time::Duration::from_millis(settings.query_interval_ms),
        enable_ipv6: settings.enable_ipv6,
    };
    mdns::tokio::Behaviour::new(config, local_pid).map(MdnsScope::new)
}

/// The vendored crate's record-store shape and TTL clamp (ADR-0053 rules
/// 2 and 3), re-exported so the workspace can drift-check them against
/// the provider's own bounds and observation TTL without naming a libp2p
/// crate: `tests/discovery-conformance/tests/composition_and_exit_gate.rs`
/// does.
pub use libp2p::mdns::{MAX_ADDRESSES_PER_DISCOVERED_PEER, MAX_DISCOVERED_PEERS, MAX_RECORD_TTL};

/// What ADR-0053's bounds dropped in the mDNS crate, read through
/// `SwarmRuntime::mdns_drop_counts` (rule 7). Counts only, never an
/// address.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MdnsDropCounts {
    /// Records evicted when a bound of the record store was hit; each is
    /// reported as expired unless the batch that evicted it had added it.
    pub records_evicted: u64,
    /// Records refused because they would have expired soonest within the
    /// bound they hit, the peer bound or one peer's address bound.
    pub records_refused: u64,
    /// Pairs an interface dropped because its queue was full.
    pub discovered_dropped: u64,
    /// Packets an interface dropped because its send buffer was full.
    pub packets_dropped: u64,
    /// Queries not answered because the interface had sent the same
    /// answer less than a second before, or still held it queued.
    pub queries_unanswered: u64,
    /// Interface-failure reports lost because their channel was full.
    pub failures_dropped: u64,
}

impl MdnsDropCounts {
    pub(crate) fn read(counts: &libp2p::mdns::DropCounts) -> Self {
        Self {
            records_evicted: counts.records_evicted(),
            records_refused: counts.records_refused(),
            discovered_dropped: counts.discovered_dropped(),
            packets_dropped: counts.packets_dropped(),
            queries_unanswered: counts.queries_unanswered(),
            failures_dropped: counts.failures_dropped(),
        }
    }
}

/// Peers one announcement batch may yield, however many announce.
///
/// `MAX_ADDRESSES` bounds the addresses of ONE peer; nothing bounded
/// the number of peers, so a single host announcing distinct PeerIds
/// chose the size of one `SwarmEvent::MdnsDiscovered`. This bounds the
/// EVENT, not the work: the driver still judges every pair the crate
/// reports before the bound drops it. The crate's own store beneath it
/// was unbounded as released; since ADR-0053 the vendored copy caps it at
/// the provider's shape (`MAX_DISCOVERED_PEERS` peers,
/// `MAX_ADDRESSES_PER_DISCOVERED_PEER` each) and clamps its TTLs, which bounds that
/// work now (DISCOVERY-CONFORMANCE.md's Decision 2026-09-25 recorded the
/// exception until then; #111 mDNS review F2).
/// `may_buffer_delivery` does not help:
/// it bounds how many events sit in the outbox, not how large one is.
/// `DISCOVERY-CONFORMANCE.md` guarantee 5 bounds emitted BATCHES by
/// name, and mDNS input is the least trusted this process takes --
/// any host on the multicast domain, unauthenticated.
///
/// NOT THE PROVIDER'S CONSTANT, though it is the same 256 today.
/// `crates/discovery/mdns`'s `MAX_PEERS` bounds the provider's whole
/// state; this bounds one event. This crate does not depend on the
/// provider and must not start: the transport learns, the provider
/// normalizes, and the two meet through `discovery-api`.
///
/// What must hold between them -- a batch naming no more peers than the
/// provider can hold -- is checked where both crates are visible:
/// `tests/discovery-conformance/tests/composition_and_exit_gate.rs`,
/// `a_drivers_batch_names_no_more_peers_than_the_provider_holds`. An
/// earlier version pinned the two as equal and called it one shape
/// (#111 mDNS review F7). Public for that test, and so the public docs
/// that name it can link to it.
pub const MAX_PEERS_PER_BATCH: usize = 256;

/// What the learn-site filter did, by class -- a snapshot of the MDNS
/// entry in the runtime's shared store counts ([`MdnsState::counters`]).
///
/// Counted rather than logged: an address refused on class is never
/// written to a log (ADR-0052 rule 5), and a count is what lets a test
/// tell a filter that ran from a multicast domain that was quiet.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MdnsCounters {
    /// Pairs that passed the boundary and were offered to the batch.
    ///
    /// Not "became candidates": a duplicate pair, or one past a bound,
    /// passed the boundary too. That is the meaning every store's count
    /// has, and an earlier doc here claimed the narrower one while
    /// counting the wider (#111 re-review).
    pub admitted: usize,
    /// Pairs refused, by the class they were refused for.
    pub refused: BTreeMap<&'static str, usize>,
    /// Pairs dropped for a bound: a batch or hold already at
    /// [`MAX_PEERS_PER_BATCH`] distinct peers, or a peer in it already at
    /// `MAX_ADDRESSES` addresses -- in a discovery, a retraction, or
    /// either hold. The name predates the address half; it said "the
    /// batch was already at `MAX_PEERS_PER_BATCH`" alone while counting
    /// both (#111 mDNS review F6).
    ///
    /// Separate from `refused`, which is ADR-0052's classes: a bound is
    /// not a judgement about the address, and folding it in would make
    /// a flood read as a boundary refusal.
    pub over_peer_bound: usize,
}

impl MdnsCounters {
    /// Every refusal, whatever its class.
    #[must_use]
    pub fn refused_total(&self) -> usize {
        self.refused.values().sum()
    }
}

/// The driver's state: what the filter has done, and the one record of
/// the operator's door it consults.
#[derive(Debug, Default)]
pub struct MdnsState {
    /// Where this learn site files what it admitted, refused and dropped
    /// for its bound: the runtime's shared handle, readable through
    /// `SwarmRuntime::store_refusals` (#111 re-review P2-5 -- the counts
    /// used to live here, where nothing outside this file could read
    /// them).
    stores: crate::store_refusals::StoreRefusals,
    /// What came in by the operator's door, admitted whatever its class
    /// (ADR-0052 rule 9). The runtime's one set, shared.
    operator: crate::operator_set::OperatorSet,
    /// Discoveries the outbox had no room for, HELD rather than dropped.
    ///
    /// A dropped `Discovered` is not re-emitted by the crate until its
    /// record lapses -- `libp2p-mdns 0.49.0` dedups against
    /// `discovered_nodes` (`behaviour.rs:329`); six minutes by default
    /// as released, at most `MAX_RECORD_TTL` (120 s) since ADR-0053's
    /// clamp -- so dropping it under backpressure left a reachable LAN
    /// peer undiscovered for that long (#111 review F1). Bounded by the same
    /// per-batch peer bound as a discovery itself.
    held_discovered: BTreeMap<TransportIdentity, BTreeSet<String>>,
    /// Retractions the outbox had no room for, held likewise.
    ///
    /// DISJOINT from `held_discovered` by construction: holding an expiry
    /// cancels a held discovery of the same pair, and holding a discovery
    /// cancels a held expiry. So splitting them across slots cannot net a
    /// pair wrongly, and a pair that came and went while the consumer was
    /// behind nets to what actually happened -- given the crate's own
    /// batches are netted, which ADR-0053 rule 2's patch does. The ORDER
    /// still matters for capacity: the flush delivers retractions first
    /// (#112).
    held_expired: BTreeMap<TransportIdentity, BTreeSet<String>>,
    /// Interface failures the outbox had no room for (ADR-0053 rule 5),
    /// the latest reason per interface. Bounded by this node's own
    /// interfaces: the key is its own address, which no remote host adds.
    held_failures: BTreeMap<std::net::IpAddr, String>,
    /// A watcher failure the outbox had no room for, the latest winning:
    /// at most one (ADR-0053 rule 5).
    held_watcher_failure: Option<String>,
}

impl MdnsState {
    /// A fresh driver.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Share the runtime's operator set (rule 9) and store counts
    /// (rule 8) with this learn site.
    #[must_use]
    pub fn with_boundary(
        mut self,
        operator: crate::operator_set::OperatorSet,
        stores: crate::store_refusals::StoreRefusals,
    ) -> Self {
        self.operator = operator;
        self.stores = stores;
        self
    }

    /// What the learn-site filter has done so far.
    ///
    /// A VIEW over the runtime's shared store counts, the MDNS entry: the
    /// same numbers `SwarmRuntime::store_refusals` reports, so a test
    /// reading them here and an operator reading them there cannot
    /// disagree.
    #[must_use]
    pub fn counters(&self) -> MdnsCounters {
        let counts = self.stores.get(crate::store_refusals::store::MDNS);
        MdnsCounters {
            admitted: counts.admitted,
            refused: counts.refused,
            over_peer_bound: counts.over_bound,
        }
    }

    /// Turn one `Discovered` into the candidates that survive the
    /// boundary.
    ///
    /// `own_listeners` is this node's bound listener set, which rule 3
    /// needs: a private candidate is admitted only beside a private
    /// listener of the same family.
    pub fn on_discovered<'a>(
        &mut self,
        pairs: &[(PeerId, Multiaddr)],
        own_listeners: impl IntoIterator<Item = &'a str> + Clone,
        now_ms: u64,
    ) -> Vec<interweave_discovery_api::CandidatePeer> {
        let mut by_peer: BTreeMap<TransportIdentity, BTreeSet<String>> = BTreeMap::new();
        for (peer, address) in pairs {
            let Ok(identity) = to_transport_identity(peer) else {
                continue;
            };
            // THE ROUTE, not the crate's spelling of it: `libp2p-mdns`
            // appends `/p2p/<peer>` to every address it reports
            // (`query.rs:195`), and the Kademlia driver strips the same
            // suffix, so emitting it verbatim made one route two keys in
            // the provider's dedup and spent two of its address slots
            // (#111 mDNS review F8). The Kademlia driver's own function,
            // so a foreign suffix is refused here exactly as there.
            let Some(route) = super::kademlia_driver::suffix_checked(address, peer) else {
                continue;
            };
            let text = route.to_string();
            let verdict = self
                .operator
                .admits_discovered(&route, own_listeners.clone());
            if !self
                .stores
                .record(crate::store_refusals::store::MDNS, verdict)
            {
                continue;
            }
            // THE BOUNDS ARE CHECKED WHILE READING, not after: the
            // announcement is remote-authored, so collecting first and
            // capping later would hold an oversized address in the
            // accumulator and emit what downstream validation refuses.
            // The Kademlia driver reads its query results the same way,
            // including the PEER bound -- `kademlia_driver` breaks at
            // `max_results_per_query`, and an earlier version of this
            // comment claimed that parity while having no peer bound at
            // all.
            if text.is_empty() || text.len() > interweave_discovery_api::MAX_ADDRESS_BYTES {
                continue;
            }
            // A PEER THIS BATCH HAS NOT SEEN COSTS A SLOT; one it has
            // costs nothing. What the membership test prevents is
            // narrower than it looks: once the batch already holds
            // `MAX_PEERS_PER_BATCH` distinct peers, a FURTHER address for
            // one of them must still be taken rather than dropped and
            // counted as a flood. (`by_peer.len()` counts distinct peers,
            // so a single chatty peer never nears the bound whatever this
            // clause says -- an earlier version of this comment and its
            // test both claimed that case, and the test could not fail.)
            if !by_peer.contains_key(&identity) && by_peer.len() >= MAX_PEERS_PER_BATCH {
                self.stores.over_bound(crate::store_refusals::store::MDNS);
                continue;
            }
            let addresses = by_peer.entry(identity).or_default();
            // COUNTED, like every other bound here: it was the one drop
            // that left no trace (#111 mDNS review F6).
            if addresses.len() >= interweave_discovery_api::MAX_ADDRESSES
                && !addresses.contains(&text)
            {
                self.stores.over_bound(crate::store_refusals::store::MDNS);
                continue;
            }
            addresses.insert(text);
        }
        by_peer
            .into_iter()
            .map(
                |(peer_id, addresses)| interweave_discovery_api::CandidatePeer {
                    peer_id,
                    addresses,
                    source: "mdns".to_owned(),
                    observed_at: now_ms,
                    expires_at: None,
                    protocol_observations: BTreeSet::new(),
                },
            )
            .collect()
    }

    /// Hold discoveries the outbox could not take (see `held_discovered`).
    ///
    /// A held address for a peer is merged into what is already held for
    /// it, up to `MAX_ADDRESSES`; a peer not yet held takes a slot only
    /// while fewer than [`MAX_PEERS_PER_BATCH`] are. What does not fit is
    /// counted as over the bound, never silently lost. Each held pair
    /// cancels a held expiry of the same pair.
    pub fn hold_discovered(&mut self, candidates: Vec<interweave_discovery_api::CandidatePeer>) {
        for candidate in candidates {
            for address in candidate.addresses {
                if let Some(held) = self.held_expired.get_mut(&candidate.peer_id) {
                    held.remove(&address);
                    if held.is_empty() {
                        self.held_expired.remove(&candidate.peer_id);
                    }
                }
                if !self.held_discovered.contains_key(&candidate.peer_id)
                    && self.held_discovered.len() >= MAX_PEERS_PER_BATCH
                {
                    self.stores.over_bound(crate::store_refusals::store::MDNS);
                    continue;
                }
                let held = self
                    .held_discovered
                    .entry(candidate.peer_id.clone())
                    .or_default();
                if held.len() >= interweave_discovery_api::MAX_ADDRESSES && !held.contains(&address)
                {
                    self.stores.over_bound(crate::store_refusals::store::MDNS);
                    continue;
                }
                held.insert(address);
            }
        }
    }

    /// Hold retractions the outbox could not take (see `held_expired`).
    ///
    /// Each cancels a held, undelivered discovery of the same pair -- and
    /// is still held itself, because the provider may have that pair from
    /// an EARLIER delivered discovery, and a retraction of a pair it does
    /// not hold is harmless. Bounded in the same shape as a discovery --
    /// [`MAX_PEERS_PER_BATCH`] peers, `MAX_ADDRESSES` each -- which is NOT
    /// a promise that everything discovery admitted fits: the hold takes
    /// the retractions `on_expired` produced, class-refused pairs
    /// included, so they can crowd an admitted one out, as there (#111
    /// mDNS review F6). Past the bound the provider's own ageing is the
    /// backstop, and the drop is counted.
    pub fn hold_expired(&mut self, expired: Vec<(TransportIdentity, String)>) {
        for (peer, address) in expired {
            if let Some(held) = self.held_discovered.get_mut(&peer) {
                held.remove(&address);
                if held.is_empty() {
                    self.held_discovered.remove(&peer);
                }
            }
            let fits = match self.held_expired.get(&peer) {
                None => self.held_expired.len() < MAX_PEERS_PER_BATCH,
                Some(held) => {
                    held.contains(&address) || held.len() < interweave_discovery_api::MAX_ADDRESSES
                }
            };
            if !fits {
                self.stores.over_bound(crate::store_refusals::store::MDNS);
                continue;
            }
            self.held_expired.entry(peer).or_default().insert(address);
        }
    }

    /// Everything held, taken out for delivery: the discoveries as
    /// candidates stamped `now_ms`, and the retractions. Empty when
    /// nothing is held, which is the common case and costs nothing.
    #[must_use]
    pub fn take_held(
        &mut self,
        now_ms: u64,
    ) -> (
        Vec<interweave_discovery_api::CandidatePeer>,
        Vec<(TransportIdentity, String)>,
    ) {
        (self.take_held_discovered(now_ms), self.take_held_expired())
    }

    /// The held discoveries alone, as one batch. The discovery and
    /// retraction holds are disjoint by pair (each cancels the other's
    /// entry for a pair it takes), so delivering them in separate slots
    /// cannot net a pair wrongly: that is what lets the flush deliver ONE
    /// event when there is room for only one. It delivers the retractions
    /// FIRST all the same, because a consumer at capacity needs their room
    /// before the discoveries (#112).
    pub fn take_held_discovered(
        &mut self,
        now_ms: u64,
    ) -> Vec<interweave_discovery_api::CandidatePeer> {
        std::mem::take(&mut self.held_discovered)
            .into_iter()
            .map(
                |(peer_id, addresses)| interweave_discovery_api::CandidatePeer {
                    peer_id,
                    addresses,
                    source: "mdns".to_owned(),
                    observed_at: now_ms,
                    expires_at: None,
                    protocol_observations: BTreeSet::new(),
                },
            )
            .collect()
    }

    /// The held retractions alone, as one batch; see
    /// [`Self::take_held_discovered`].
    pub fn take_held_expired(&mut self) -> Vec<(TransportIdentity, String)> {
        std::mem::take(&mut self.held_expired)
            .into_iter()
            .flat_map(|(peer, addresses)| {
                addresses
                    .into_iter()
                    .map(move |address| (peer.clone(), address))
            })
            .collect()
    }

    /// Whether anything is held for delivery.
    #[must_use]
    pub fn holds_anything(&self) -> bool {
        self.holds_discovered()
            || self.holds_expired()
            || !self.held_failures.is_empty()
            || self.held_watcher_failure.is_some()
    }

    /// Hold a watcher failure the outbox could not take; a later one
    /// replaces it.
    pub fn hold_watcher_failure(&mut self, detail: String) {
        self.held_watcher_failure = Some(detail);
    }

    /// The held watcher failure, taken out for delivery.
    pub fn take_held_watcher_failure(&mut self) -> Option<String> {
        self.held_watcher_failure.take()
    }

    /// Hold an interface failure the outbox could not take; a later one
    /// for the same interface replaces it.
    pub fn hold_failure(&mut self, address: std::net::IpAddr, detail: String) {
        let _ = self.held_failures.insert(address, detail);
    }

    /// The first held interface failure, taken out for delivery.
    pub fn take_held_failure(&mut self) -> Option<(std::net::IpAddr, String)> {
        self.held_failures.pop_first()
    }

    /// Whether a discovery is held for delivery.
    #[must_use]
    pub fn holds_discovered(&self) -> bool {
        !self.held_discovered.is_empty()
    }

    /// Whether a retraction is held for delivery.
    #[must_use]
    pub fn holds_expired(&self) -> bool {
        !self.held_expired.is_empty()
    }

    /// Turn one `Expired` into the retractions the provider takes.
    ///
    /// NO BOUNDARY HERE, and that is deliberate. A retraction removes a
    /// pair the provider may already hold; refusing it on class would
    /// leave an address this node once admitted in place until the
    /// provider's own ageing removed it -- the only event that would
    /// clear it sooner is the one being refused. The floor decides what
    /// may be DIALLED, not what may be forgotten. (An earlier version
    /// said "forever"; the provider's TTL is the backstop, and a
    /// retraction the outbox cannot take is held rather than dropped.)
    pub fn on_expired(
        &mut self,
        pairs: &[(PeerId, Multiaddr)],
    ) -> Vec<(TransportIdentity, String)> {
        // BOUNDED BY THE SAME SHAPE AS THE DISCOVERY -- at most
        // `MAX_PEERS_PER_BATCH` distinct peers, each at most
        // `MAX_ADDRESSES` addresses -- and this is not a class judgement:
        // the retraction is still unfiltered on address class (above).
        // An earlier version bounded retractions by PAIRS at the peer
        // bound, so a legitimate expiry of 200 peers on two addresses
        // each dropped 144 retractions and counted them as over the peer
        // bound (#111 re-review P3-7).
        //
        // WHAT THIS DOES NOT PROMISE: that anything discovery admitted is
        // retractable in the same batch. A retraction spends a slot for
        // every pair, including pairs discovery refused on class, and it
        // cannot tell them apart -- a pair admitted beside a private
        // listener that has since gone is refused on class NOW and must
        // still be retracted. So a batch mixing refused pairs with
        // admitted ones can crowd an admitted retraction out. That drop
        // is counted, and the provider's own ageing is the backstop
        // (#111 mDNS review F6, which found the stronger claim false).
        let mut out: Vec<(TransportIdentity, String)> = Vec::new();
        let mut per_peer: BTreeMap<TransportIdentity, usize> = BTreeMap::new();
        for (peer, address) in pairs {
            let Ok(identity) = to_transport_identity(peer) else {
                continue;
            };
            // THE SAME KEY AS THE DISCOVERY'S: stripped here too, or a
            // retraction names a string the provider never holds.
            let Some(route) = super::kademlia_driver::suffix_checked(address, peer) else {
                continue;
            };
            let held = per_peer.get(&identity).copied();
            let fits = match held {
                None => per_peer.len() < MAX_PEERS_PER_BATCH,
                Some(n) => n < interweave_discovery_api::MAX_ADDRESSES,
            };
            if !fits {
                self.stores.over_bound(crate::store_refusals::store::MDNS);
                continue;
            }
            *per_peer.entry(identity.clone()).or_default() += 1;
            out.push((identity, route.to_string()));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;

    /// A REAL Ed25519 identity. `PeerId::random()` mints a digest-form
    /// id the neutral grammar refuses, so a test built on it measures
    /// `to_transport_identity` rejecting everything rather than the
    /// filter -- the trap the Kademlia driver's tests already record.
    fn peer() -> PeerId {
        libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id()
    }

    /// One host announcing more peers than the batch may carry does not
    /// choose the size of the event, or of the work building it.
    ///
    /// `DISCOVERY-CONFORMANCE.md` guarantee 5 bounds emitted batches.
    /// Remove the peer bound in `on_discovered` and this fails with
    /// `MAX_PEERS_PER_BATCH + 40` candidates.
    #[test]
    fn a_flood_of_distinct_peers_stops_at_the_batch_bound() {
        let over = 40;
        let pairs: Vec<(PeerId, Multiaddr)> = (0..MAX_PEERS_PER_BATCH + over)
            .map(|i| {
                let port = 4001 + u16::try_from(i % 1000).expect("port fits");
                (
                    peer(),
                    format!("/ip4/8.8.8.8/tcp/{port}")
                        .parse()
                        .expect("a global literal the floor admits"),
                )
            })
            .collect();

        let mut state = MdnsState::new();
        let candidates = state.on_discovered(&pairs, ["/ip4/0.0.0.0/tcp/1"], 0);

        assert_eq!(
            candidates.len(),
            MAX_PEERS_PER_BATCH,
            "the batch must stop at the bound, not at whatever the announcer sent"
        );
        assert_eq!(
            state.counters().over_peer_bound,
            over,
            "and the drop is counted, or a flood is indistinguishable from a quiet domain"
        );
        assert_eq!(
            state.counters().refused_total(),
            0,
            "a bound is not an ADR-0052 class refusal and must not be reported as one"
        );
    }

    fn candidate(
        peer: &TransportIdentity,
        addresses: &[&str],
    ) -> interweave_discovery_api::CandidatePeer {
        interweave_discovery_api::CandidatePeer {
            peer_id: peer.clone(),
            addresses: addresses.iter().map(|a| (*a).to_owned()).collect(),
            source: "mdns".to_owned(),
            observed_at: 0,
            expires_at: None,
            protocol_observations: BTreeSet::new(),
        }
    }

    /// Held under backpressure, delivered when there is room: a
    /// discovery the outbox could not take is NOT lost (#111 review F1),
    /// and taking it empties the hold.
    #[test]
    fn a_discovery_the_outbox_could_not_take_is_held_and_delivered() {
        let a = to_transport_identity(&peer()).expect("canonical");
        let mut state = MdnsState::new();
        assert!(!state.holds_anything(), "nothing held to begin with");

        state.hold_discovered(vec![candidate(&a, &["/ip4/8.8.8.8/tcp/1"])]);
        assert!(state.holds_anything());
        let (discovered, expired) = state.take_held(7);
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].peer_id, a);
        assert!(discovered[0].addresses.contains("/ip4/8.8.8.8/tcp/1"));
        assert_eq!(discovered[0].observed_at, 7, "stamped when delivered");
        assert!(expired.is_empty());
        assert!(!state.holds_anything(), "taking empties the hold");
    }

    /// A pair discovered and then retracted while held nets to the
    /// retraction: the discovery is cancelled, the expiry kept -- kept
    /// because the provider may hold that pair from an EARLIER delivery.
    #[test]
    fn a_held_discovery_then_expiry_nets_to_the_expiry() {
        let a = to_transport_identity(&peer()).expect("canonical");
        let mut state = MdnsState::new();
        state.hold_discovered(vec![candidate(&a, &["/ip4/8.8.8.8/tcp/1"])]);
        state.hold_expired(vec![(a.clone(), "/ip4/8.8.8.8/tcp/1".to_owned())]);
        let (discovered, expired) = state.take_held(0);
        assert!(
            discovered.is_empty(),
            "the held discovery is cancelled by the later retraction"
        );
        assert_eq!(expired, vec![(a, "/ip4/8.8.8.8/tcp/1".to_owned())]);
    }

    /// And the reverse: retracted, then rediscovered while held, nets to
    /// the discovery. Without this cancellation the two would be
    /// delivered together and the retraction could undo the rediscovery.
    #[test]
    fn a_held_expiry_then_rediscovery_nets_to_the_discovery() {
        let a = to_transport_identity(&peer()).expect("canonical");
        let mut state = MdnsState::new();
        state.hold_expired(vec![(a.clone(), "/ip4/8.8.8.8/tcp/1".to_owned())]);
        state.hold_discovered(vec![candidate(&a, &["/ip4/8.8.8.8/tcp/1"])]);
        let (discovered, expired) = state.take_held(0);
        assert!(
            expired.is_empty(),
            "the rediscovery cancels the held retraction"
        );
        assert_eq!(discovered.len(), 1);
    }

    /// #111 mDNS review F8, fed the shape the crate actually reports: a
    /// discovery and a retraction of the SAME suffixed pair emit the same
    /// bare route, so the provider sees one key at both ends. A foreign
    /// suffix is dropped at both. The bare pair beside it is the control
    /// that the route, not the spelling, is what is emitted.
    #[test]
    fn the_crates_peer_suffix_is_stripped_at_both_ends() {
        let libp2p_peer = peer();
        let other = peer();
        let route = "/ip4/8.8.8.8/tcp/4001";
        let suffixed: Multiaddr = format!("{route}/p2p/{libp2p_peer}").parse().expect("valid");
        let foreign: Multiaddr = format!("/ip4/1.1.1.1/tcp/1/p2p/{other}")
            .parse()
            .expect("valid");
        let bare: Multiaddr = "/ip4/9.9.9.9/tcp/1".parse().expect("valid");
        let pairs = [
            (libp2p_peer, suffixed.clone()),
            (libp2p_peer, foreign.clone()),
            (libp2p_peer, bare.clone()),
        ];

        let mut state = MdnsState::new();
        let found = state.on_discovered(&pairs, std::iter::empty::<&str>(), 0);
        assert_eq!(
            found[0].addresses,
            BTreeSet::from([route.to_owned(), bare.to_string()])
        );
        let retracted: BTreeSet<String> = state
            .on_expired(&pairs)
            .into_iter()
            .map(|(_, address)| address)
            .collect();
        assert_eq!(retracted, found[0].addresses, "one key at both ends");
    }

    /// The query interval's floor, with the floor itself as the control.
    #[test]
    fn a_query_interval_below_one_second_is_refused() {
        let at = |query_interval_ms| MdnsSettings {
            ttl_ms: 10 * MIN_QUERY_INTERVAL_MS,
            query_interval_ms,
            enable_ipv6: false,
        };
        assert!(at(1).validate().is_err());
        assert!(at(MIN_QUERY_INTERVAL_MS - 1).validate().is_err());
        assert!(at(MIN_QUERY_INTERVAL_MS).validate().is_ok());
    }

    /// ADR-0053 rule 3 (#112 blind review F6): an interval at or past the
    /// 120 s record clamp is refused even when it is below this node's own
    /// announced TTL, and the default sits below it. The interval just
    /// under the clamp is the control.
    #[test]
    fn a_query_interval_at_the_record_clamp_is_refused_and_the_default_is_below_it() {
        let clamp = u64::try_from(MAX_RECORD_TTL.as_millis()).expect("fits");
        let at = |query_interval_ms| MdnsSettings {
            ttl_ms: 6 * 60 * 1000,
            query_interval_ms,
            enable_ipv6: false,
        };
        assert!(at(clamp).validate().is_err(), "at the clamp");
        assert!(
            at(5 * 60 * 1000).validate().is_err(),
            "the crate's own 5 min"
        );
        assert!(
            at(clamp - 1).validate().is_err(),
            "just under the clamp, but the jitter can take it there (#112)"
        );
        assert!(
            at(clamp - QUERY_JITTER_MAX_MS).validate().is_err(),
            "the largest jittered interval would reach the clamp"
        );
        assert!(
            MdnsSettings {
                ttl_ms: u64::MAX,
                query_interval_ms: u64::MAX - 1,
                enable_ipv6: false,
            }
            .validate()
            .is_err(),
            "an interval near u64::MAX is refused, not an overflow"
        );
        assert!(
            at(clamp - QUERY_JITTER_MAX_MS - 1).validate().is_ok(),
            "the control: the largest interval whose jitter stays under it"
        );
        assert!(MdnsSettings::default().query_interval_ms < clamp);
        assert!(MdnsSettings::default().validate().is_ok());
    }

    /// `MAX_ADDRESSES + 1` distinct public addresses for one peer.
    fn one_too_many() -> Vec<String> {
        (0..=interweave_discovery_api::MAX_ADDRESSES)
            .map(|i| format!("/ip4/8.8.{}.{}/tcp/1", i / 256, i % 256))
            .collect()
    }

    /// #111 mDNS review F5/F6: the PER-PEER address bound, at each of the
    /// four places it applies, each dropping exactly the one address
    /// past it and counting it. A duplicate of an address already taken
    /// is not a drop -- the control that the count is the bound's.
    #[test]
    fn every_per_peer_address_bound_keeps_the_bound_and_counts_the_rest() {
        let max = interweave_discovery_api::MAX_ADDRESSES;
        let libp2p_peer = peer();
        let a = to_transport_identity(&libp2p_peer).expect("canonical");
        let addresses = one_too_many();

        // A discovery.
        let mut state = MdnsState::new();
        let pairs: Vec<(PeerId, Multiaddr)> = addresses
            .iter()
            .chain(std::iter::once(&addresses[0]))
            .map(|x| (libp2p_peer, x.parse().expect("valid")))
            .collect();
        let found = state.on_discovered(&pairs, std::iter::empty::<&str>(), 0);
        assert_eq!(found[0].addresses.len(), max, "discovery");
        assert_eq!(state.counters().over_peer_bound, 1, "discovery");

        // A retraction.
        let mut state = MdnsState::new();
        let pairs: Vec<(PeerId, Multiaddr)> = addresses
            .iter()
            .map(|x| (libp2p_peer, x.parse().expect("valid")))
            .collect();
        assert_eq!(state.on_expired(&pairs).len(), max, "retraction");
        assert_eq!(state.counters().over_peer_bound, 1, "retraction");

        // The discovery hold, across two candidates for one peer.
        let mut state = MdnsState::new();
        let first: Vec<&str> = addresses[..max].iter().map(String::as_str).collect();
        state.hold_discovered(vec![candidate(&a, &first)]);
        state.hold_discovered(vec![candidate(&a, &[&addresses[max], &addresses[0]])]);
        let (held, _) = state.take_held(0);
        assert_eq!(held[0].addresses.len(), max, "discovery hold");
        assert_eq!(state.counters().over_peer_bound, 1, "discovery hold");

        // The retraction hold.
        let mut state = MdnsState::new();
        state.hold_expired(
            addresses
                .iter()
                .chain(std::iter::once(&addresses[0]))
                .map(|x| (a.clone(), x.clone()))
                .collect(),
        );
        let (_, held) = state.take_held(0);
        assert_eq!(held.len(), max, "retraction hold");
        assert_eq!(state.counters().over_peer_bound, 1, "retraction hold");
    }

    /// The retraction hold's PEER bound, which only its address half had
    /// a sibling for (#111 mDNS review F5).
    #[test]
    fn the_retraction_hold_takes_no_more_peers_than_a_batch() {
        let mut state = MdnsState::new();
        state.hold_expired(
            (0..=MAX_PEERS_PER_BATCH)
                .map(|_| {
                    (
                        to_transport_identity(&peer()).expect("canonical"),
                        "/ip4/8.8.8.8/tcp/1".to_owned(),
                    )
                })
                .collect(),
        );
        let (_, held) = state.take_held(0);
        assert_eq!(held.len(), MAX_PEERS_PER_BATCH);
        assert_eq!(state.counters().over_peer_bound, 1);
    }

    /// Bounded: the hold takes no more peers than one batch may carry,
    /// and counts what it could not take rather than dropping it
    /// silently.
    #[test]
    fn the_hold_is_bounded_like_a_batch_and_counts_its_overflow() {
        let over = 5;
        let candidates: Vec<_> = (0..MAX_PEERS_PER_BATCH + over)
            .map(|_| {
                candidate(
                    &to_transport_identity(&peer()).expect("canonical"),
                    &["/ip4/8.8.8.8/tcp/1"],
                )
            })
            .collect();
        let mut state = MdnsState::new();
        state.hold_discovered(candidates);
        let (discovered, _) = state.take_held(0);
        assert_eq!(discovered.len(), MAX_PEERS_PER_BATCH);
        assert_eq!(state.counters().over_peer_bound, over);
    }

    /// The membership clause, fed the one shape it exists for: the batch
    /// is already FULL of distinct peers, and then one of them announces
    /// a second address. That address must be taken, not dropped and
    /// counted as a flood.
    ///
    /// Delete `!by_peer.contains_key(&identity) &&` and this fails: the
    /// second address meets a full batch, is dropped, and is counted
    /// `over_peer_bound`. An earlier version fed ONE peer many addresses,
    /// which never nears a peer bound either way, and so it passed with
    /// the clause removed (#111 re-review P2-4).
    #[test]
    fn a_full_batch_still_takes_another_address_for_a_peer_it_holds() {
        let peers: Vec<PeerId> = (0..MAX_PEERS_PER_BATCH).map(|_| peer()).collect();
        let mut pairs: Vec<(PeerId, Multiaddr)> = peers
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let port = 4001 + u16::try_from(i).expect("port fits");
                (
                    *p,
                    format!("/ip4/8.8.8.8/tcp/{port}")
                        .parse()
                        .expect("a global literal the floor admits"),
                )
            })
            .collect();
        // The batch is now full. THE SHAPE: a second address for the
        // FIRST peer it holds.
        let first = peers[0];
        pairs.push((
            first,
            "/ip4/8.8.4.4/tcp/4001"
                .parse()
                .expect("a global literal the floor admits"),
        ));

        let mut state = MdnsState::new();
        let candidates = state.on_discovered(&pairs, ["/ip4/0.0.0.0/tcp/1"], 0);

        assert_eq!(
            candidates.len(),
            MAX_PEERS_PER_BATCH,
            "still exactly the bound"
        );
        let first_id = to_transport_identity(&first).expect("canonical");
        let first_addresses = candidates
            .iter()
            .find(|c| c.peer_id == first_id)
            .map(|c| c.addresses.len());
        assert_eq!(
            first_addresses,
            Some(2),
            "a peer already in the batch keeps taking addresses when the batch is full"
        );
        assert_eq!(
            state.counters().over_peer_bound,
            0,
            "and nothing was dropped for the peer bound: no NEW peer asked for a slot"
        );
    }

    /// A retraction batch is bounded too, and for the size of the
    /// emitted batch rather than on class -- `an_expiry_is_never_refused_on_class`
    /// is the sibling that pins the class half stays open.
    #[test]
    fn a_flood_of_expiries_stops_at_the_batch_bound() {
        let over = 40;
        let pairs: Vec<(PeerId, Multiaddr)> = (0..MAX_PEERS_PER_BATCH + over)
            .map(|_| {
                (
                    peer(),
                    "/ip4/127.0.0.1/tcp/4001"
                        .parse()
                        .expect("a loopback the floor would refuse on discovery"),
                )
            })
            .collect();

        let mut state = MdnsState::new();
        let expired = state.on_expired(&pairs);

        assert_eq!(expired.len(), MAX_PEERS_PER_BATCH);
        assert_eq!(state.counters().over_peer_bound, over);
    }

    /// THE RE-REVIEW'S COUNTER-EXAMPLE (P3-7): 200 peers on two addresses
    /// each is well inside what a discovery batch admits, so all 400
    /// retractions must go out in one batch. ONE case, not the general
    /// claim its old name made -- a batch mixing class-refused pairs can
    /// crowd an admitted retraction out (#111 mDNS review F6). The first version bounded
    /// retractions by PAIRS at the peer bound, kept 256, and counted 144
    /// as over a peer bound no peer had exceeded.
    #[test]
    fn two_hundred_peers_on_two_addresses_retract_in_one_batch() {
        let peers = 200;
        let pairs: Vec<(PeerId, Multiaddr)> = (0..peers)
            .flat_map(|i| {
                let p = peer();
                let base = 4001 + 2 * u16::try_from(i).expect("fits");
                [base, base + 1].map(|port| {
                    (
                        p,
                        format!("/ip4/8.8.8.8/tcp/{port}").parse().expect("valid"),
                    )
                })
            })
            .collect();

        let mut state = MdnsState::new();
        let expired = state.on_expired(&pairs);

        assert_eq!(expired.len(), 2 * peers, "every retraction goes out");
        assert_eq!(state.counters().over_peer_bound, 0, "no bound was met");
    }

    fn addr(text: &str) -> Multiaddr {
        text.parse().expect("address")
    }

    #[test]
    fn settings_refuse_a_query_interval_that_outlives_the_record() {
        // A provider that forgets itself: records lapse before the next
        // query refreshes them, so a peer that never left is announced,
        // expired and re-announced on the interval.
        assert!(
            MdnsSettings {
                ttl_ms: 1_000,
                query_interval_ms: 1_000,
                enable_ipv6: false,
            }
            .validate()
            .is_err()
        );
        assert!(MdnsSettings::default().validate().is_ok());
        assert!(
            MdnsSettings {
                ttl_ms: 0,
                ..MdnsSettings::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn a_global_candidate_is_admitted_and_a_special_use_one_never_becomes_an_observation() {
        // The floor, at the learn site. The refused address does not
        // reach the candidate list at all -- not flagged, not carried.
        let mut state = MdnsState::new();
        let (a, b) = (peer(), peer());
        let no_listeners: [&str; 0] = [];
        let out = state.on_discovered(
            &[
                (a, addr("/ip4/8.8.8.8/tcp/4001")),
                (b, addr("/ip4/127.0.0.1/tcp/4001")),
            ],
            no_listeners,
            77,
        );
        assert_eq!(out.len(), 1, "only the global candidate survives");
        assert_eq!(out[0].source, "mdns");
        assert_eq!(out[0].observed_at, 77);
        assert_eq!(state.counters().admitted, 1);
        assert_eq!(
            state.counters().refused.get("special_use").copied(),
            Some(1)
        );
    }

    #[test]
    fn a_private_candidate_needs_a_private_listener_of_its_family() {
        // providers/mdns.md's rule 3 instance, which is what makes LAN
        // discovery work at all on an RFC 1918 network -- and what stops
        // a node with no LAN interface from being handed one.
        let lan = peer();
        let mut without = MdnsState::new();
        let none: [&str; 0] = [];
        assert!(
            without
                .on_discovered(&[(lan, addr("/ip4/192.168.1.7/tcp/4001"))], none, 1)
                .is_empty()
        );
        assert_eq!(
            without
                .counters()
                .refused
                .get("private_without_private_listener")
                .copied(),
            Some(1)
        );

        let mut with = MdnsState::new();
        let out = with.on_discovered(
            &[(lan, addr("/ip4/192.168.1.7/tcp/4001"))],
            ["/ip4/192.168.1.20/tcp/4001"],
            1,
        );
        assert_eq!(out.len(), 1, "the same candidate, beside a LAN listener");
        assert_eq!(with.counters().refused_total(), 0);
    }

    #[test]
    fn the_link_local_cost_the_provider_document_records() {
        // An IPv6 link-local-only LAN yields no dialable candidate, and
        // that is the floor working rather than a gap: an announcement
        // naming fe80:: from an untrusted multicast domain cannot be
        // told from one probing this host's own interfaces. The document
        // costs this out; this is the assertion behind it.
        let mut state = MdnsState::new();
        let out = state.on_discovered(
            &[(peer(), addr("/ip6/fe80::1/tcp/4001"))],
            ["/ip6/fe80::20/tcp/4001"],
            1,
        );
        assert!(out.is_empty());
        assert_eq!(
            state.counters().refused.get("special_use").copied(),
            Some(1)
        );
    }

    #[test]
    fn an_expiry_is_never_refused_on_class() {
        // A retraction for an address the floor would refuse must still
        // reach the provider: dropping it would strand whatever the
        // provider holds, since the only event that could clear it is
        // the one being dropped. The floor decides what may be DIALLED,
        // not what may be forgotten.
        let mut state = MdnsState::new();
        let gone = peer();
        let out = state.on_expired(&[
            (gone, addr("/ip4/127.0.0.1/tcp/4001")),
            (gone, addr("/ip4/8.8.8.8/tcp/4001")),
        ]);
        assert_eq!(out.len(), 2, "both retractions pass, class notwithstanding");
    }

    #[test]
    fn one_peer_announcing_twice_is_one_candidate_with_two_addresses() {
        // The crate reports pairs, not peers. Grouping here is what
        // keeps a peer on two interfaces from arriving as two candidates
        // that the pipeline then has to reconcile.
        let both = peer();
        let mut state = MdnsState::new();
        let out = state.on_discovered(
            &[
                (both, addr("/ip4/8.8.8.8/tcp/4001")),
                (both, addr("/ip4/1.1.1.1/tcp/4001")),
            ],
            [] as [&str; 0],
            5,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].addresses.len(), 2);
    }

    #[test]
    fn a_peer_past_the_address_bound_stops_at_the_bound() {
        // Remote-authored input held to the discovery contract's bounds
        // WHILE being read, so nothing oversized reaches the
        // accumulator.
        let noisy = peer();
        let mut state = MdnsState::new();
        let pairs: Vec<(PeerId, Multiaddr)> = (0..interweave_discovery_api::MAX_ADDRESSES + 5)
            .map(|i| {
                (
                    noisy,
                    addr(&format!("/ip4/8.8.{}.{}/tcp/4001", i / 200, (i % 200) + 1)),
                )
            })
            .collect();
        let out = state.on_discovered(&pairs, [] as [&str; 0], 1);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].addresses.len(),
            interweave_discovery_api::MAX_ADDRESSES
        );
    }
}
