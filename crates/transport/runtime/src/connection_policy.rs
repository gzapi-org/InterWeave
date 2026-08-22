// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `DialAdmissionGate` decisions, connection classes, and the two failure
//! scopes.
//!
//! Pure decision logic. No dialing happens here — this answers *may this
//! dial proceed*, and the backend that asks owns the doing.
//!
//! # Every outbound dial, not merely the scheduled ones
//!
//! A libp2p `NetworkBehaviour` can request a dial while driving its own
//! protocol; Kademlia's iterative queries do exactly that. So "the
//! provider does not call the dial scheduler" is not enough, and
//! [`DialRequest`] carries a [`DialOrigin`] that has no exempt value. A
//! behaviour-originated dial goes through the same gate as any other
//! (ADR-0011).
//!
//! # Two failure scopes, kept apart
//!
//! [`AddressState`] and [`PeerBackoff`] are separate on purpose. An
//! attacker who injects one bogus address for a trusted peer must not be
//! able to turn that address's failures into peer-wide punitive backoff
//! while a known-good route still exists. Merging the two counters is
//! precisely how that attack would succeed.
//!
//! Address state is keyed by **(peer, address)**, not by address alone.
//! A bare address key looks tidier and is wrong in both directions: one
//! peer's success at some address would spare a *different* peer from
//! backoff forever, and an identity mismatch is a fact about the
//! address-claims-to-be-this-peer mapping rather than about the address
//! in general.

use std::collections::BTreeMap;

use interweave_transport_api::TransportIdentity;

/// Default quarantine for an address that authenticated the wrong PeerId.
pub const IDENTITY_MISMATCH_QUARANTINE_MS: u64 = 30 * 60 * 1000;

/// Maximum retained `(peer, address)` state entries.
///
/// Generous, because the cost of an entry is small and the cost of
/// forgetting a quarantine is not. The bound exists so the map cannot
/// grow with the number of addresses an adversary can name — CLAUDE.md
/// section 6 requires every map here to be bounded, and this one was not.
pub const DEFAULT_MAX_ADDRESS_ENTRIES: usize = 8_192;

/// Maximum retained peer-backoff entries.
///
/// Matches the configuration ceiling on allowed peers: a peer must be
/// authorized before it can be dialed at all, so the number of peers that
/// can ever be in backoff is bounded by the allowlist. Aligning the two
/// means this cap is only reached in a configuration that was already at
/// its own limit.
pub const DEFAULT_MAX_PEER_ENTRIES: usize = 4_096;

/// How long a non-punitive entry survives untouched.
///
/// One hour. Long enough to keep "known-good" useful across a normal
/// session, short enough that a peer set churning through addresses does
/// not accumulate them forever.
pub const DEFAULT_IDLE_TTL_MS: u64 = 60 * 60 * 1000;

/// What a peer is authorized to do, computed locally.
///
/// Not a spectrum. `ConnectivityInfrastructureOnly` is emphatically not
/// "a bit less than trusted": it permits reachability-control protocols
/// and nothing else — no GossipSub, no direct v2, no endpoint directory,
/// no Kademlia routing (ADR-0036).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionClass {
    /// Authorized for the application data plane.
    DataPlaneTrusted,
    /// Authorized only for reachability control.
    ConnectivityInfrastructureOnly,
    /// Not authorized for anything.
    Unauthorized,
}

/// Why a dial was requested.
///
/// There is deliberately **no** `Exempt` or `Internal` variant. Every
/// origin is attributable and every origin is gated; a value meaning
/// "skip the gate" would recreate the hole the gate exists to close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialOrigin {
    /// A person or an admin API asked for this exact peer.
    ///
    /// Distinct from [`Self::ConnectionManager`] because the roadmap
    /// requires every origin to be representable AND observable: folding
    /// a human's explicit request into the scheduler's own dials means a
    /// denial cannot say which of the two it refused, and those are the
    /// two a person most needs told apart.
    Manual,
    /// The ordinary candidate dial scheduler.
    ConnectionManager,
    /// Re-establishing a peer a discovery provider reported.
    ///
    /// Advisory input, never authority: a candidate is a reason to
    /// consider dialling and nothing more, and naming the origin is what
    /// lets the gate — and anything reading its decisions — see how much
    /// of the dial volume is discovery-driven.
    DiscoveryReconnect,
    /// A Kademlia iterative query asked the Swarm to dial.
    KademliaQuery,
    /// Establishing or renewing a relay reservation.
    RelayReservation,
    /// Opening a relayed circuit.
    RelayCircuit,
    /// An AutoNAT probe.
    AutonatProbe,
    /// A DCUtR hole-punch attempt.
    DcutrHolePunch,
}

impl DialOrigin {
    /// Whether this origin is application data-plane traffic.
    ///
    /// The reachability origins are not, which is what lets an
    /// infrastructure-only peer be dialed for a relay reservation while
    /// staying unauthorized for a Kademlia query to the same address.
    #[must_use]
    pub const fn is_data_plane(self) -> bool {
        matches!(
            self,
            Self::Manual | Self::ConnectionManager | Self::DiscoveryReconnect | Self::KademliaQuery
        )
    }

    /// Every origin, so an exhaustive check cannot silently miss one.
    ///
    /// A test that lists origins by hand proves what its author
    /// remembered. Adding a variant without adding it here fails to
    /// compile, which is the property worth having when the rule being
    /// tested is "no origin skips the gate".
    pub const ALL: [Self; 8] = [
        Self::Manual,
        Self::ConnectionManager,
        Self::DiscoveryReconnect,
        Self::KademliaQuery,
        Self::RelayReservation,
        Self::RelayCircuit,
        Self::AutonatProbe,
        Self::DcutrHolePunch,
    ];
}

/// One dial the gate is asked to admit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialRequest {
    /// The intended peer, when known.
    ///
    /// `None` for a dial to an address whose identity has not been
    /// established. Such a dial is still gated — on limits and drain
    /// state — because an unauthenticated dial still consumes resources.
    pub peer: Option<TransportIdentity>,
    /// The normalized address being dialed.
    pub address: String,
    /// Why the dial was requested.
    pub origin: DialOrigin,
}

