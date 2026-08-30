// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The other half of the dial gate: the one behaviours cannot walk past.
//!
//! # Two doors, and this one now answers with policy
//!
//! [`GatedSwarm`](crate::gated_swarm::GatedSwarm) closes the command
//! path: the raw `Swarm` is private, and `dial` needs an admission
//! ticket. That says nothing about dials a `NetworkBehaviour`
//! originates from inside the Swarm — Kademlia filling a bucket,
//! AutoNAT probing, Relay renewing a reservation. Those never pass
//! through the wrapper at all.
//!
//! libp2p routes every dial, whatever asked for it, through
//! `NetworkBehaviour::handle_pending_outbound_connection`, and it does
//! so synchronously inside `Swarm::dial`. Until Stage 10 this behaviour
//! refused every dial without a ticket outright, which was correct
//! while nothing behaviour-originated dialled. Kademlia is the change
//! that makes something dial, so an unticketed dial is now admitted
//! through the SAME root admission an ordinary dial passes —
//! `SnapshotHandle::admit` under `DialOrigin::KademliaQuery` — and its
//! ticket is deposited with the runtime's in-flight set so the ordinary
//! settlement path owns the outcome. Trust, peer backoff, drain state
//! and both ceilings all bind (SPIKE-003 F1/F7/F8); nothing is admitted
//! that the policy would refuse, and nothing dials unaccounted.
//!
//! # What each hook can decide (F9)
//!
//! For a behaviour-originated dial libp2p calls the pending hook with
//! an EMPTY address list — the hook exists so each behaviour can
//! contribute addresses, and the union is dialled after it returns. So
//! the pending hook decides everything peer-scoped and global, on the
//! empty placeholder address, and the ADDRESS decision moves to
//! `handle_established_outbound_connection`, which is handed the
//! address the dial actually used: the ticket is re-bound to it
//! (F12), stripped of its `/p2p/` suffix first (F10), and the
//! quarantine is asked through the capacity-free
//! [`PolicySnapshot::address_dialable`] (F11). A quarantined route
//! therefore costs one TCP connect and is then refused — later than
//! the command path's check, and the only place a behaviour dial has
//! one at all.
//!
//! # What it is not
//!
//! Not a policy. The decisions are made by the root
//! [`PolicySnapshot::admit`] and
//! [`PolicySnapshot::address_dialable`]; this behaviour only makes
//! sure they are asked for THIS dial, at the moments they can be.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use libp2p::PeerId;
use libp2p::core::transport::PortUse;
use libp2p::core::{Endpoint, Multiaddr};
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler, THandlerInEvent,
    THandlerOutEvent, ToSwarm, dummy,
};

use interweave_transport_api::TransportIdentity;
use interweave_transport_runtime::{DialOrigin, DialRequest, DialTicket, SnapshotHandle};

/// What a refused behaviour dial is told when it names no peer.
const NO_PEER: &str = "a behaviour dial that names no peer cannot be classified";

/// Connection ids the root admission has issued a ticket for.
///
/// Shared between the wrapper that dials and the behaviour that
/// answers, because the two are the same decision seen from either end
/// of one synchronous call: `Swarm::dial` invokes the hook before it
/// returns, so an id is registered and consumed within a single
/// statement and the set is empty in between.
#[derive(Debug, Clone, Default)]
pub struct AdmittedDials {
    ids: Arc<Mutex<HashSet<ConnectionId>>>,
}

impl AdmittedDials {
    /// Announce that this connection id carries a valid admission.
    pub fn register(&self, id: ConnectionId) {
        self.lock().insert(id);
    }

    /// Consume the announcement, reporting whether there was one.
    pub fn take(&self, id: ConnectionId) -> bool {
        self.lock().remove(&id)
    }

    /// Drop an announcement that was never consumed.
    ///
    /// A dial libp2p refuses before reaching the hook leaves its id
    /// behind, and an id that stays is one a later dial could reuse
    /// without an admission. Cheap to call unconditionally, which is
    /// why the caller does.
    pub fn forget(&self, id: ConnectionId) {
        self.lock().remove(&id);
    }

