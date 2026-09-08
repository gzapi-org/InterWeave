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
