// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Swarm, with dialing reachable only through admission.
//!
//! # Why a wrapper and not a rule
//!
//! ADR-0011's exit gate says root admission is **the only** policy
//! authority for outbound Swarm dials. Written as a convention — "call
//! `admit` before `dial`" — that lasts exactly until someone adds a
//! second call site. This session has already watched the same shape
//! fail three times over: the /64 bucketing rule published as advice
//! nothing applied, a `source_bucket` helper documented as essential and
//! called by nothing, and then a bucket function that did not recognise
//! the string its only real caller holds.
//!
//! So the raw `Swarm` is private to this type and [`GatedSwarm::dial`] takes
//! an [`AdmittedDial`], which cannot be built without a
//! [`DialTicket`], which only [`PolicySnapshot::admit`] issues. A call
//! site that forgets to ask does not misbehave at runtime — it does not
//! compile.
//!
//! # What this does NOT cover
//!
//! Dials that a `NetworkBehaviour` originates from inside the Swarm.
//! Those never pass through this API at all; libp2p routes them through
//! `NetworkBehaviour::handle_pending_outbound_connection`, and that hook
//! is where the same ticket has to be required. Stage 4's behaviour set
//! is TCP, Noise, Yamux and Identify — none of which dials — so there is
//! nothing to gate there yet, and the honest statement is that this
//! closes the command path and the behaviour path is closed when the
//! first dialing behaviour arrives. Kademlia must not be enabled before
//! it is.
//!
//! [`PolicySnapshot::admit`]: interweave_transport_runtime::PolicySnapshot::admit

use futures::stream::SelectNextSome;
use libp2p::core::transport::ListenerId;
use libp2p::swarm::dial_opts::{DialOpts, PeerCondition};
use libp2p::swarm::{ConnectionId, DialError, Swarm, SwarmEvent};
use libp2p::{Multiaddr, PeerId, TransportError};

use interweave_transport_runtime::DialTicket;

use crate::behaviour::{SubstrateBehaviour, SubstrateBehaviourEvent};
use crate::outbound_gate::AdmittedDials;

/// A dial DERIVED from an admission.
///
/// # Pairing is not binding
///
/// The first version of this took a [`DialTicket`] and a `DialOpts`
/// side by side and checked neither against the other. A caller could
/// be admitted for a trusted peer at a known-good address, then hand
/// over options naming a different peer at a different address, and the
/// wrapper would carry it through — proving only that SOME admission
/// had happened, never that this dial was the one admitted. The outcome
/// was then recorded against the peer that was never dialled, so the
/// wrong address collected the success and the real one collected
/// nothing.
///
/// A gate that proves an unrelated admission is not a gate. So the
/// options are no longer accepted at all: they are BUILT here, from the
/// ticket's own peer and address, and there is no constructor that
/// takes them from outside. The dial and the admission cannot name
/// different destinations because only one of them names anything.
#[derive(Debug)]
#[must_use = "an admitted dial holds a pending-dial slot until it is executed or dropped"]
pub struct AdmittedDial {
    opts: DialOpts,
    ticket: DialTicket,
}

/// An admission that cannot be turned into a dial.
///
/// The ticket is returned so its slot is released by the caller rather
/// than leaked -- a rejected conversion is still a reservation someone
/// has to give back.
#[derive(Debug)]
pub struct UndialableAdmission {
    /// Why the ticket could not become a dial.
    pub reason: String,
    /// The unspent admission.
    pub ticket: DialTicket,
}