/// Why the gate refused.
///
/// Ordered as the gate evaluates them, and each is distinct because an
/// operator debugging a connection needs to know which bound they hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialDenial {
    /// The runtime is draining or shutting down.
    ShuttingDown,
    /// The peer is not authorized for anything.
    Unauthorized,
    /// The peer is authorized for reachability only, and this is data-plane.
    NotAuthorizedForDataPlane,
    /// The peer is in punitive backoff.
    PeerBackoff,
    /// This address is quarantined after an identity mismatch.
    AddressQuarantined,
    /// The global pending-dial budget is exhausted.
    TooManyPendingDials,
    /// The global connection budget is exhausted.
    ConnectionLimitReached,
    /// Policy state is full of live suppressions and cannot take more.
    ///
    /// Fails CLOSED. The alternative is evicting a live quarantine to
    /// make room, which is attacker-controlled: anyone able to provoke
    /// evictions could flood the table to clear their own. A table that
    /// is entirely live suppressions already describes a hostile peer
    /// set, so denying is the honest answer.
    PolicyStateFull,
}

/// Address-scoped reachability and authentication state.
///
/// Kept separate from [`PeerBackoff`] deliberately — see the module note.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AddressState {
    /// Consecutive failures for this address.
    pub consecutive_failures: u32,
    /// When this entry was last written.
    ///
    /// Drives pruning. Without it the map has no notion of an entry that
    /// stopped mattering, and "bounded" would depend on the peer set
    /// never changing.
    pub last_touched_ms: u64,
    /// When this address last authenticated successfully.
    ///
    /// The field that makes "prefer known-good" possible: an address that
    /// has worked once is materially different from one that never has.
    pub last_success_ms: Option<u64>,
    /// Quarantined until this time, after an identity mismatch.
    pub quarantined_until_ms: Option<u64>,
}

impl AddressState {
    /// Whether this address may be dialed at `now_ms`.
    #[must_use]
    pub fn is_dialable_at(&self, now_ms: u64) -> bool {
        self.quarantined_until_ms
            .is_none_or(|until| now_ms >= until)
    }

    /// Whether this address has ever authenticated successfully.
    #[must_use]
    pub const fn is_known_good(&self) -> bool {
        self.last_success_ms.is_some()
    }

    /// Whether this entry is currently PROTECTING something.
    ///
    /// A live quarantine, or a failure count that is shaping retries.
    /// Such an entry must never be evicted to make room: dropping it
    /// restores a route the policy had decided to suppress, and an
    /// attacker who can cause evictions could then clear their own
    /// quarantine by flooding the table.
    #[must_use]
    pub fn is_punitive_at(&self, now_ms: u64) -> bool {
        self.consecutive_failures > 0
            || self
                .quarantined_until_ms
                .is_some_and(|until| now_ms < until)
    }
}

/// Peer-scoped punitive backoff.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeerBackoff {
    /// Consecutive peer-scoped failures.
    pub consecutive_failures: u32,
    /// When this entry was last written.
    pub last_touched_ms: u64,
    /// In backoff until this time.
    pub until_ms: Option<u64>,
}

impl PeerBackoff {
    /// Whether the peer may be dialed at `now_ms`.
    #[must_use]
    pub fn is_clear_at(&self, now_ms: u64) -> bool {
        self.until_ms.is_none_or(|until| now_ms >= until)
    }

    /// Whether this entry is currently suppressing dials.
    #[must_use]
    pub fn is_punitive_at(&self, now_ms: u64) -> bool {
        !self.is_clear_at(now_ms)
    }
}

/// The key for address state: which peer this address was dialed as.
///
/// Keyed by the pair rather than by the address alone, so one peer's
/// history cannot answer questions about another's.
type AddressKey = (TransportIdentity, String);

/// The atomically readable policy snapshot the gate consults.
///
/// A snapshot rather than a live query because the gate runs synchronously
/// inside the Swarm poll: it must not block on an async policy call while
/// the Swarm is being driven (ADR-0011).
#[derive(Debug, Clone)]
pub struct ConnectionPolicy {
    addresses: BTreeMap<AddressKey, AddressState>,
    peers: BTreeMap<TransportIdentity, PeerBackoff>,
    /// Maximum address entries retained.
    pub max_addresses: usize,
    /// Maximum peer-backoff entries retained.
    pub max_peers: usize,
    /// How long a non-punitive entry survives without being touched.
    pub idle_ttl_ms: u64,
    /// Currently pending dials.
    pub pending_dials: usize,
    /// Currently established connections.
    pub connections: usize,
    /// Maximum pending dials.
    pub max_pending_dials: usize,
    /// Maximum established connections.
    pub max_connections: usize,
    /// Whether the runtime is draining.
    pub shutting_down: bool,
}

impl Default for ConnectionPolicy {
    fn default() -> Self {
        Self {
            addresses: BTreeMap::new(),
            peers: BTreeMap::new(),
            max_addresses: DEFAULT_MAX_ADDRESS_ENTRIES,
            max_peers: DEFAULT_MAX_PEER_ENTRIES,
            idle_ttl_ms: DEFAULT_IDLE_TTL_MS,
            pending_dials: 0,
            connections: 0,
            max_pending_dials: 0,
            max_connections: 0,
            shutting_down: false,
        }
    }
}

impl ConnectionPolicy {
    /// Build a policy with explicit limits.
    #[must_use]
    pub fn new(max_pending_dials: usize, max_connections: usize) -> Self {
        Self {
            max_pending_dials,
            max_connections,
            max_addresses: DEFAULT_MAX_ADDRESS_ENTRIES,
            max_peers: DEFAULT_MAX_PEER_ENTRIES,
            idle_ttl_ms: DEFAULT_IDLE_TTL_MS,
            ..Self::default()
        }
    }