    /// Ids currently announced. Zero everywhere except mid-dial.
    #[must_use]
    pub fn outstanding(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashSet<ConnectionId>> {
        // Poisoning is recovered rather than propagated: the protected
        // value is a set of ids with no invariant spanning two
        // operations, so a panic elsewhere must not turn every future
        // dial into a denial.
        self.ids.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Tickets for dials the Swarm has accepted and not yet reported on.
///
/// Keyed by the connection id the dial was built with, which is what
/// every outcome event carries back. Shared — the same `Arc` pattern as
/// [`AdmittedDials`] — because two parties genuinely hold it: the
/// runtime loop deposits and settles ordinary dials, and the gate
/// deposits behaviour dials in its pending hook and re-binds them in
/// its established hook. Bounded by `max_pending_dials`: every ticket
/// in here holds a pending-dial reservation, so the admission that
/// minted it already enforced the ceiling.
#[derive(Debug, Clone, Default)]
pub struct InFlightTickets {
    inner: Arc<Mutex<HashMap<ConnectionId, DialTicket>>>,
}

impl InFlightTickets {
    /// File a ticket under the connection that will settle it.
    pub fn deposit(&self, id: ConnectionId, ticket: DialTicket) {
        self.lock().insert(id, ticket);
    }

    /// Take the ticket a settlement owns, if this dial was ours.
    #[must_use]
    pub fn settle(&self, id: ConnectionId) -> Option<DialTicket> {
        self.lock().remove(&id)
    }

    /// Re-bind a PLACEHOLDER ticket to the address its dial used,
    /// returning the admitted peer when this was such a ticket.
    ///
    /// `None` for a connection that is not ours, and for an ordinary
    /// admitted dial — its address was decided at admission and
    /// [`DialTicket::rebind_address`] refuses to move it, so the
    /// established hook leaves it entirely alone.
    #[must_use]
    pub fn rebind_placeholder(&self, id: ConnectionId, used: &str) -> Option<TransportIdentity> {
        let mut held = self.lock();
        let ticket = held.get_mut(&id)?;
        if !ticket.rebind_address(used) {
            return None;
        }
        ticket.peer().cloned()
    }

    /// Dials currently in flight.
    #[must_use]
    pub fn outstanding(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ConnectionId, DialTicket>> {
        // Recovered for the same reason as [`AdmittedDials::lock`]: no
        // invariant spans two operations on the map itself.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Strip TRAILING `/p2p/<peer>` components, leaving the address the
/// policy is keyed by (F10).
///
/// A behaviour dial's address arrives with the peer appended — a query
/// result carries it — while the address book and the quarantine map
/// are keyed by the bare transport address, which is what
/// `AdmittedDial` binds. Passing the suffixed form to the policy looks
/// up an address it has never seen, so every quarantine silently
/// misses. Only the TRAILING components go: a `/p2p/` in the middle of
/// the address is a relay path's inner hop, part of the route rather
/// than a claim about who answers.
#[must_use]
pub fn strip_peer_suffix(address: &Multiaddr) -> String {
    let mut parts: Vec<_> = address.iter().collect();
    while matches!(parts.last(), Some(libp2p::multiaddr::Protocol::P2p(_))) {
        parts.pop();
    }
    parts.into_iter().collect::<Multiaddr>().to_string()
}

/// Strip trailing `/p2p/` components ONLY while they name `peer`.
///
/// The identity-checked variant for a connection whose peer is already
/// authenticated: a trailing claim that names someone else is the
/// address contradicting the connection, and stripping it would launder
/// the contradiction into the bare route — the policy would then score
/// and quarantine an address string the observation never honestly
/// described. The foreign claim stays in the key instead, so whatever
/// the policy records is recorded against the literal that lied.
#[must_use]
pub fn strip_own_suffix(address: &Multiaddr, peer: &PeerId) -> String {
    let mut parts: Vec<_> = address.iter().collect();
    while matches!(parts.last(), Some(libp2p::multiaddr::Protocol::P2p(claimed)) if claimed == peer)
    {
        parts.pop();
    }
    parts.into_iter().collect::<Multiaddr>().to_string()
}

/// Admits every outbound dial: ticketed dials by their ticket,
/// behaviour dials through the root policy.
#[derive(Debug)]
pub struct OutboundAdmission {
    admitted: AdmittedDials,
    /// The root admission, through the non-blocking handle: this runs
    /// synchronously inside the Swarm poll, and ADR-0011 forbids the
    /// gate blocking on the policy there. The handle also retries a
    /// `PolicySuperseded` refusal, so a trust revision landing mid-dial
    /// is a reload rather than a spurious denial.
    admission: SnapshotHandle,
    /// Where an admitted behaviour dial's ticket goes, so the ordinary
    /// settlement path owns its outcome (F7/F8).
    in_flight: InFlightTickets,
    /// The SAME clock origin the runtime hands the policy. A second
    /// origin — or a frozen one — would timestamp admissions on a
    /// different axis than settlements: SPIKE-003's F8b measured the
    /// frozen version making every backoff permanent.
    started: tokio::time::Instant,
}

impl OutboundAdmission {
    /// Build the gate over the root admission.
    #[must_use]
    pub fn new(
        admission: SnapshotHandle,
        in_flight: InFlightTickets,
        started: tokio::time::Instant,
    ) -> Self {
        Self {
            admitted: AdmittedDials::default(),
            admission,
            in_flight,
            started,
        }
    }

    /// A handle to the set this behaviour consults.
    #[must_use]
    pub fn admitted(&self) -> AdmittedDials {
        self.admitted.clone()
    }

    /// Milliseconds since the runtime's clock origin.
    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

impl NetworkBehaviour for OutboundAdmission {
    type ConnectionHandler = dummy::ConnectionHandler;
    type ToSwarm = std::convert::Infallible;

    /// Every outbound dial, whoever asked for it.
    ///
    /// # Errors
    /// [`ConnectionDenied`] carrying the policy's own refusal when the
    /// root admission denies a behaviour-originated dial, or the
    /// no-peer refusal when there is no identity to classify.
    fn handle_pending_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: Option<PeerId>,
        _addresses: &[Multiaddr],
        _effective_role: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        if self.admitted.take(connection_id) {
            // No extra addresses: the admitted ones are bound into the
            // DialOpts by `AdmittedDial`, and adding any here would be
            // this behaviour deciding where an admitted dial goes.
            return Ok(Vec::new());
        }
        // NO TICKET means no root admission issued this dial, which is
        // the definition of behaviour-originated. Every one of those is
        // a Kademlia iterative-query dial today — the only dialling
        // behaviour Stage 10 activates — so the origin names it, and a
        // denial an operator reads says which subsystem asked.
        let Some(peer) = peer else {
            return Err(ConnectionDenied::new(std::io::Error::other(NO_PEER)));
        };
        let Ok(identity) = TransportIdentity::parse(peer.to_base58()) else {
            return Err(ConnectionDenied::new(std::io::Error::other(format!(
                "behaviour dial names an identity outside the neutral grammar: {peer}"
            ))));
        };
        // THE EMPTY PLACEHOLDER, deliberately (F9): the hook is given
        // no addresses, so peer-scoped and global policy — trust,
        // backoff, drain, both ceilings — are decided here, and the
        // address decision waits for the established hook, where an
        // address exists. F16's cost is accepted knowingly: the
        // reservation cannot be separated from the admission, so an
        // address table full of live quarantines refuses behaviour
        // dials outright, which is fail-closed.
        let request = DialRequest {
            peer: Some(identity),
            address: String::new(),
            origin: DialOrigin::KademliaQuery,
        };
        match self.admission.admit(&request, self.now_ms()) {
            Ok(ticket) => {
                // DEPOSITED, not dropped: the ticket holds the pending
                // and connection reservations, and the runtime's
                // ordinary settlement path releases them when the
                // outcome event arrives (F7/F8).
                self.in_flight.deposit(connection_id, ticket);
                Ok(Vec::new())
            }
            Err(denial) => Err(ConnectionDenied::new(std::io::Error::other(format!(
                "kademlia dial refused: {denial:?}"
            )))),
        }
    }

    fn handle_established_inbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        _peer: PeerId,
        _local_addr: &Multiaddr,
        _remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(dummy::ConnectionHandler)
    }

    /// The ADDRESS decision the pending hook could not make (F9).
    ///
    /// Only a behaviour dial is judged here: an ordinary admitted dial
    /// bound its address into the `DialOpts` and was decided before it
    /// was made, and `rebind_placeholder` refuses to touch its ticket.
    /// The ticket is re-bound FIRST (F12), so whichever way this hook
    /// answers, the settlement that follows scores the address the
    /// dial actually used rather than the placeholder.
    ///
    /// # Errors
    /// [`ConnectionDenied`] when the address this dial actually used is
    /// one the policy suppresses. The refusal surfaces as an
    /// `OutgoingConnectionError`, and the re-bound ticket settles
    /// against the real address there.
    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        _role_override: Endpoint,
        _port_use: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        let used = strip_own_suffix(addr, &peer);
        let Some(peer) = self.in_flight.rebind_placeholder(connection_id, &used) else {
            return Ok(dummy::ConnectionHandler);
        };
        // CAPACITY-FREE, by construction (F11): `address_dialable`
        // reads the quarantine and nothing else, so there is no
        // capacity denial to discard — the probe-through-admit version
        // was refused at a full ceiling for the very slot this
        // connection occupies.
        if self
            .admission
            .load()
            .address_dialable(&peer, &used, self.now_ms())
        {
            Ok(dummy::ConnectionHandler)
        } else {
            // One TCP connect was spent learning this (F9's stated
            // cost); the connection is refused before a handler exists.
            Err(ConnectionDenied::new(std::io::Error::other(format!(
                "kademlia connection refused on its address: {used} is quarantined"
            ))))
        }
    }

    fn on_swarm_event(&mut self, _event: FromSwarm<'_>) {}

    fn on_connection_handler_event(
        &mut self,
        _peer: PeerId,
        _connection: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        match event {}
    }

    fn poll(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    use interweave_transport_runtime::{ConnectionManager, ConnectionPolicy, TrustSources};
    use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
    use libp2p::swarm::ConnectionId;

    const TRUSTED: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

    fn ident(s: &str) -> TransportIdentity {
        TransportIdentity::parse(s).expect("valid identity")
    }

    fn manager(trusted: &[&str]) -> ConnectionManager {
        let mut m = ConnectionManager::new(ConnectionPolicy::new(8, 8), 8);
        let _ = m.set_trust(
            TrustSources::new(
                PeerTrustPolicy::new(trusted.iter().map(|p| ident(p))).expect("small"),
                InfrastructureSet::default(),
            ),
            &[],
        );
        m
    }

    fn gate(m: &ConnectionManager) -> (OutboundAdmission, InFlightTickets) {
        let in_flight = InFlightTickets::default();
        (
            OutboundAdmission::new(m.handle(), in_flight.clone(), tokio::time::Instant::now()),
            in_flight,
        )
    }

    fn behaviour_dial(
        gate: &mut OutboundAdmission,
        id: usize,
        peer: &str,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        gate.handle_pending_outbound_connection(
            ConnectionId::new_unchecked(id),
            Some(peer.parse().expect("valid PeerId")),
            &[],
            Endpoint::Dialer,
        )
    }

    fn established(
        gate: &mut OutboundAdmission,
        id: usize,
        peer: &str,
        addr: &str,
    ) -> Result<THandler<OutboundAdmission>, ConnectionDenied> {
        gate.handle_established_outbound_connection(
            ConnectionId::new_unchecked(id),
            peer.parse().expect("valid PeerId"),
            &addr.parse::<Multiaddr>().expect("valid multiaddr"),
            Endpoint::Dialer,
            PortUse::Reuse,
        )
    }

    #[tokio::test]
    async fn an_untrusted_behaviour_dial_is_refused_and_reserves_nothing() {
        // The spike's own mutation, against production: admit everything
        // and all three of these tests fail. ADR-0012's default admits
        // nobody, so a Kademlia walk to a stranger stops HERE.
        let m = manager(&[]);
        let (mut g, in_flight) = gate(&m);
        assert!(behaviour_dial(&mut g, 1, TRUSTED).is_err());
        assert_eq!(in_flight.outstanding(), 0, "a refusal deposits nothing");
        assert_eq!(
            m.handle().load().pending_dials(),
            0,
            "and reserves nothing — a denied dial cannot spend the ceiling"
        );
    }

    #[tokio::test]
    async fn a_trusted_behaviour_dial_is_admitted_and_its_ticket_deposited() {
        let m = manager(&[TRUSTED]);
        let (mut g, in_flight) = gate(&m);
        let contributed = behaviour_dial(&mut g, 1, TRUSTED).expect("trusted, fresh policy");
        assert!(
            contributed.is_empty(),
            "the gate never decides where a dial goes"
        );
        assert_eq!(in_flight.outstanding(), 1, "the ticket is deposited");
        assert_eq!(
            m.handle().load().pending_dials(),
            1,
            "and its reservation is REAL: dropped on receipt, the ceiling \
             would bound nothing (F8)"
        );
        let ticket = in_flight
            .settle(ConnectionId::new_unchecked(1))
            .expect("ours");
        assert_eq!(ticket.address(), "", "admitted on the placeholder (F9)");
        assert_eq!(ticket.origin(), DialOrigin::KademliaQuery);
    }

    #[tokio::test]
    async fn a_draining_manager_refuses_behaviour_dials() {
        let mut m = manager(&[TRUSTED]);
        m.begin_shutdown();
        let (mut g, in_flight) = gate(&m);
        assert!(behaviour_dial(&mut g, 1, TRUSTED).is_err());
        assert_eq!(in_flight.outstanding(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn peer_backoff_binds_and_lapses_on_the_live_clock() {
        // F8b: the spike's gate froze its clock at zero and every
        // backoff became permanent while every immediate-refusal
        // assertion stayed green. The lapse half of this test is what
        // that mutation fails.
        let mut m = manager(&[TRUSTED]);
        let peer = ident(TRUSTED);
        let ticket = m
            .handle()
            .admit(
                &interweave_transport_runtime::DialRequest {
                    peer: Some(peer),
                    address: "/ip4/192.0.2.1/tcp/1".to_owned(),
                    origin: DialOrigin::ConnectionManager,
                },
                0,
            )
            .expect("admitted");
        m.record_failure(ticket, 0);

        let (mut g, _in_flight) = gate(&m);
        assert!(
            behaviour_dial(&mut g, 1, TRUSTED).is_err(),
            "a peer in backoff is refused to Kademlia exactly as to everyone"
        );
        tokio::time::advance(std::time::Duration::from_secs(3_600)).await;
        behaviour_dial(&mut g, 2, TRUSTED)
            .expect("the backoff LAPSES: the gate reads a clock that moves");
    }

    #[tokio::test]
    async fn a_ticketed_dial_passes_exactly_once() {
        let m = manager(&[]);
        let (mut g, _in_flight) = gate(&m);
        let admitted = g.admitted();
        let id = ConnectionId::new_unchecked(11);
        admitted.register(id);
        assert!(
            g.handle_pending_outbound_connection(id, None, &[], Endpoint::Dialer)
                .is_ok(),
            "the admitted dial proceeds"
        );
        assert_eq!(admitted.outstanding(), 0, "and the registration is spent");
        assert!(
            g.handle_pending_outbound_connection(id, None, &[], Endpoint::Dialer)
                .is_err(),
            "the same id must not pass a second time — and with no peer \
             there is nothing to classify a policy admission from"
        );
    }

    #[tokio::test]
    async fn the_established_hook_rebinds_and_refuses_a_quarantined_route() {
        let mut m = manager(&[TRUSTED]);
        let peer = ident(TRUSTED);
        // Quarantine one address the ordinary way: it authenticated the
        // wrong identity once.
        let bad = m
            .handle()
            .admit(
                &interweave_transport_runtime::DialRequest {
                    peer: Some(peer.clone()),
                    address: "/ip4/192.0.2.1/tcp/1".to_owned(),
                    origin: DialOrigin::ConnectionManager,
                },
                0,
            )
            .expect("admitted");
        assert!(m.record_identity_mismatch(bad, 0));

        let (mut g, in_flight) = gate(&m);
        // The PENDING hook passes: the quarantine is address-scoped and
        // the placeholder names no address (F16, structurally).
        behaviour_dial(&mut g, 1, TRUSTED).expect("peer-scoped policy holds");
        // The dial lands on the quarantined route, peer suffix and all.
        let refused = established(
            &mut g,
            1,
            TRUSTED,
            &format!("/ip4/192.0.2.1/tcp/1/p2p/{TRUSTED}"),
        );
        assert!(
            refused.is_err(),
            "the address the dial actually used is one the policy suppresses"
        );
        let ticket = in_flight
            .settle(ConnectionId::new_unchecked(1))
            .expect("ours");
        assert_eq!(
            ticket.address(),
            "/ip4/192.0.2.1/tcp/1",
            "re-bound BEFORE the verdict — the settlement scores the real \
             route, stripped of its /p2p suffix (F10/F12)"
        );

        // THE CONTROL: the same peer's other address establishes.
        behaviour_dial(&mut g, 2, TRUSTED).expect("admitted again");
        let kept = established(
            &mut g,
            2,
            TRUSTED,
            &format!("/ip4/192.0.2.2/tcp/1/p2p/{TRUSTED}"),
        );
        assert!(
            kept.is_ok(),
            "quarantine is a fact about ONE address, not about the peer"
        );
        let ticket = in_flight
            .settle(ConnectionId::new_unchecked(2))
            .expect("ours");
        assert_eq!(ticket.address(), "/ip4/192.0.2.2/tcp/1");
    }

    #[tokio::test]
    async fn an_ordinary_admitted_dial_is_untouched_at_establishment() {
        let m = manager(&[TRUSTED]);
        let (mut g, in_flight) = gate(&m);
        let ticket = m
            .handle()
            .admit(
                &interweave_transport_runtime::DialRequest {
                    peer: Some(ident(TRUSTED)),
                    address: "/ip4/192.0.2.9/tcp/1".to_owned(),
                    origin: DialOrigin::ConnectionManager,
                },
                0,
            )
            .expect("admitted");
        in_flight.deposit(ConnectionId::new_unchecked(3), ticket);
        // Even a quarantined-looking establishment address changes
        // nothing: the address was DECIDED at admission, and the hook
        // refuses to move or judge it.
        let kept = established(&mut g, 3, TRUSTED, "/ip4/192.0.2.7/tcp/7");
        assert!(kept.is_ok());
        let ticket = in_flight
            .settle(ConnectionId::new_unchecked(3))
            .expect("ours");
        assert_eq!(
            ticket.address(),
            "/ip4/192.0.2.9/tcp/1",
            "an ordinary ticket's address never moves"
        );
    }

    #[tokio::test]
    async fn a_foreign_suffix_is_not_laundered_at_establishment() {
        // The established hook's variant of the identity check: the
        // connection authenticated TRUSTED, and the address claims
        // someone else. The claim stays in the settlement key — the
        // policy records the literal that lied, never the bare route it
        // was lying about.
        let m = manager(&[TRUSTED]);
        let (mut g, in_flight) = gate(&m);
        behaviour_dial(&mut g, 4, TRUSTED).expect("admitted");
        const OTHER: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";
        let kept = established(
            &mut g,
            4,
            TRUSTED,
            &format!("/ip4/192.0.2.1/tcp/1/p2p/{OTHER}"),
        );
        assert!(kept.is_ok(), "no quarantine exists for that literal");
        let ticket = in_flight
            .settle(ConnectionId::new_unchecked(4))
            .expect("ours");
        assert_eq!(
            ticket.address(),
            format!("/ip4/192.0.2.1/tcp/1/p2p/{OTHER}"),
            "a claim naming another identity is not stripped into the bare route"
        );
    }

    #[test]
    fn the_peer_suffix_strip_is_trailing_only() {
        let plain: Multiaddr = "/ip4/192.0.2.1/tcp/1".parse().expect("valid");
        assert_eq!(strip_peer_suffix(&plain), "/ip4/192.0.2.1/tcp/1");
        let suffixed: Multiaddr = format!("/ip4/192.0.2.1/tcp/1/p2p/{TRUSTED}")
            .parse()
            .expect("valid");
        assert_eq!(strip_peer_suffix(&suffixed), "/ip4/192.0.2.1/tcp/1");
        // A relay path's INNER hop is part of the route, not a claim.
        let relayed: Multiaddr =
            format!("/ip4/192.0.2.1/tcp/1/p2p/{TRUSTED}/p2p-circuit/p2p/{TRUSTED}")
                .parse()
                .expect("valid");
        assert_eq!(
            strip_peer_suffix(&relayed),
            format!("/ip4/192.0.2.1/tcp/1/p2p/{TRUSTED}/p2p-circuit"),
            "only the trailing component is the dial's own peer claim"
        );
    }
}