impl AdmittedDial {
    /// Build the dial this admission authorizes.
    ///
    /// # Errors
    /// Returns [`UndialableAdmission`], carrying the ticket back, when
    /// the admitted peer or address is not something libp2p can dial:
    /// a ticket with no peer, a peer that is not a `PeerId`, or an
    /// address that is not a `Multiaddr`. Those are refusals rather
    /// than panics because the strings reach the policy layer from
    /// configuration and from discovery.
    pub fn from_ticket(ticket: DialTicket) -> Result<Self, Box<UndialableAdmission>> {
        let Some(peer) = ticket.peer() else {
            return Err(Box::new(UndialableAdmission {
                reason: "the admission names no peer, so there is nothing to bind the dial to"
                    .to_owned(),
                ticket,
            }));
        };
        let Ok(expected) = peer.as_str().parse::<PeerId>() else {
            let reason = format!("admitted peer {} is not a libp2p PeerId", peer.as_str());
            return Err(Box::new(UndialableAdmission { reason, ticket }));
        };
        let Ok(address) = ticket.address().parse::<Multiaddr>() else {
            let reason = format!("admitted address {} is not a multiaddr", ticket.address());
            return Err(Box::new(UndialableAdmission { reason, ticket }));
        };

        // BOUND TO THE EXPECTED IDENTITY. Dialling a bare address tells
        // libp2p nothing about who should be there, so a server at that
        // address can complete a Noise handshake with any key and the
        // connection is accepted. Building the dial with the PeerId
        // makes the mismatch libp2p's problem, and is what produces the
        // `WrongPeerId` the runtime routes to quarantine.
        let opts = DialOpts::peer_id(expected)
            .addresses(vec![address])
            .condition(PeerCondition::Always)
            .build();
        Ok(Self { opts, ticket })
    }

    /// The connection this dial will use, known before it is made.
    ///
    /// The key the ticket is filed under, so the outcome event can find
    /// the admission it belongs to. Reading it here rather than after
    /// dialling matters: on a synchronous failure there is no event, and
    /// a ticket filed under an id nothing will ever report is a leaked
    /// slot.
    #[must_use]
    pub fn connection_id(&self) -> ConnectionId {
        self.opts.connection_id()
    }
}

/// The Swarm, with `dial` reachable only through [`AdmittedDial`].
///
/// Forwards exactly the operations the runtime uses. Deliberately not
/// `Deref`: dereferencing to the inner `Swarm` would hand back the
/// ungated `dial` and undo the whole point.
pub struct GatedSwarm {
    inner: Swarm<SubstrateBehaviour>,
    /// The other end of the outbound gate.
    ///
    /// Registering here is what lets an admitted dial through
    /// `OutboundAdmission`, so the two halves cannot drift: a dial that
    /// skipped this type would not be registered, and the behaviour
    /// refuses it.
    admitted: AdmittedDials,
}

impl core::fmt::Debug for GatedSwarm {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // `Swarm` is not `Debug`. The local identity is the only thing
        // worth printing and the only thing that is stable.
        f.debug_struct("GatedSwarm")
            .field("local_peer_id", self.inner.local_peer_id())
            .finish_non_exhaustive()
    }
}

/// The peer has no live connection to send over.
///
/// Distinct from every dial refusal: nothing was attempted, because
/// attempting it would mean a behaviour-originated dial the outbound
/// gate is required to refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotConnected;

/// The response channel is no longer answerable.
///
/// The connection that carried the request closed while it was being
/// decided. The local delivery still happened; the remote will not learn
/// that it did, and will retry into dedup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unanswerable;

impl GatedSwarm {
    /// Wrap a built Swarm.
    #[must_use]
    pub fn new(inner: Swarm<SubstrateBehaviour>) -> Self {
        let admitted = inner.behaviour().outbound.admitted();
        Self { inner, admitted }
    }

    /// Begin listening.
    ///
    /// # Errors
    /// Whatever the transport reports.
    pub fn listen_on(
        &mut self,
        address: Multiaddr,
    ) -> Result<ListenerId, TransportError<std::io::Error>> {
        self.inner.listen_on(address)
    }