    /// Drop entries that are neither punitive nor recently used.
    ///
    /// Returns how many were dropped. Separate from every decision path,
    /// because a read that mutated would make the policy's answer depend
    /// on how often it was asked.
    ///
    /// A punitive entry is NEVER dropped here regardless of age: its
    /// whole purpose is to outlive the traffic that created it.
    pub fn prune(&mut self, now_ms: u64) -> usize {
        let ttl = self.idle_ttl_ms;
        let idle = |touched: u64| now_ms.saturating_sub(touched) >= ttl;

        let before = self.addresses.len() + self.peers.len();
        self.addresses
            .retain(|_, s| s.is_punitive_at(now_ms) || !idle(s.last_touched_ms));
        self.peers
            .retain(|_, b| b.is_punitive_at(now_ms) || !idle(b.last_touched_ms));
        before - (self.addresses.len() + self.peers.len())
    }

    /// How many address entries are currently held.
    #[must_use]
    pub fn address_entries(&self) -> usize {
        self.addresses.len()
    }

    /// How many peer-backoff entries are currently held.
    #[must_use]
    pub fn peer_entries(&self) -> usize {
        self.peers.len()
    }

    /// Make room for one more address entry, or report that there is none.
    ///
    /// # Eviction can never clear a quarantine
    ///
    /// Only non-punitive entries are candidates, least recently touched
    /// first. If every entry is punitive the table is FULL and this
    /// returns false — the caller then denies the dial rather than
    /// forgetting a suppression.
    ///
    /// That direction is deliberate. Forgetting is attacker-controlled:
    /// anyone who can provoke evictions could flood the table to clear
    /// their own quarantine, which turns a bounded map into a way to
    /// launder a failed identity check. Denying is at worst self-
    /// inflicted, and a table consisting entirely of live suppressions is
    /// already a description of a hostile peer set.
    fn make_room_for_address(&mut self, now_ms: u64) -> bool {
        if self.addresses.len() < self.max_addresses {
            return true;
        }
        let victim = self
            .addresses
            .iter()
            .filter(|(_, s)| !s.is_punitive_at(now_ms))
            .min_by_key(|(_, s)| s.last_touched_ms)
            .map(|(k, _)| k.clone());
        match victim {
            Some(key) => {
                self.addresses.remove(&key);
                true
            }
            None => false,
        }
    }

    fn make_room_for_peer(&mut self, now_ms: u64) -> bool {
        if self.peers.len() < self.max_peers {
            return true;
        }
        let victim = self
            .peers
            .iter()
            .filter(|(_, b)| !b.is_punitive_at(now_ms))
            .min_by_key(|(_, b)| b.last_touched_ms)
            .map(|(k, _)| k.clone());
        match victim {
            Some(key) => {
                self.peers.remove(&key);
                true
            }
            None => false,
        }
    }

    /// The state of one address as dialed for one peer.
    #[must_use]
    pub fn address(&self, peer: &TransportIdentity, address: &str) -> Option<&AddressState> {
        self.addresses.get(&(peer.clone(), address.to_owned()))
    }

    /// The backoff state of one peer.
    #[must_use]
    pub fn peer(&self, peer: &TransportIdentity) -> Option<&PeerBackoff> {
        self.peers.get(peer)
    }

    /// Decide whether a dial may proceed.
    ///
    /// Evaluated in the order ADR-0011 states: drain, then class and
    /// origin, then peer backoff, then address quarantine, then limits.
    /// The order is observable — the denial an operator sees should be
    /// the most fundamental reason, not whichever check ran first.
    ///
    /// # Errors
    /// Returns the [`DialDenial`] that stopped it.
    pub fn admit(
        &self,
        request: &DialRequest,
        class: ConnectionClass,
        now_ms: u64,
    ) -> Result<(), DialDenial> {
        if self.shutting_down {
            return Err(DialDenial::ShuttingDown);
        }

        match class {
            ConnectionClass::Unauthorized => return Err(DialDenial::Unauthorized),
            ConnectionClass::ConnectivityInfrastructureOnly => {
                // The rule ADR-0036 exists for: an infrastructure peer is
                // dialable for reachability and refused for the data
                // plane, on the same address, in the same moment.
                if request.origin.is_data_plane() {
                    return Err(DialDenial::NotAuthorizedForDataPlane);
                }
            }
            ConnectionClass::DataPlaneTrusted => {}
        }

        if let Some(peer) = &request.peer
            && let Some(backoff) = self.peers.get(peer)
            && !backoff.is_clear_at(now_ms)
        {
            return Err(DialDenial::PeerBackoff);
        }

        if let Some(peer) = &request.peer
            && let Some(state) = self.addresses.get(&(peer.clone(), request.address.clone()))
            && !state.is_dialable_at(now_ms)
        {
            return Err(DialDenial::AddressQuarantined);
        }

        if self.pending_dials >= self.max_pending_dials {
            return Err(DialDenial::TooManyPendingDials);
        }
        if self.connections >= self.max_connections {
            return Err(DialDenial::ConnectionLimitReached);
        }

        // A dial whose outcome could not be RECORDED is a dial whose
        // failure cannot suppress a retry, so admitting it would turn the
        // capacity bound into a way to dial without accounting. Only
        // relevant when this (peer, address) has no entry yet and nothing
        // benign can be evicted to make one.
        if let Some(peer) = &request.peer {
            let key = (peer.clone(), request.address.clone());
            if !self.addresses.contains_key(&key)
                && self.addresses.len() >= self.max_addresses
                && !self.addresses.values().any(|s| !s.is_punitive_at(now_ms))
            {
                return Err(DialDenial::PolicyStateFull);
            }
        }
        Ok(())
    }

    /// Record a successful authenticated connection.
    ///
    /// Clears this address's failures and the peer's punitive state. It
    /// does **not** rehabilitate other quarantined addresses: one working
    /// route says nothing about an address that authenticated the wrong
    /// identity.
    pub fn record_success(&mut self, peer: &TransportIdentity, address: &str, now_ms: u64) {
        let key = (peer.clone(), address.to_owned());
        if !self.addresses.contains_key(&key) && !self.make_room_for_address(now_ms) {
            // Nothing evictable. A success is not worth denying over, so
            // it is simply not recorded — the address stays un-preferred
            // rather than the table forgetting a quarantine.
            self.peers.remove(peer);
            return;
        }
        let entry = self.addresses.entry(key).or_default();
        entry.consecutive_failures = 0;
        entry.last_success_ms = Some(now_ms);
        entry.quarantined_until_ms = None;
        entry.last_touched_ms = now_ms;
        self.peers.remove(peer);
    }

