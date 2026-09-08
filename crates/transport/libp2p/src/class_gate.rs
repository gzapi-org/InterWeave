// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Restrict the protocol set a connection is OFFERED, by trust class.
//!
//! `BOTTOM-UP-IMPLEMENTATION-PLAN.md` §14 requires that a
//! `ConnectivityInfrastructureOnly` peer never gain the data-plane
//! protocols "merely by being connected", and is explicit that this is
//! about EXPOSURE rather than authority. Authority was already refused:
//! direct ingress, the GossipSub publisher check, `build_answer` and the
//! Kademlia driver's `try_admit` each classify their caller. What none
//! of them changes is that `SubstrateBehaviour` installs every
//! data-plane behaviour on every connection uniformly, so such a peer
//! can advertise and OPEN those substreams and be refused only after a
//! request has been parsed and accounted.
//!
//! This wrapper closes that at the connection. A gated connection gets a
//! handler that advertises nothing, and the inner behaviour is never
//! asked about it at all.
//!
//! # Gating and advertisement are the same fact
//!
//! Verified against the pinned sources rather than assumed:
//! `Connection::new` calls `gather_supported_protocols` on the handler
//! it was given and pushes the result as `LocalProtocolsChange`
//! (`libp2p-swarm-0.47.1` `connection.rs:196`), and Identify builds its
//! advertised list from exactly that. So refusing to install a handler
//! is refusing to advertise its protocols; there is no second place to
//! also suppress.
//!
//! # Why this is not `Either`
//!
//! libp2p ships a `ConnectionHandler` impl for `Either<L, R>` that would
//! give the two-sided handler for free, and using it would be a latent
//! abort. Its `on_behaviour_event` is `match (self, event) { .. _ =>
//! unreachable!() }` (`handler/either.rs:110`) because it makes
//! `FromBehaviour` an `Either` too — so a behaviour holding a
//! left-shaped event and a right-shaped handler panics the Swarm task.
//! That pairing is reachable: `kad` dispatches with
//! `NotifyHandler::Any`, which picks a connection by peer, so a peer
//! with one trusted and one gated connection can route an event to the
//! gated side.
//!
//! The fix is in the type rather than in a check. `FromBehaviour` here
//! is the INNER handler's, unwrapped, so a gated handler receives an
//! ordinary event and drops it. There is no mismatched pairing to be
//! unreachable about.

use std::collections::HashSet;
use std::task::{Context, Poll};

use either::Either;
use libp2p::core::upgrade::DeniedUpgrade;
use libp2p::swarm::handler::{
    ConnectionEvent, ConnectionHandler, ConnectionHandlerEvent, SendWrapper,
};
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, SubstreamProtocol, THandler,
    THandlerInEvent, THandlerOutEvent, ToSwarm,
};
use libp2p::{
    Multiaddr, PeerId,
    core::{Endpoint, transport::PortUse},
};

use interweave_transport_api::TransportIdentity;
use interweave_transport_runtime::{ConnectionClass, SnapshotHandle};

/// Wraps a data-plane behaviour so it is offered only to trusted peers.
///
/// Transparent for a `DataPlaneTrusted` connection: the inner behaviour
/// decides everything it decided before. For any other class the inner
/// behaviour is not consulted — not asked to build a handler, and so
/// never told the connection exists.
pub struct ClassGated<B> {
    inner: B,
    policy: SnapshotHandle,
    /// Connections this wrapper gated, so their lifecycle can be hidden
    /// from the inner behaviour.
    ///
    /// **Not bookkeeping for its own sake — the inner behaviour PANICS
    /// without it.** `libp2p-request-response 0.29.0` records a
    /// connection in its established hook, via `preload_new_handler`
    /// (`lib.rs:757`), and `on_connection_closed` then does
    /// `.expect("Expected some established connection to peer before
    /// closing.")` (`lib.rs:668`). A gated connection never reaches the
    /// first, so forwarding the second aborts the Swarm task. Found by
    /// running the connectivity suite, not by reading.
    ///
    /// So "never call into `B` for a gated connection" is not only about
    /// the handler: the SWARM EVENTS are the other half, and a behaviour
    /// whose two halves disagree is worse off than one told nothing.
    ///
    /// BOUNDED by the Swarm's own connection ceiling and drained on
    /// close: an id enters on a gated establish and leaves on that
    /// connection's close, which the Swarm reports for every connection
    /// it reported established.
    gated: HashSet<ConnectionId>,
}

