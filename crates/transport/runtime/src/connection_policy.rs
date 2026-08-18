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
    /// The ordinary candidate dial scheduler.
    ConnectionManager,
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
        matches!(self, Self::ConnectionManager | Self::KademliaQuery)
    }
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
}

/// Address-scoped reachability and authentication state.
///
/// Kept separate from [`PeerBackoff`] deliberately — see the module note.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AddressState {
    /// Consecutive failures for this address.
    pub consecutive_failures: u32,
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
}

/// Peer-scoped punitive backoff.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeerBackoff {
    /// Consecutive peer-scoped failures.
    pub consecutive_failures: u32,
    /// In backoff until this time.
    pub until_ms: Option<u64>,
}

impl PeerBackoff {
    /// Whether the peer may be dialed at `now_ms`.
    #[must_use]
    pub fn is_clear_at(&self, now_ms: u64) -> bool {
        self.until_ms.is_none_or(|until| now_ms >= until)
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
#[derive(Debug, Clone, Default)]
pub struct ConnectionPolicy {
    addresses: BTreeMap<AddressKey, AddressState>,
    peers: BTreeMap<TransportIdentity, PeerBackoff>,
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

impl ConnectionPolicy {
    /// Build a policy with explicit limits.
    #[must_use]
    pub fn new(max_pending_dials: usize, max_connections: usize) -> Self {
        Self {
            max_pending_dials,
            max_connections,
            ..Self::default()
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
        Ok(())
    }

    /// Record a successful authenticated connection.
    ///
    /// Clears this address's failures and the peer's punitive state. It
    /// does **not** rehabilitate other quarantined addresses: one working
    /// route says nothing about an address that authenticated the wrong
    /// identity.
    pub fn record_success(&mut self, peer: &TransportIdentity, address: &str, now_ms: u64) {
        let entry = self
            .addresses
            .entry((peer.clone(), address.to_owned()))
            .or_default();
        entry.consecutive_failures = 0;
        entry.last_success_ms = Some(now_ms);
        entry.quarantined_until_ms = None;
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
        let entry = self
            .addresses
            .entry((peer.clone(), address.to_owned()))
            .or_default();
        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);

        // Scoped to THIS peer. A global scan would let any unrelated
        // peer's past success spare this one from backoff indefinitely,
        // removing retry protection exactly where it is needed.
        let alternative_exists = self.addresses.iter().any(|((p, a), s)| {
            p == peer && a != address && s.is_known_good() && s.is_dialable_at(now_ms)
        });
        if alternative_exists {
            return false;
        }
        let b = self.peers.entry(peer.clone()).or_default();
        b.consecutive_failures = b.consecutive_failures.saturating_add(1);
        b.until_ms = Some(now_ms.saturating_add(backoff_ms));
        true
    }

    /// Record that an address authenticated a **different** PeerId.
    ///
    /// Quarantines the address and deliberately does not touch the
    /// expected peer's backoff. An attacker who can inject one bogus
    /// address for a trusted peer must not thereby suppress that peer's
    /// real routes — which is exactly what would happen if this counted
    /// as a peer failure (ADR-0011).
    pub fn record_identity_mismatch(
        &mut self,
        expected_peer: &TransportIdentity,
        address: &str,
        now_ms: u64,
    ) {
        let entry = self
            .addresses
            .entry((expected_peer.clone(), address.to_owned()))
            .or_default();
        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        entry.quarantined_until_ms = Some(now_ms.saturating_add(IDENTITY_MISMATCH_QUARANTINE_MS));
        // The peer map is untouched, on purpose.
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
    fn an_unauthorized_peer_is_refused_whatever_the_origin() {
        let p = policy();
        for origin in [
            DialOrigin::ConnectionManager,
            DialOrigin::KademliaQuery,
            DialOrigin::RelayReservation,
            DialOrigin::AutonatProbe,
            DialOrigin::DcutrHolePunch,
            DialOrigin::RelayCircuit,
        ] {
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
    fn an_identity_mismatch_quarantines_the_address_and_spares_the_peer() {
        // The attack this split exists to defeat: injecting one bogus
        // address for a trusted peer must not suppress its real routes.
        let mut p = policy();
        p.record_identity_mismatch(&peer(), A1, 1_000);

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
        p.record_identity_mismatch(&peer(), A1, 0);
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
        p.record_identity_mismatch(&peer(), A1, 0);
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
        p.record_identity_mismatch(&peer(), A1, 0);
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
}