    /// Record an address-scoped failure.
    ///
    /// Returns whether the failure also advanced peer-level backoff. It
    /// does so only when **no other eligible known-good address remains**:
    /// while one does, the problem is demonstrably the address, and
    /// punishing the peer would suppress a route that works.
    pub fn record_address_failure(
        &mut self,
        peer: &TransportIdentity,
        address: &str,
        now_ms: u64,
        backoff_ms: u64,
    ) -> bool {
        let key = (peer.clone(), address.to_owned());
        // A failure that cannot be recorded is worse than one that can:
        // it is the entry that would have suppressed a retry. Prune
        // first, then evict a benign entry to hold it.
        //
        // But when there is nothing evictable the answer is to NOT
        // record it. Inserting anyway — which is what discarding this
        // result did — grows a map whose whole purpose is being bounded:
        // enough concurrent failures fill the table with live punitive
        // entries, after which every further failed address is appended
        // without limit. The peer branch below already refuses on the
        // same terms; this one only looked like it did.
        let room = self.addresses.contains_key(&key) || {
            self.prune(now_ms);
            self.make_room_for_address(now_ms)
        };
        if room {
            let entry = self.addresses.entry(key).or_default();
            entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
            entry.last_touched_ms = now_ms;
        }

        // Scoped to THIS peer. A global scan would let any unrelated
        // peer's past success spare this one from backoff indefinitely,
        // removing retry protection exactly where it is needed.
        let alternative_exists = self.addresses.iter().any(|((p, a), s)| {
            p == peer && a != address && s.is_known_good() && s.is_dialable_at(now_ms)
        });
        if alternative_exists {
            return false;
        }
        if !self.peers.contains_key(peer) && !self.make_room_for_peer(now_ms) {
            // Every entry is a live backoff and there is no room. Report
            // truthfully that peer backoff did NOT advance rather than
            // growing the map: the return value is what the caller uses
            // to decide whether the peer is now suppressed.
            //
            // Losing a backoff that was never created is bounded harm —
            // that peer keeps being retried. Evicting a live one is not:
            // it restores a peer the policy had decided to suppress.
            return false;
        }
        let b = self.peers.entry(peer.clone()).or_default();
        b.consecutive_failures = b.consecutive_failures.saturating_add(1);
        b.until_ms = Some(now_ms.saturating_add(backoff_ms));
        b.last_touched_ms = now_ms;
        true
    }

    /// Record that an address authenticated a **different** PeerId.
    ///
    /// Quarantines the address and deliberately does not touch the
    /// expected peer's backoff. An attacker who can inject one bogus
    /// address for a trusted peer must not thereby suppress that peer's
    /// real routes — which is exactly what would happen if this counted
    /// as a peer failure (ADR-0011).
    ///
    /// Returns whether the quarantine was actually recorded. It is not
    /// when the address table is full of entries that are all themselves
    /// live suppressions: the alternative is to insert regardless and
    /// let a bounded map grow without limit, or to evict a live
    /// quarantine — which would let anyone able to provoke evictions
    /// launder their own failed identity check. A caller that needs the
    /// address suppressed has to see that it was not.
    #[must_use]
    pub fn record_identity_mismatch(
        &mut self,
        expected_peer: &TransportIdentity,
        address: &str,
        now_ms: u64,
    ) -> bool {
        let key = (expected_peer.clone(), address.to_owned());
        let room = self.addresses.contains_key(&key) || {
            self.prune(now_ms);
            self.make_room_for_address(now_ms)
        };
        if !room {
            return false;
        }
        let entry = self.addresses.entry(key).or_default();
        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        entry.quarantined_until_ms = Some(now_ms.saturating_add(IDENTITY_MISMATCH_QUARANTINE_MS));
        entry.last_touched_ms = now_ms;
        // The peer map is untouched, on purpose.
        true
    }