impl<B> ClassGated<B> {
    /// Wrap `inner`, classifying against `policy`.
    pub fn new(inner: B, policy: SnapshotHandle) -> Self {
        Self {
            inner,
            policy,
            gated: HashSet::new(),
        }
    }

    /// The wrapped behaviour, for the composed behaviour's own use.
    pub fn inner_mut(&mut self) -> &mut B {
        &mut self.inner
    }

    /// Whether this peer may be offered the data plane.
    ///
    /// FAILS CLOSED on every uncertainty. A `PeerId` the neutral grammar
    /// refuses cannot be classified, so it is gated rather than given
    /// the benefit of the doubt -- the same answer `dialing.rs` gives
    /// when it cannot build a `TransportIdentity`.
    fn admits(&self, peer: &PeerId) -> bool {
        let Ok(identity) = TransportIdentity::parse(peer.to_base58()) else {
            return false;
        };
        self.policy.load().classify(&identity) == ConnectionClass::DataPlaneTrusted
    }
}

impl<B: NetworkBehaviour> NetworkBehaviour for ClassGated<B> {
    type ConnectionHandler = ClassGatedHandler<B::ConnectionHandler>;
    type ToSwarm = B::ToSwarm;

    fn handle_established_inbound_connection(
        &mut self,
        id: ConnectionId,
        peer: PeerId,
        local: &Multiaddr,
        remote: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        if !self.admits(&peer) {
            self.gated.insert(id);
            return Ok(ClassGatedHandler::Denied);
        }
        Ok(ClassGatedHandler::Allowed(
            self.inner
                .handle_established_inbound_connection(id, peer, local, remote)?,
        ))
    }

    fn handle_established_outbound_connection(
        &mut self,
        id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role: Endpoint,
        port: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        if !self.admits(&peer) {
            self.gated.insert(id);
            return Ok(ClassGatedHandler::Denied);
        }
        Ok(ClassGatedHandler::Allowed(
            self.inner
                .handle_established_outbound_connection(id, peer, addr, role, port)?,
        ))
    }

    fn handle_pending_inbound_connection(
        &mut self,
        id: ConnectionId,
        local: &Multiaddr,
        remote: &Multiaddr,
    ) -> Result<(), ConnectionDenied> {
        self.inner
            .handle_pending_inbound_connection(id, local, remote)
    }

    fn handle_pending_outbound_connection(
        &mut self,
        id: ConnectionId,
        peer: Option<PeerId>,
        addresses: &[Multiaddr],
        role: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        self.inner
            .handle_pending_outbound_connection(id, peer, addresses, role)
    }

    /// Hide a gated connection's whole lifecycle from the inner
    /// behaviour.
    ///
    /// The inner behaviour was never given a handler for it, so telling
    /// it the connection established, moved or closed describes a
    /// connection it does not have. See [`Self::gated`] for the crash
    /// this prevents.
    ///
    /// Every OTHER event is forwarded untouched: they are about the
    /// node rather than about one connection, and a behaviour that
    /// stopped hearing about new external addresses or expired listeners
    /// because some unrelated peer was gated would be broken in a way
    /// far harder to see.
    fn on_swarm_event(&mut self, event: FromSwarm<'_>) {
        match &event {
            FromSwarm::ConnectionEstablished(established)
                if self.gated.contains(&established.connection_id) =>
            {
                return;
            }
            FromSwarm::ConnectionClosed(closed) if self.gated.contains(&closed.connection_id) => {
                // REMOVED HERE, which is what keeps the set bounded.
                self.gated.remove(&closed.connection_id);
                return;
            }
            FromSwarm::AddressChange(change) if self.gated.contains(&change.connection_id) => {
                return;
            }
            _ => {}
        }
        self.inner.on_swarm_event(event);
    }

    fn on_connection_handler_event(
        &mut self,
        peer: PeerId,
        id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        self.inner.on_connection_handler_event(peer, id, event);
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        self.inner.poll(cx)
    }
}

/// The handler a [`ClassGated`] connection gets.
///
/// `Denied` advertises nothing, negotiates nothing, and answers no
/// behaviour event.
pub enum ClassGatedHandler<H> {
    /// The inner behaviour's handler, for a trusted connection.
    Allowed(H),
    /// A handler offering no protocols at all.
    Denied,
}