    /// Close one connection.
    ///
    /// Ungated on purpose: refusing a connection is never the operation
    /// admission exists to constrain, and the ceiling needs a way to
    /// decline an inbound connection it cannot account for.
    /// Start one directed exchange with an ALREADY-CONNECTED peer.
    ///
    /// The single behaviour method this wrapper exposes, and it is
    /// narrow on purpose: a general `behaviour_mut` would hand every
    /// caller the ability to originate whatever a behaviour can, which
    /// is precisely what [`OutboundAdmission`](crate::outbound_gate)
    /// exists to prevent.
    ///
    /// # Why connectivity is a precondition rather than a convenience
    ///
    /// `send_request` DIALS when the peer is not connected, and that
    /// dial is behaviour-originated — so the outbound gate refuses it,
    /// correctly, because no ticket was issued for it. Rather than
    /// emitting a request whose implicit dial is guaranteed to be denied,
    /// this refuses up front and says so. `DIRECT.md` allows the
    /// ConnectionManager to dial under the command deadline; sequencing
    /// that admitted dial before the send is the caller's job, and doing
    /// it here would mean this method awaited, which the Swarm task
    /// cannot.
    ///
    /// # Errors
    /// [`NotConnected`] when the peer has no live connection.
    pub fn send_direct(
        &mut self,
        peer: &libp2p::PeerId,
        frame: interweave_transport_api::DirectMessageV2,
    ) -> Result<libp2p::request_response::OutboundRequestId, NotConnected> {
        if !self.inner.is_connected(peer) {
            return Err(NotConnected);
        }
        Ok(self.inner.behaviour_mut().direct.send_request(
            peer,
            crate::direct_codec::InboundRequest::Frame(Box::new(frame)),
        ))
    }

    /// Answer one inbound directed exchange.
    ///
    /// # Errors
    /// [`Unanswerable`] when the connection that carried the request is
    /// gone. SPIKE-002 finding 2: producing a response is not evidence
    /// the peer heard it, so this result is reported rather than
    /// discarded.
    pub fn answer_direct(
        &mut self,
        channel: libp2p::request_response::ResponseChannel<crate::direct_codec::DirectResponse>,
        response: crate::direct_codec::DirectResponse,
    ) -> Result<(), Unanswerable> {
        self.inner
            .behaviour_mut()
            .direct
            .send_response(channel, response)
            .map_err(|_| Unanswerable)
    }

    /// Close one connection by id.
    ///
    /// Returns whether the Swarm knew it. A connection this profile has
    /// decided not to keep is closed here rather than left open.
    pub fn close_connection(&mut self, id: ConnectionId) -> bool {
        self.inner.close_connection(id)
    }

    /// Stop a listener.
    pub fn remove_listener(&mut self, id: ListenerId) -> bool {
        self.inner.remove_listener(id)
    }

    /// The next Swarm event.
    pub fn select_next_some(&mut self) -> SelectNextSome<'_, Swarm<SubstrateBehaviour>>
    where
        Swarm<SubstrateBehaviour>:
            futures::Stream<Item = SwarmEvent<SubstrateBehaviourEvent>> + Unpin,
    {
        futures::StreamExt::select_next_some(&mut self.inner)
    }