    /// Addresses worth trying for a peer, known-good first.
    ///
    /// Preference, not exclusion: a never-successful address is still
    /// returned, just later. Excluding it would make a peer whose only
    /// address is new permanently undialable.
    #[must_use]
    pub fn preferred_addresses(
        &self,
        peer: &TransportIdentity,
        candidates: &[String],
        now_ms: u64,
    ) -> Vec<String> {
        let key = |a: &String| (peer.clone(), a.clone());
        let mut dialable: Vec<&String> = candidates
            .iter()
            .filter(|a| {
                self.addresses
                    .get(&key(a))
                    .is_none_or(|s| s.is_dialable_at(now_ms))
            })
            .collect();
        dialable.sort_by_key(|a| {
            let s = self.addresses.get(&key(a));
            let known_good = s.is_some_and(AddressState::is_known_good);
            let failures = s.map_or(0, |s| s.consecutive_failures);
            // Known-good first, then fewest failures, then stable by name
            // so the order does not depend on map iteration.
            (!known_good, failures, (*a).clone())
        });
        dialable.into_iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    const P2: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";
    const A1: &str = "/ip4/192.0.2.1/tcp/4001";
    const A2: &str = "/ip4/192.0.2.2/tcp/4001";

    /// Distinct synthetic identities, so a small pool genuinely collides.
    fn peer_n(i: usize) -> TransportIdentity {
        let tail = format!("{i:044}").replace('0', "a");
        TransportIdentity::parse(format!("Qm{}", &tail[..44])).expect("valid test identity")
    }

    fn peer() -> TransportIdentity {
        TransportIdentity::parse(P1).expect("valid identity")
    }

    fn request(origin: DialOrigin, address: &str) -> DialRequest {
        DialRequest {
            peer: Some(peer()),
            address: address.to_owned(),
            origin,
        }
    }

    fn policy() -> ConnectionPolicy {
        ConnectionPolicy::new(16, 64)
    }

    fn request_for(p: &TransportIdentity, address: &str) -> DialRequest {
        DialRequest {
            peer: Some(p.clone()),
            address: address.to_owned(),
            origin: DialOrigin::ConnectionManager,
        }
    }

    #[test]
    fn every_origin_is_classified_and_the_classification_is_pinned() {
        // Driving the other tests off `is_data_plane()` makes them blind
        // to it being WRONG: a misclassified origin simply moves to the
        // other loop, where it also passes. So the split is asserted
        // here, on its own terms, origin by origin.
        //
        // The distinction is ADR-0036's: an infrastructure-only peer may
        // be dialled for a relay reservation while staying unauthorized
        // for anything carrying application traffic. Putting a data-plane
        // origin on the reachability side is precisely how it would
        // acquire that authority by accident.
        for origin in DialOrigin::ALL {
            let expected = match origin {
                DialOrigin::Manual
                | DialOrigin::ConnectionManager
                | DialOrigin::DiscoveryReconnect
                | DialOrigin::KademliaQuery => true,
                DialOrigin::RelayReservation
                | DialOrigin::RelayCircuit
                | DialOrigin::AutonatProbe
                | DialOrigin::DcutrHolePunch => false,
            };
            assert_eq!(
                origin.is_data_plane(),
                expected,
                "{origin:?} is on the wrong side of the data-plane split"
            );
        }
    }

    #[test]
    fn an_unauthorized_peer_is_refused_whatever_the_origin() {
        let p = policy();
        // EVERY origin, from the enum rather than from memory. A
        // hand-written list proves what its author remembered, and the
        // rule under test is "no origin skips the gate".
        for origin in DialOrigin::ALL {
            assert_eq!(
                p.admit(&request(origin, A1), ConnectionClass::Unauthorized, 0),
                Err(DialDenial::Unauthorized),
                "{origin:?} should be refused"
            );
        }
    }

    #[test]
    fn an_infrastructure_peer_is_dialable_for_reachability_and_not_for_data() {
        // ADR-0036's whole point: the same peer, the same address, the
        // same instant — the origin decides.
        let p = policy();
        let class = ConnectionClass::ConnectivityInfrastructureOnly;
        assert!(
            p.admit(&request(DialOrigin::RelayReservation, A1), class, 0)
                .is_ok()
        );
        assert!(
            p.admit(&request(DialOrigin::AutonatProbe, A1), class, 0)
                .is_ok()
        );
        assert_eq!(
            p.admit(&request(DialOrigin::KademliaQuery, A1), class, 0),
            Err(DialDenial::NotAuthorizedForDataPlane)
        );
        assert_eq!(
            p.admit(&request(DialOrigin::ConnectionManager, A1), class, 0),
            Err(DialDenial::NotAuthorizedForDataPlane)
        );
    }

    #[test]
    fn a_behaviour_originated_dial_is_gated_like_any_other() {
        // There is no exempt origin. A Kademlia query cannot dial past a
        // limit that stops the ordinary scheduler.
        let mut p = policy();
        p.pending_dials = p.max_pending_dials;
        assert_eq!(
            p.admit(
                &request(DialOrigin::KademliaQuery, A1),
                ConnectionClass::DataPlaneTrusted,
                0
            ),
            Err(DialDenial::TooManyPendingDials)
        );
    }

    #[test]
    fn drain_state_precedes_every_other_answer() {
        // The most fundamental reason should be the one reported.
        let mut p = policy();
        p.shutting_down = true;
        assert_eq!(
            p.admit(
                &request(DialOrigin::ConnectionManager, A1),
                ConnectionClass::Unauthorized,
                0
            ),
            Err(DialDenial::ShuttingDown)
        );
    }

    #[test]
    fn address_poisoning_cannot_suppress_a_known_good_route() {
        // STAGE 5's REQUIRED POISONING TEST.
        //
        // The existing mismatch test uses a second address with no
        // recorded state, which is a weaker claim than the gate makes: an
        // address nobody has dialled and an address that has WORKED are
        // different things, and only the second is what a trusted peer is
        // actually reachable on. Peer-wide punitive backoff would
        // suppress both, so proving the healthy route survives needs the
        // route to be genuinely known-good.
        //
        // The attack: an attacker who can inject one address for a
        // trusted peer — through discovery, a bootstrap list, or a
        // manipulated Identify — makes it authenticate the wrong key. If
        // that counted as a peer failure, one bogus address would take
        // the peer offline.
        let mut p = policy();
        let good = "/ip4/10.0.0.1/tcp/4001";
        let poisoned = "/ip4/198.51.100.9/tcp/4001";

        p.record_success(&peer(), good, 1_000);
        assert!(
            p.address(&peer(), good)
                .is_some_and(AddressState::is_known_good),
            "the route has to be known-good for this test to mean anything"
        );

        assert!(
            p.record_identity_mismatch(&peer(), poisoned, 2_000),
            "the quarantine is recorded"
        );

        // The poisoned address is suppressed.
        assert_eq!(
            p.admit(
                &request(DialOrigin::ConnectionManager, poisoned),
                ConnectionClass::DataPlaneTrusted,
                2_000
            ),
            Err(DialDenial::AddressQuarantined)
        );

        // The known-good route is untouched: admitted, still known-good,
        // and no peer-wide backoff was created to shape it.
        assert!(
            p.admit(
                &request(DialOrigin::ConnectionManager, good),
                ConnectionClass::DataPlaneTrusted,
                2_000
            )
            .is_ok(),
            "one poisoned address must not suppress a route that works"
        );
        assert!(
            p.peer(&peer()).is_none(),
            "a mismatch must not create peer-scoped backoff"
        );
        assert!(
            p.address(&peer(), good)
                .is_some_and(AddressState::is_known_good),
            "and must not downgrade what the route had earned"
        );

        // Preference is unchanged too: the working route still sorts
        // first, and the poisoned one is excluded rather than merely
        // ranked lower.
        let preferred =
            p.preferred_addresses(&peer(), &[poisoned.to_owned(), good.to_owned()], 2_000);
        assert_eq!(preferred, vec![good.to_owned()]);

        // Repeating the attack does not accumulate into a peer-level
        // suppression by another name.
        for i in 0..16 {
            let _ = p.record_identity_mismatch(
                &peer(),
                &format!("/ip4/198.51.100.{i}/tcp/4001"),
                2_000,
            );
        }
        assert!(
            p.admit(
                &request(DialOrigin::ConnectionManager, good),
                ConnectionClass::DataPlaneTrusted,
                2_000
            )
            .is_ok(),
            "sixteen poisoned addresses must still not suppress the working one"
        );
    }

    #[test]
    fn an_identity_mismatch_quarantines_the_address_and_spares_the_peer() {
        // The attack this split exists to defeat: injecting one bogus
        // address for a trusted peer must not suppress its real routes.
        let mut p = policy();
        assert!(
            p.record_identity_mismatch(&peer(), A1, 1_000),
            "the quarantine must actually be recorded"
        );

        assert_eq!(
            p.admit(
                &request(DialOrigin::ConnectionManager, A1),
                ConnectionClass::DataPlaneTrusted,
                1_000
            ),
            Err(DialDenial::AddressQuarantined)
        );
        // The peer itself is untouched, so its other address still works.
        assert!(p.peer(&peer()).is_none());
        assert!(
            p.admit(
                &request(DialOrigin::ConnectionManager, A2),
                ConnectionClass::DataPlaneTrusted,
                1_000
            )
            .is_ok()
        );

        // And the quarantine expires rather than being permanent.
        let after = 1_000 + IDENTITY_MISMATCH_QUARANTINE_MS;
        assert!(
            p.admit(
                &request(DialOrigin::ConnectionManager, A1),
                ConnectionClass::DataPlaneTrusted,
                after
            )
            .is_ok()
        );
    }

    #[test]
    fn an_address_failure_does_not_punish_the_peer_while_a_good_route_remains() {
        let mut p = policy();
        p.record_success(&peer(), A2, 0);

        // A1 fails, but A2 is known-good: the problem is demonstrably the
        // address, so peer backoff must not advance.
        let advanced = p.record_address_failure(&peer(), A1, 100, 30_000);
        assert!(!advanced);
        assert!(p.peer(&peer()).is_none());
        assert!(
            p.admit(
                &request(DialOrigin::ConnectionManager, A2),
                ConnectionClass::DataPlaneTrusted,
                100
            )
            .is_ok()
        );
    }

    #[test]
    fn an_address_failure_with_no_alternative_does_advance_peer_backoff() {
        let mut p = policy();
        let advanced = p.record_address_failure(&peer(), A1, 100, 30_000);
        assert!(advanced);
        assert_eq!(
            p.admit(
                &request(DialOrigin::ConnectionManager, A2),
                ConnectionClass::DataPlaneTrusted,
                100
            ),
            Err(DialDenial::PeerBackoff)
        );
        // It expires.
        assert!(
            p.admit(
                &request(DialOrigin::ConnectionManager, A2),
                ConnectionClass::DataPlaneTrusted,
                30_100
            )
            .is_ok()
        );
    }

    #[test]
    fn one_peers_success_does_not_spare_a_different_peer_from_backoff() {
        // The scan used to be global, so P1 succeeding at A2 left P2
        // permanently clear however often it failed — removing retry
        // protection exactly where it was needed.
        let mut p = policy();
        let other = TransportIdentity::parse(P2).expect("valid identity");
        p.record_success(&peer(), A2, 0);

        let advanced = p.record_address_failure(&other, A1, 100, 30_000);
        assert!(
            advanced,
            "a different peer's success must not spare this one"
        );
        let request = DialRequest {
            peer: Some(other.clone()),
            address: A2.to_owned(),
            origin: DialOrigin::ConnectionManager,
        };
        assert_eq!(
            p.admit(&request, ConnectionClass::DataPlaneTrusted, 100),
            Err(DialDenial::PeerBackoff)
        );
        // And the peer that really does have a good route is unaffected.
        assert!(
            p.admit(
                &request_for(&peer(), A2),
                ConnectionClass::DataPlaneTrusted,
                100
            )
            .is_ok()
        );
    }

    #[test]
    fn an_identity_mismatch_is_scoped_to_the_peer_it_was_dialed_as() {
        // The mismatch is a fact about "A1 claims to be P1", not about A1
        // in general: another peer legitimately reachable there is not
        // quarantined by it.
        let mut p = policy();
        let other = TransportIdentity::parse(P2).expect("valid identity");
        assert!(
            p.record_identity_mismatch(&peer(), A1, 0),
            "the quarantine must actually be recorded"
        );
        assert!(
            p.admit(
                &request_for(&other, A1),
                ConnectionClass::DataPlaneTrusted,
                0
            )
            .is_ok()
        );
    }

    #[test]
    fn a_success_clears_the_peer_but_not_unrelated_quarantines() {
        let mut p = policy();
        assert!(
            p.record_identity_mismatch(&peer(), A1, 0),
            "the quarantine must actually be recorded"
        );
        p.record_address_failure(&peer(), A2, 0, 30_000);
        assert!(p.peer(&peer()).is_some());

        p.record_success(&peer(), A2, 10);
        assert!(p.peer(&peer()).is_none());
        // One working route says nothing about an address that
        // authenticated the wrong identity.
        assert!(!p.address(&peer(), A1).expect("known").is_dialable_at(10));
    }

    #[test]
    fn known_good_addresses_are_preferred_without_excluding_new_ones() {
        let mut p = policy();
        p.record_success(&peer(), A2, 0);
        p.record_address_failure(&peer(), A1, 1, 0);

        let order = p.preferred_addresses(&peer(), &[A1.to_owned(), A2.to_owned()], 10);
        assert_eq!(order, vec![A2.to_owned(), A1.to_owned()]);

        // A never-tried address is still offered: excluding it would make
        // a peer whose only address is new permanently undialable.
        let fresh = "/ip4/192.0.2.3/tcp/4001".to_owned();
        let order = p.preferred_addresses(&peer(), std::slice::from_ref(&fresh), 10);
        assert_eq!(order, vec![fresh]);
    }

    #[test]
    fn quarantined_addresses_are_omitted_from_the_preference_list() {
        let mut p = policy();
        assert!(
            p.record_identity_mismatch(&peer(), A1, 0),
            "the quarantine must actually be recorded"
        );
        let order = p.preferred_addresses(&peer(), &[A1.to_owned(), A2.to_owned()], 10);
        assert_eq!(order, vec![A2.to_owned()]);
        // And return once the quarantine lapses.
        let order = p.preferred_addresses(
            &peer(),
            &[A1.to_owned(), A2.to_owned()],
            IDENTITY_MISMATCH_QUARANTINE_MS + 1,
        );
        assert_eq!(order.len(), 2);
    }

    #[test]
    fn an_unidentified_dial_is_still_gated_on_limits() {
        // No PeerId yet, so no class-based answer — but an
        // unauthenticated dial still consumes resources.
        let mut p = policy();
        p.connections = p.max_connections;
        let anonymous = DialRequest {
            peer: None,
            address: A1.to_owned(),
            origin: DialOrigin::ConnectionManager,
        };
        assert_eq!(
            p.admit(&anonymous, ConnectionClass::DataPlaneTrusted, 0),
            Err(DialDenial::ConnectionLimitReached)
        );
    }
    #[test]
    fn flooding_the_table_cannot_clear_an_existing_quarantine() {
        // THE attack this bound has to survive. An address that failed
        // its identity check is quarantined; the attacker then names as
        // many fresh addresses as the table will hold, hoping the
        // eviction that follows drops the quarantine and restores their
        // route. It must not.
        let mut p = ConnectionPolicy::new(64, 64);
        p.max_addresses = 8;

        assert!(
            p.record_identity_mismatch(&peer(), "/ip4/10.0.0.1/tcp/1", 1_000),
            "the quarantine must actually be recorded"
        );
        assert!(
            !p.address(&peer(), "/ip4/10.0.0.1/tcp/1")
                .expect("quarantined")
                .is_dialable_at(2_000)
        );

        // Flood well past the cap.
        for i in 0..200 {
            p.record_success(&peer(), &format!("/ip4/10.0.0.2/tcp/{i}"), 2_000);
        }

        assert!(
            p.address_entries() <= p.max_addresses,
            "the table must stay bounded: {} entries",
            p.address_entries()
        );
        let quarantined = p
            .address(&peer(), "/ip4/10.0.0.1/tcp/1")
            .expect("the quarantine must survive the flood");
        assert!(
            !quarantined.is_dialable_at(2_000),
            "an attacker must not be able to evict their own quarantine"
        );
    }

    #[test]
    fn a_table_of_live_quarantines_denies_rather_than_forgetting_one() {
        // With nothing benign left to evict the only options are dropping
        // a live suppression or refusing. Refusing is at worst
        // self-inflicted; forgetting is attacker-controlled.
        let mut p = ConnectionPolicy::new(64, 64);
        p.max_addresses = 4;
        for i in 0..4 {
            assert!(
                p.record_identity_mismatch(&peer(), &format!("/ip4/10.0.0.9/tcp/{i}"), 1_000),
                "the quarantine must actually be recorded"
            );
        }
        assert_eq!(p.address_entries(), 4);

        let fresh = DialRequest {
            peer: Some(peer()),
            address: "/ip4/10.0.0.99/tcp/1".to_owned(),
            origin: DialOrigin::ConnectionManager,
        };
        assert_eq!(
            p.admit(&fresh, ConnectionClass::DataPlaneTrusted, 2_000),
            Err(DialDenial::PolicyStateFull)
        );

        // Once the quarantines lapse, the same dial is admitted again.
        let later = 1_000 + IDENTITY_MISMATCH_QUARANTINE_MS;
        p.record_success(&peer(), "/ip4/10.0.0.9/tcp/0", later);
        assert!(
            p.admit(&fresh, ConnectionClass::DataPlaneTrusted, later)
                .is_ok()
        );
    }

    #[test]
    fn a_full_table_of_live_suppressions_stops_growing() {
        // The bound is the point of the map, and it was enforced
        // everywhere except the one path that mattered: the eviction
        // result was computed and then discarded, so a failure with
        // nothing evictable was appended anyway.
        //
        // Enough concurrent failures fill the table with live punitive
        // entries, and from then on every further failed address grows a
        // structure that is documented as bounded — which is precisely
        // the shape a remote peer would drive.
        let mut p = ConnectionPolicy::new(64, 64);
        p.max_addresses = 4;
        for i in 0..4 {
            assert!(
                p.record_identity_mismatch(&peer(), &format!("/ip4/10.0.0.9/tcp/{i}"), 1_000),
                "the table fills with live quarantines"
            );
        }
        assert_eq!(p.address_entries(), 4);

        // Every entry is punitive and live, so nothing can be evicted.
        for i in 0..32 {
            let _ =
                p.record_address_failure(&peer(), &format!("/ip4/10.0.0.8/tcp/{i}"), 1_100, 500);
        }
        assert_eq!(
            p.address_entries(),
            4,
            "a failure that cannot evict must not be recorded either"
        );

        for i in 0..32 {
            assert!(
                !p.record_identity_mismatch(&peer(), &format!("/ip4/10.0.0.7/tcp/{i}"), 1_100),
                "a quarantine that cannot be held is reported, not silently inserted"
            );
        }
        assert_eq!(p.address_entries(), 4, "still bounded");

        // The suppressions the table already holds are intact — refusing
        // is what protects them.
        for i in 0..4 {
            let held = p
                .address(&peer(), &format!("/ip4/10.0.0.9/tcp/{i}"))
                .expect("the original quarantines survive");
            assert!(!held.is_dialable_at(1_100));
        }
    }

    #[test]
    fn peer_backoff_still_advances_when_the_address_table_is_full() {
        // Declining to record the ADDRESS must not also cost the
        // peer-level suppression: the dial did fail, and the return
        // value is what a caller uses to learn the peer is now backed
        // off.
        let mut p = ConnectionPolicy::new(64, 64);
        p.max_addresses = 2;
        for i in 0..2 {
            assert!(
                p.record_identity_mismatch(&peer(), &format!("/ip4/10.0.0.9/tcp/{i}"), 1_000),
                "fill with live quarantines"
            );
        }

        let advanced = p.record_address_failure(&peer(), "/ip4/10.0.0.5/tcp/1", 1_100, 500);
        assert!(
            advanced,
            "no eligible known-good address remains, so the peer backs off"
        );
        assert_eq!(p.address_entries(), 2, "and the table did not grow");
    }

    #[test]
    fn the_caps_hold_under_arbitrary_outcome_sequences() {
        // Selected examples prove the cases someone thought of, and the
        // bug this replaces was in a path nobody had thought of: the
        // eviction result was computed and discarded on exactly the
        // branch the examples did not reach.
        //
        // So drive the state machine with a deterministic pseudo-random
        // mix of every mutation and assert the invariant after each one.
        // Deterministic because a cap violation must reproduce from the
        // seed printed in the failure, not on a rerun that happens to
        // shuffle differently.
        const MAX_ADDRESSES: usize = 8;
        const MAX_PEERS: usize = 4;

        for seed in 0..16u64 {
            let mut p = ConnectionPolicy::new(64, 64);
            p.max_addresses = MAX_ADDRESSES;
            p.max_peers = MAX_PEERS;

            // xorshift: no dependency, and the sequence is a pure
            // function of the seed.
            let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
            let mut next = move || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state
            };

            for step in 0..400u64 {
                let r = next();
                // A small pool of peers and addresses, so entries
                // genuinely collide and evictions genuinely happen.
                let who = peer_n(usize::try_from(r % 6).unwrap_or(0));
                let addr = format!("/ip4/10.0.0.{}/tcp/{}", (r >> 8) % 7, (r >> 16) % 5);
                let now = step.wrapping_mul(37);

                match (r >> 32) % 4 {
                    0 => p.record_success(&who, &addr, now),
                    1 => {
                        let _ = p.record_address_failure(&who, &addr, now, 500);
                    }
                    2 => {
                        let _ = p.record_identity_mismatch(&who, &addr, now);
                    }
                    _ => {
                        let _ = p.prune(now);
                    }
                }

                assert!(
                    p.address_entries() <= MAX_ADDRESSES,
                    "seed {seed} step {step}: address table holds {} entries, cap is {MAX_ADDRESSES}",
                    p.address_entries()
                );
                assert!(
                    p.peer_entries() <= MAX_PEERS,
                    "seed {seed} step {step}: peer table holds {} entries, cap is {MAX_PEERS}",
                    p.peer_entries()
                );
            }
        }
    }