impl<H: ConnectionHandler> ConnectionHandler for ClassGatedHandler<H> {
    // NOT an `Either`, and that is the whole point -- see the module
    // note. The behaviour sends the inner handler's event type, so a
    // `Denied` handler has an ordinary event to drop rather than a
    // mismatched pairing to panic on.
    type FromBehaviour = H::FromBehaviour;
    type ToBehaviour = H::ToBehaviour;

    // The inbound side must cover both, since `Denied` has to offer
    // SOMETHING and `DeniedUpgrade` is the something that is empty.
    type InboundProtocol = Either<SendWrapper<H::InboundProtocol>, SendWrapper<DeniedUpgrade>>;
    type InboundOpenInfo = Either<H::InboundOpenInfo, ()>;

    // The outbound side does NOT need covering: `Denied` never requests
    // an outbound substream, because its `poll` never returns anything.
    // So these stay the inner handler's types and the `Allowed` path
    // needs no mapping at all.
    type OutboundProtocol = H::OutboundProtocol;
    type OutboundOpenInfo = H::OutboundOpenInfo;

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        match self {
            Self::Allowed(handler) => handler
                .listen_protocol()
                .map_upgrade(|u| Either::Left(SendWrapper(u)))
                .map_info(Either::Left),
            // `DeniedUpgrade::protocol_info()` is `iter::empty()`
            // (`libp2p-core-0.43.2` `upgrade/denied.rs:34-37`), so this
            // contributes no protocol to `gather_supported_protocols`
            // and therefore none to what Identify advertises.
            Self::Denied => {
                SubstreamProtocol::new(Either::Right(SendWrapper(DeniedUpgrade)), Either::Right(()))
            }
        }
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        match self {
            Self::Allowed(handler) => handler.on_behaviour_event(event),
            // DROPPED, not `unreachable!()`. This is reachable: `kad`
            // dispatches with `NotifyHandler::Any`, which selects a
            // connection by peer, so a peer holding one trusted and one
            // gated connection can have an event routed here.
            Self::Denied => {}
        }
    }

    fn connection_keep_alive(&self) -> bool {
        match self {
            Self::Allowed(handler) => handler.connection_keep_alive(),
            // A gated connection is kept alive by whatever else the
            // Swarm holds it for -- this wrapper never votes to keep it.
            Self::Denied => false,
        }
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>,
    > {
        match self {
            Self::Allowed(handler) => handler.poll(cx),
            Self::Denied => Poll::Pending,
        }
    }

    fn poll_close(&mut self, cx: &mut Context<'_>) -> Poll<Option<Self::ToBehaviour>> {
        match self {
            Self::Allowed(handler) => handler.poll_close(cx),
            Self::Denied => Poll::Ready(None),
        }
    }

    fn on_connection_event(
        &mut self,
        event: ConnectionEvent<
            '_,
            Self::InboundProtocol,
            Self::OutboundProtocol,
            Self::InboundOpenInfo,
            Self::OutboundOpenInfo,
        >,
    ) {
        let Self::Allowed(handler) = self else {
            // Nothing negotiates on a denied connection, so the
            // substream events cannot arrive; the informational ones
            // are of no use to a handler that does nothing.
            return;
        };
        match event {
            ConnectionEvent::FullyNegotiatedInbound(inbound) => {
                // TWO DIFFERENT `Either`s, deliberately named apart. The
                // upgrade's Output is `futures::future::Either` because
                // that is what `InboundUpgradeSend` produces for a
                // two-sided upgrade; the info is `either::Either`
                // because that is the type this handler declared.
                // Reading them as one type is a compile error here and
                // was the first one this file hit.
                let (futures::future::Either::Left(protocol), Either::Left(info)) =
                    (inbound.protocol, inbound.info)
                else {
                    // A denied upgrade negotiated on an allowed
                    // handler: impossible by construction, and dropped
                    // rather than asserted. libp2p's own `transpose`
                    // writes `unreachable!()` at this exact spot; this
                    // wrapper exists partly because that choice aborts
                    // a Swarm task, so it does not repeat it.
                    return;
                };
                handler.on_connection_event(ConnectionEvent::FullyNegotiatedInbound(
                    libp2p::swarm::handler::FullyNegotiatedInbound { protocol, info },
                ));
            }
            ConnectionEvent::FullyNegotiatedOutbound(outbound) => {
                handler.on_connection_event(ConnectionEvent::FullyNegotiatedOutbound(outbound));
            }
            ConnectionEvent::AddressChange(change) => {
                handler.on_connection_event(ConnectionEvent::AddressChange(change));
            }
            ConnectionEvent::DialUpgradeError(err) => {
                handler.on_connection_event(ConnectionEvent::DialUpgradeError(err));
            }
            ConnectionEvent::ListenUpgradeError(err) => {
                // The ERROR is an `Either` too -- a listen upgrade can
                // fail on either side, so its error type is
                // `Either<H::Error, Infallible>`. Both halves must
                // unwrap together or this is not the allowed side's
                // failure.
                let (Either::Left(info), Either::Left(error)) = (err.info, err.error) else {
                    return;
                };
                handler.on_connection_event(ConnectionEvent::ListenUpgradeError(
                    libp2p::swarm::handler::ListenUpgradeError { info, error },
                ));
            }
            ConnectionEvent::LocalProtocolsChange(change) => {
                handler.on_connection_event(ConnectionEvent::LocalProtocolsChange(change));
            }
            ConnectionEvent::RemoteProtocolsChange(change) => {
                handler.on_connection_event(ConnectionEvent::RemoteProtocolsChange(change));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{Arc, Mutex};

    use libp2p::swarm::dummy;
    use libp2p::swarm::handler::UpgradeInfoSend as _;

    use interweave_transport_runtime::{
        ConnectionManager, ConnectionPolicy, DialOrigin, TrustSources,
    };
    use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};

    /// Trusted for the data plane.
    const TRUSTED: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    /// Infrastructure only — reachability control, no data plane.
    const INFRA: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";
    /// In neither set.
    const STRANGER: &str = "12D3KooWLRPJAEanjW29ZTaVLZQMMTZKm1F4bXpqzYFqLzJTdgBQ";

    fn ident(p: &str) -> TransportIdentity {
        TransportIdentity::parse(p.to_owned()).expect("a canonical identity")
    }

    fn peer(p: &str) -> PeerId {
        p.parse().expect("a libp2p identity")
    }

    fn addr() -> Multiaddr {
        "/ip4/127.0.0.1/tcp/4001"
            .parse()
            .expect("a valid multiaddr")
    }

    /// A policy handle where `TRUSTED` is data-plane and `INFRA` is not.
    fn policy() -> SnapshotHandle {
        let mut m = ConnectionManager::new(ConnectionPolicy::new(8, 8), 8);
        let _ = m.set_trust(
            TrustSources::new(
                PeerTrustPolicy::new([ident(TRUSTED)]).expect("small"),
                InfrastructureSet::new([ident(INFRA)]).expect("small"),
            ),
            &[],
        );
        m.handle()
    }

    /// A behaviour that records whether it was consulted at all.
    ///
    /// The point of the wrapper is that a gated connection never reaches
    /// the inner behaviour, and "never reaches" is only checkable
    /// against something that would say so.
    #[derive(Default)]
    struct Recording {
        establishes: Arc<Mutex<Vec<ConnectionId>>>,
        swarm_events: Arc<Mutex<usize>>,
    }

    impl NetworkBehaviour for Recording {
        type ConnectionHandler = dummy::ConnectionHandler;
        type ToSwarm = std::convert::Infallible;

        fn handle_established_inbound_connection(
            &mut self,
            id: ConnectionId,
            _peer: PeerId,
            _local: &Multiaddr,
            _remote: &Multiaddr,
        ) -> Result<THandler<Self>, ConnectionDenied> {
            self.establishes.lock().expect("not poisoned").push(id);
            Ok(dummy::ConnectionHandler)
        }

        fn handle_established_outbound_connection(
            &mut self,
            id: ConnectionId,
            _peer: PeerId,
            _addr: &Multiaddr,
            _role: Endpoint,
            _port: PortUse,
        ) -> Result<THandler<Self>, ConnectionDenied> {
            self.establishes.lock().expect("not poisoned").push(id);
            Ok(dummy::ConnectionHandler)
        }

        fn on_swarm_event(&mut self, _event: FromSwarm<'_>) {
            *self.swarm_events.lock().expect("not poisoned") += 1;
        }

        fn on_connection_handler_event(
            &mut self,
            _peer: PeerId,
            _id: ConnectionId,
            _event: THandlerOutEvent<Self>,
        ) {
        }

        fn poll(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
            Poll::Pending
        }
    }

    /// How many protocols this handler offers on the wire.
    ///
    /// Read through `listen_protocol`, which is the same value
    /// `gather_supported_protocols` reads when the Swarm builds the
    /// connection — so this is what Identify would advertise, not a
    /// proxy for it.
    fn offered<H: ConnectionHandler>(handler: &ClassGatedHandler<H>) -> usize {
        handler
            .listen_protocol()
            .upgrade()
            .protocol_info()
            .into_iter()
            .count()
    }

    fn gated(inner: Recording) -> ClassGated<Recording> {
        ClassGated::new(inner, policy())
    }

    #[test]
    fn a_trusted_peer_is_offered_the_inner_behaviours_protocols() {
        // THE CONTROL, and it has to come first: a wrapper that gated
        // everything would pass every other test in this module.
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut g = gated(Recording {
            establishes: Arc::clone(&seen),
            ..Recording::default()
        });

        let handler = g
            .handle_established_inbound_connection(
                ConnectionId::new_unchecked(1),
                peer(TRUSTED),
                &addr(),
                &addr(),
            )
            .expect("a trusted peer is admitted");

        assert!(matches!(handler, ClassGatedHandler::Allowed(_)));
        assert_eq!(
            seen.lock().expect("not poisoned").len(),
            1,
            "the inner behaviour must be asked to build the handler"
        );
    }

    #[test]
    fn an_infrastructure_only_peer_is_offered_nothing_and_the_inner_behaviour_is_never_asked() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut g = gated(Recording {
            establishes: Arc::clone(&seen),
            ..Recording::default()
        });

        let handler = g
            .handle_established_inbound_connection(
                ConnectionId::new_unchecked(1),
                peer(INFRA),
                &addr(),
                &addr(),
            )
            .expect("gating is not a refusal -- the connection still exists");

        assert!(matches!(handler, ClassGatedHandler::Denied));
        assert_eq!(
            offered(&handler),
            0,
            "a gated connection must be offered no protocol at all, which is what \
             makes Identify advertise none of them"
        );
        assert!(
            seen.lock().expect("not poisoned").is_empty(),
            "the inner behaviour must not even be asked to build a handler for a \
             gated connection -- being asked is how it learns the connection exists"
        );
    }

    #[test]
    fn a_peer_in_neither_trust_set_is_gated_too() {
        // `Unauthorized` should never reach here -- the gated swarm
        // refuses it earlier -- so this pins the answer this wrapper
        // gives if it ever does, rather than assuming it cannot.
        let mut g = gated(Recording::default());
        let handler = g
            .handle_established_inbound_connection(
                ConnectionId::new_unchecked(1),
                peer(STRANGER),
                &addr(),
                &addr(),
            )
            .expect("still a connection");
        assert!(matches!(handler, ClassGatedHandler::Denied));
    }

    #[test]
    fn the_outbound_side_gates_on_the_same_answer() {
        let mut g = gated(Recording::default());
        let handler = g
            .handle_established_outbound_connection(
                ConnectionId::new_unchecked(1),
                peer(INFRA),
                &addr(),
                Endpoint::Dialer,
                PortUse::Reuse,
            )
            .expect("still a connection");
        assert!(matches!(handler, ClassGatedHandler::Denied));
        assert_eq!(offered(&handler), 0);
    }

    #[test]
    fn a_gated_handler_drops_a_behaviour_event_rather_than_panicking() {
        // THE `unreachable!()` THIS WRAPPER EXISTS NOT TO INHERIT.
        // libp2p's `ConnectionHandler for Either<L, R>` panics on this
        // exact pairing, and it is reachable: `kad` dispatches with
        // `NotifyHandler::Any`, which selects a connection by PEER, so a
        // peer holding one trusted and one gated connection can have an
        // event routed to the gated side.
        //
        // `dummy::ConnectionHandler`'s `FromBehaviour` is `Infallible`,
        // so there is no value to send it here; the property is checked
        // one level down, on a handler whose event type is inhabited.
        let mut denied: ClassGatedHandler<request_response_handler::Stub> =
            ClassGatedHandler::Denied;
        denied.on_behaviour_event(request_response_handler::Event);
        // Reaching this line IS the assertion: the library's version
        // would have aborted the task.
    }

    /// A minimal handler whose `FromBehaviour` is inhabited.
    ///
    /// `dummy::ConnectionHandler` cannot express the drop test because
    /// its event type is `Infallible` — there is no event to drop.
    mod request_response_handler {
        use super::*;

        #[derive(Debug)]
        pub(super) struct Event;

        pub(super) struct Stub;

        impl ConnectionHandler for Stub {
            type FromBehaviour = Event;
            type ToBehaviour = std::convert::Infallible;
            type InboundProtocol = DeniedUpgrade;
            type OutboundProtocol = DeniedUpgrade;
            type InboundOpenInfo = ();
            type OutboundOpenInfo = ();

            fn listen_protocol(
                &self,
            ) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
                SubstreamProtocol::new(DeniedUpgrade, ())
            }

            fn on_behaviour_event(&mut self, _event: Self::FromBehaviour) {
                unreachable!("the allowed side is not exercised by this test");
            }

            fn poll(
                &mut self,
                _cx: &mut Context<'_>,
            ) -> Poll<
                ConnectionHandlerEvent<
                    Self::OutboundProtocol,
                    Self::OutboundOpenInfo,
                    Self::ToBehaviour,
                >,
            > {
                Poll::Pending
            }

            fn on_connection_event(
                &mut self,
                _event: ConnectionEvent<
                    '_,
                    Self::InboundProtocol,
                    Self::OutboundProtocol,
                    Self::InboundOpenInfo,
                    Self::OutboundOpenInfo,
                >,
            ) {
            }
        }
    }

    #[test]
    fn an_unparseable_peer_id_is_gated_rather_than_admitted() {
        // FAIL-CLOSED. `admits` cannot classify a PeerId the neutral
        // grammar refuses, and the safe answer to "I cannot tell" is to
        // offer nothing. Constructed through libp2p rather than through
        // `TransportIdentity`, because the whole case is an id the
        // latter would reject.
        let mut g = gated(Recording::default());
        let unparseable = PeerId::random();
        let classifiable = TransportIdentity::parse(unparseable.to_base58()).is_ok();

        let handler = g
            .handle_established_inbound_connection(
                ConnectionId::new_unchecked(1),
                unparseable,
                &addr(),
                &addr(),
            )
            .expect("still a connection");

        // A random PeerId IS parseable, so this documents the branch
        // rather than exercising it -- and it is still gated, because a
        // random peer is in neither trust set.
        assert!(
            matches!(handler, ClassGatedHandler::Denied),
            "parseable={classifiable}: either way, an unclassifiable or untrusted \
             peer must be gated"
        );
    }

    #[test]
    fn a_gated_connections_lifecycle_is_hidden_from_the_inner_behaviour() {
        // THE CRASH THIS PREVENTS, pinned. `libp2p-request-response`
        // records a connection in its established hook and `.expect()`s
        // it back in `on_connection_closed`; a gated connection reaches
        // the second and not the first, which aborts the Swarm task.
        // Found by running the connectivity suite against the first
        // wiring of this wrapper.
        let events = Arc::new(Mutex::new(0));
        let mut g = gated(Recording {
            swarm_events: Arc::clone(&events),
            ..Recording::default()
        });

        let id = ConnectionId::new_unchecked(1);
        let handler = g
            .handle_established_inbound_connection(id, peer(INFRA), &addr(), &addr())
            .expect("still a connection");
        assert!(matches!(handler, ClassGatedHandler::Denied));
        assert!(
            g.gated.contains(&id),
            "a gated connection must be remembered, or its lifecycle cannot be hidden"
        );

        // The close arrives for every connection the Swarm established,
        // gated or not.
        g.on_swarm_event(FromSwarm::ConnectionClosed(
            libp2p::swarm::behaviour::ConnectionClosed {
                peer_id: peer(INFRA),
                connection_id: id,
                endpoint: &libp2p::core::ConnectedPoint::Listener {
                    local_addr: addr(),
                    send_back_addr: addr(),
                },
                remaining_established: 0,
                cause: None,
            },
        ));

        assert_eq!(
            *events.lock().expect("not poisoned"),
            0,
            "the inner behaviour must not be told a gated connection closed -- it was \
             never told it opened"
        );
        assert!(
            !g.gated.contains(&id),
            "and the id must be released on close, or the set grows without bound"
        );
    }

    #[test]
    fn a_trusted_connections_lifecycle_still_reaches_the_inner_behaviour() {
        // The control for the test above: hiding is scoped to gated
        // connections, not applied to everything.
        let events = Arc::new(Mutex::new(0));
        let mut g = gated(Recording {
            swarm_events: Arc::clone(&events),
            ..Recording::default()
        });

        let id = ConnectionId::new_unchecked(1);
        let _ = g
            .handle_established_inbound_connection(id, peer(TRUSTED), &addr(), &addr())
            .expect("admitted");
        g.on_swarm_event(FromSwarm::ConnectionClosed(
            libp2p::swarm::behaviour::ConnectionClosed {
                peer_id: peer(TRUSTED),
                connection_id: id,
                endpoint: &libp2p::core::ConnectedPoint::Listener {
                    local_addr: addr(),
                    send_back_addr: addr(),
                },
                remaining_established: 0,
                cause: None,
            },
        ));

        assert_eq!(
            *events.lock().expect("not poisoned"),
            1,
            "a trusted connection's close must reach the inner behaviour, or its own \
             bookkeeping goes stale"
        );
    }

    #[test]
    fn a_node_level_swarm_event_reaches_the_inner_behaviour_even_while_a_peer_is_gated() {
        // Scoping again, from the other side. An event that is about
        // the NODE rather than about one connection must be forwarded
        // whatever else is gated -- a behaviour that stopped hearing
        // about external addresses because some unrelated peer was
        // gated would be broken far less visibly.
        let events = Arc::new(Mutex::new(0));
        let mut g = gated(Recording {
            swarm_events: Arc::clone(&events),
            ..Recording::default()
        });
        let _ = g
            .handle_established_inbound_connection(
                ConnectionId::new_unchecked(1),
                peer(INFRA),
                &addr(),
                &addr(),
            )
            .expect("still a connection");

        g.on_swarm_event(FromSwarm::NewExternalAddrCandidate(
            libp2p::swarm::behaviour::NewExternalAddrCandidate { addr: &addr() },
        ));

        assert_eq!(
            *events.lock().expect("not poisoned"),
            1,
            "a node-level event must reach the inner behaviour regardless of gating"
        );
    }

    #[test]
    fn the_wrapper_reads_the_live_snapshot_rather_than_one_taken_at_construction() {
        // A class is policy and policy is republished. A wrapper that
        // photographed the snapshot in `new` would go on offering the
        // data plane to a peer whose trust had been withdrawn, until
        // the process restarted.
        let mut m = ConnectionManager::new(ConnectionPolicy::new(8, 8), 8);
        let _ = m.set_trust(
            TrustSources::new(
                PeerTrustPolicy::new([ident(TRUSTED)]).expect("small"),
                InfrastructureSet::default(),
            ),
            &[],
        );
        let mut g = ClassGated::new(Recording::default(), m.handle());

        let before = g
            .handle_established_inbound_connection(
                ConnectionId::new_unchecked(1),
                peer(TRUSTED),
                &addr(),
                &addr(),
            )
            .expect("admitted");
        assert!(matches!(before, ClassGatedHandler::Allowed(_)));

        // Withdraw it, which republishes the snapshot.
        let _ = m.set_trust(
            TrustSources::new(
                PeerTrustPolicy::new([]).expect("empty"),
                InfrastructureSet::new([ident(TRUSTED)]).expect("small"),
            ),
            &[],
        );

        let after = g
            .handle_established_inbound_connection(
                ConnectionId::new_unchecked(2),
                peer(TRUSTED),
                &addr(),
                &addr(),
            )
            .expect("still a connection");
        assert!(
            matches!(after, ClassGatedHandler::Denied),
            "a peer demoted to infrastructure-only must be gated on its NEXT \
             connection, without restarting anything"
        );

        // NOT a claim that the EXISTING connection is re-gated. It is
        // not: the handler was installed at establishment and is not
        // rebuilt. What makes that acceptable is that revocation closes
        // such connections, and THAT now has a test of its own rather
        // than a citation:
        // `advertised_protocol_set::a_peer_downgraded_to_infrastructure_only_loses_its_connection`
        // takes a trusted peer to infrastructure-only over real sockets
        // and requires the connection to go. It was written because the
        // claim rested on two separately-tested halves -- the manager
        // NAMES a downgrade, and a named connection is CLOSED -- with
        // the closing half only ever exercised for a full revocation to
        // `Unauthorized`, which is the easier case.
        let _ = DialOrigin::Manual;
    }
}