    /// Dial, given an admission.
    ///
    /// Returns the ticket back on a SYNCHRONOUS failure. libp2p reports
    /// no event for a dial it refused outright, so a caller that filed
    /// the ticket and walked away would hold a pending-dial slot until
    /// the process ended — the resource bound decaying every time a
    /// malformed address is tried.
    ///
    /// # Errors
    /// The dial error, paired with the unspent admission.
    pub fn dial(
        &mut self,
        admitted: AdmittedDial,
    ) -> Result<DialTicket, Box<(DialError, DialTicket)>> {
        let AdmittedDial { opts, ticket } = admitted;
        // REGISTERED FIRST, and consumed inside the call. libp2p runs
        // `handle_pending_outbound_connection` synchronously within
        // `dial`, so this id is announced and spent in one statement --
        // the set is empty either side of it, and no other dial can
        // arrive in between to find an admission lying around.
        let id = opts.connection_id();
        self.admitted.register(id);
        let outcome = self.inner.dial(opts);
        // A dial libp2p refuses before the hook runs leaves the
        // registration behind. Unconditional because the successful
        // path has already consumed it and removing nothing is free.
        self.admitted.forget(id);
        match outcome {
            Ok(()) => Ok(ticket),
            // Boxed: `DialError` is large, and an unboxed error variant
            // would make every successful dial carry its size.
            Err(e) => Err(Box::new((e, ticket))),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::{AdmittedDial, PeerId};
    use interweave_transport_api::TransportIdentity;
    use interweave_transport_runtime::{
        ConnectionManager, ConnectionPolicy, DialOrigin, DialRequest, DialTicket, TrustSources,
    };
    use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};

    const ADMITTED: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";
    const OTHER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

    /// A manager whose snapshot admits, kept alive by the caller so the
    /// shared pending count outlives the tickets it issues.
    fn manager() -> ConnectionManager {
        let mut m = ConnectionManager::new(ConnectionPolicy::new(8, 8), 8);
        // The gate classifies from the trust sources now, so a fixture
        // that dials has to say who it trusts. `None` below is the
        // peerless case, which is unauthorized whatever this says.
        let _ = m.set_trust(
            TrustSources::new(
                PeerTrustPolicy::new([TransportIdentity::parse(ADMITTED).expect("canonical")])
                    .expect("one"),
                InfrastructureSet::default(),
            ),
            &[],
        );
        m
    }

    fn ticket_for(manager: &ConnectionManager, peer: Option<&str>, address: &str) -> DialTicket {
        let request = DialRequest {
            peer: peer.map(|p| TransportIdentity::parse(p).expect("canonical")),
            address: address.to_owned(),
            origin: DialOrigin::Manual,
        };
        manager
            .handle()
            .load()
            .admit(&request, 0)
            .expect("a fresh policy admits")
    }

    #[test]
    fn the_dial_names_the_peer_the_admission_named() {
        // THE BINDING. The earlier constructor took the options from
        // the caller, so a ticket admitted for one peer could carry a
        // dial to another and the outcome was filed against the peer
        // that was never dialled. Deriving them makes the two the same
        // fact. Break `from_ticket` to read anything but the ticket and
        // this fails.
        let manager = manager();
        let dial = AdmittedDial::from_ticket(ticket_for(
            &manager,
            Some(ADMITTED),
            "/ip4/192.0.2.1/tcp/4001",
        ))
        .expect("dialable");

        let expected: PeerId = ADMITTED.parse().expect("canonical");
        let other: PeerId = OTHER.parse().expect("canonical");
        assert_eq!(dial.opts.get_peer_id(), Some(expected));
        assert_ne!(dial.opts.get_peer_id(), Some(other));
    }

    // The ADDRESS half of the binding is proved over a socket, in
    // `tests/connectivity`: libp2p keeps `DialOpts::get_addresses`
    // crate-private, so the only honest observation of where the dial
    // went is the connection it makes. See
    // `the_dial_goes_to_the_admitted_address`.

    #[test]
    fn a_dial_that_names_no_peer_is_never_admitted() {
        // Which is why `from_ticket`'s peerless branch cannot be
        // reached through admission: there is no identity to classify,
        // and the gate answers `Unauthorized` rather than issuing a
        // ticket. The branch stays as a fail-closed guard on a type
        // whose `peer()` is an Option; this is the test that says the
        // guard is unreachable rather than untested.
        let manager = manager();
        let request = DialRequest {
            peer: None,
            address: "/ip4/192.0.2.1/tcp/4001".to_owned(),
            origin: DialOrigin::Manual,
        };
        assert_eq!(
            manager.handle().admit(&request, 0).err(),
            Some(interweave_transport_runtime::DialDenial::Unauthorized),
        );
    }

    #[test]
    fn an_admission_whose_address_is_not_a_multiaddr_is_refused() {
        // And the ticket comes BACK. A refusal that swallowed it would
        // leak a pending slot and a connection slot per malformed
        // address, and both ceilings would decay toward zero over the
        // life of the process.
        let manager = manager();
        let handle = manager.handle();
        let refused = AdmittedDial::from_ticket(ticket_for(&manager, Some(ADMITTED), "127.0.0.1"))
            .expect_err("not a multiaddr");
        assert!(refused.reason.contains("not a multiaddr"), "{refused:?}");
        assert_eq!(
            handle.load().pending_dials(),
            1,
            "the slot is still held by the returned ticket, not silently freed"
        );
        drop(refused);
        assert_eq!(handle.load().pending_dials(), 0, "and released on drop");
    }
}