    #[test]
    fn pruning_drops_idle_entries_and_keeps_punitive_ones() {
        let mut p = ConnectionPolicy::new(64, 64);
        p.idle_ttl_ms = 1_000;

        p.record_success(&peer(), "/ip4/10.0.0.1/tcp/1", 0);
        assert!(
            p.record_identity_mismatch(&peer(), "/ip4/10.0.0.2/tcp/1", 0),
            "the quarantine must actually be recorded"
        );
        assert_eq!(p.address_entries(), 2);

        // Well past the idle TTL but inside the quarantine window.
        let dropped = p.prune(5_000);
        assert_eq!(dropped, 1, "only the benign idle entry goes");
        assert!(p.address(&peer(), "/ip4/10.0.0.1/tcp/1").is_none());
        assert!(
            p.address(&peer(), "/ip4/10.0.0.2/tcp/1").is_some(),
            "a punitive entry outlives the traffic that created it"
        );

        // And once the quarantine lapses it becomes prunable like any other.
        let after = IDENTITY_MISMATCH_QUARANTINE_MS + 10_000;
        assert_eq!(p.prune(after), 0, "consecutive_failures still protects it");
        p.record_success(&peer(), "/ip4/10.0.0.2/tcp/1", after);
        assert_eq!(p.prune(after + 10_000), 1);
    }

    #[test]
    fn peer_backoff_entries_are_bounded_too() {
        let mut p = ConnectionPolicy::new(64, 64);
        p.max_peers = 4;
        for i in 0..40_u8 {
            let mut raw = [b'1'; 44];
            raw[0] = b'1' + (i % 9);
            raw[1] = b'A' + (i / 9);
            let Ok(id) = TransportIdentity::parse(format!(
                "12D3KooW{}",
                core::str::from_utf8(&raw).expect("ascii")
            )) else {
                continue;
            };
            // A failure with no alternative address advances peer backoff.
            p.record_address_failure(&id, "/ip4/10.0.0.1/tcp/1", 1_000, 5_000);
        }
        assert!(
            p.peer_entries() <= p.max_peers,
            "peer backoff map must stay bounded: {}",
            p.peer_entries()
        );
    }

    #[test]
    fn a_full_backoff_map_reports_that_backoff_did_not_advance() {
        // The return value is what a caller uses to decide the peer is
        // suppressed. Growing the map instead would be unbounded; lying
        // about it would be worse.
        let mut p = ConnectionPolicy::new(64, 64);
        p.max_peers = 1;

        let first = peer();
        assert!(p.record_address_failure(&first, "/ip4/10.0.0.1/tcp/1", 1_000, 5_000));

        let second = TransportIdentity::parse(P2).expect("valid identity");
        assert!(
            !p.record_address_failure(&second, "/ip4/10.0.0.2/tcp/1", 1_000, 5_000),
            "with no room, backoff did not advance and the caller must be told"
        );
        assert_eq!(p.peer_entries(), 1);

        // Once the first backoff lapses it is evictable and the second
        // peer records normally.
        assert!(p.record_address_failure(&second, "/ip4/10.0.0.2/tcp/1", 9_000, 5_000));
        assert_eq!(p.peer_entries(), 1);
    }
}
