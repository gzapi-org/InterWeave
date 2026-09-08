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

use std::collections::{HashMap, HashSet};
use std::task::{Context, Poll};

use either::Either;
use libp2p::core::upgrade::DeniedUpgrade;
use libp2p::swarm::handler::{
    ConnectionEvent, ConnectionHandler, ConnectionHandlerEvent, SendWrapper,
};
use libp2p::swarm::{
    CloseConnection, ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour,
    SubstreamProtocol, THandler, THandlerInEvent, THandlerOutEvent, ToSwarm,
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
    /// close, on a listen failure and on a dial failure. The last two
    /// matter structurally rather than in practice: the derive
    /// `?`-propagates, so a field AFTER this one returning
    /// `ConnectionDenied` means the Swarm reports `ListenFailure` or
    /// `DialFailure` and never `ConnectionEstablished`, and the entry
    /// would be permanent. No behaviour after these ones refuses today;
    /// §6 asks that the bound not rest on that.
    ///
    /// A MAP RATHER THAN A SET, because the peer is needed to correct
    /// the counters the Swarm computes — see [`Self::hidden_for`].
    gated: HashMap<ConnectionId, PeerId>,
    /// The revision of the snapshot `gated` was last checked against, so
    /// a re-check costs nothing while policy is unchanged.
    checked_revision: Option<u64>,
    /// Connections already queued for closure, so the walk does not
    /// queue one twice.
    ///
    /// **Separate from removing it from [`Self::gated`], and that
    /// distinction is a crash.** An earlier version dequeued by removing
    /// the entry at PUSH time — but the connection is still open then,
    /// so the eventual `ConnectionClosed` found no entry, fell through
    /// to the forwarding arm, and handed the inner behaviour a close for
    /// a connection it was never given a handler for. That is exactly
    /// the `libp2p-request-response` `.expect()` panic `gated` exists to
    /// prevent, re-opened by the fix for something else. Review finding
    /// on PR #77.
    close_queued: HashSet<ConnectionId>,
    /// Connections to close because their peer's gating decision moved.
    ///
    /// Drained one per `poll`, which is how a `NetworkBehaviour`
    /// returns work.
    closing: Vec<(PeerId, ConnectionId)>,
}

impl<B> ClassGated<B> {
    /// Wrap `inner`, classifying against `policy`.
    pub fn new(inner: B, policy: SnapshotHandle) -> Self {
        Self {
            inner,
            policy,
            gated: HashMap::new(),
            checked_revision: None,
            close_queued: HashSet::new(),
            closing: Vec::new(),
        }
    }

    /// The wrapped behaviour, for the composed behaviour's own use.
    pub fn inner_mut(&mut self) -> &mut B {
        &mut self.inner
    }

    /// How many connections to `peer` this wrapper is currently hiding.
    ///
    /// The Swarm counts connections from its own pool, which knows
    /// nothing about gating, so `other_established` and
    /// `remaining_established` include the hidden ones. Forwarding those
    /// numbers unaltered tells an inner behaviour it has connections it
    /// was never given — the mirror of the crash that made hiding
    /// necessary, and worse, because it does not announce itself.
    fn hidden_for(&self, peer: &PeerId) -> usize {
        self.gated.values().filter(|p| *p == peer).count()
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
            self.gated.insert(id, peer);
            return Ok(ClassGatedHandler::Denied);
        }
        let handler = self
            .inner
            .handle_established_inbound_connection(id, peer, local, remote)?;
        Ok(ClassGatedHandler::Allowed(handler))
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
            self.gated.insert(id, peer);
            return Ok(ClassGatedHandler::Denied);
        }
        let handler = self
            .inner
            .handle_established_outbound_connection(id, peer, addr, role, port)?;
        Ok(ClassGatedHandler::Allowed(handler))
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
        match event {
            FromSwarm::ConnectionEstablished(established) => {
                if self.gated.contains_key(&established.connection_id) {
                    return;
                }
                // THE COUNT MUST BE CORRECTED, not just the event
                // suppressed. `other_established` comes from the Swarm's
                // connection pool, which does not know this wrapper
                // exists, so it counts the hidden connections too.
                // Forwarded unaltered it tells the inner behaviour it
                // already has connections to this peer that it was never
                // given -- and `libp2p-kad` then skips
                // `connected_peers.insert` on the `== 0` branch and
                // dials a peer it is already connected to.
                let hidden = self.hidden_for(&established.peer_id);
                self.inner.on_swarm_event(FromSwarm::ConnectionEstablished(
                    libp2p::swarm::behaviour::ConnectionEstablished {
                        other_established: established.other_established.saturating_sub(hidden),
                        ..established
                    },
                ));
            }
            FromSwarm::ConnectionClosed(closed) => {
                if self.gated.remove(&closed.connection_id).is_some() {
                    // REMOVED HERE, which is what keeps the map bounded.
                    self.close_queued.remove(&closed.connection_id);
                    return;
                }
                // The same correction, and this is the arm where getting
                // it wrong PANICS rather than misbehaves.
                // `libp2p-request-response` asserts
                // `connections.is_empty() == (remaining_established == 0)`
                // (`lib.rs:678`), and `libp2p-gossipsub` keeps its peer
                // entry alive on the `!= 0` branch and later `.expect()`s
                // a non-empty connection vec (`behaviour.rs:3457`) -- a
                // release panic in the Swarm task, reached whenever a
                // peer holds one allowed and one gated connection and
                // the allowed one closes first.
                let hidden = self.hidden_for(&closed.peer_id);
                self.inner.on_swarm_event(FromSwarm::ConnectionClosed(
                    libp2p::swarm::behaviour::ConnectionClosed {
                        remaining_established: closed.remaining_established.saturating_sub(hidden),
                        ..closed
                    },
                ));
            }
            FromSwarm::AddressChange(change) => {
                if self.gated.contains_key(&change.connection_id) {
                    return;
                }
                self.inner.on_swarm_event(FromSwarm::AddressChange(change));
            }
            // A DIAL OR LISTEN FAILURE MEANS NO CONNECTION, so the id can
            // never reach the close that would otherwise drain it. Both
            // are forwarded — `Attributing` needs `DialFailure` to drain
            // its own note map — and the removal is what makes this
            // wrapper's bound structural rather than a property of which
            // behaviours happen to refuse.
            FromSwarm::DialFailure(failure) => {
                self.gated.remove(&failure.connection_id);
                self.close_queued.remove(&failure.connection_id);
                self.inner.on_swarm_event(event);
            }
            FromSwarm::ListenFailure(failure) => {
                self.gated.remove(&failure.connection_id);
                self.close_queued.remove(&failure.connection_id);
                self.inner.on_swarm_event(event);
            }
            _ => self.inner.on_swarm_event(event),
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer: PeerId,
        id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        self.inner.on_connection_handler_event(peer, id, event);
    }

    /// Close any GATED connection whose peer has since gained data-plane
    /// trust, then poll the inner behaviour.
    ///
    /// The other direction is not here, and the split is deliberate. A
    /// peer that LOSES data-plane trust is closed by
    /// `dialing::connections_to_close`, in the command path, so that
    /// `set_trust`'s count includes it — that count is ADR-0012's
    /// observable, and a closure this wrapper performed on its own would
    /// be invisible to it. A promotion is not a revocation and is not
    /// part of that count, so it is handled here.
    ///
    /// **Why a promotion needs closing at all**, when the peer is only
    /// under-privileged: a gated connection that survives a promotion
    /// leaves the peer holding a `Denied` handler and, once it connects
    /// again, an `Allowed` one. `kad` dispatches with
    /// `NotifyHandler::Any`, which selects by PEER, so the Swarm may
    /// route a query to the denied connection — where
    /// `on_behaviour_event` drops it, safely and silently, and the query
    /// times out with a usable connection sitting right there. Review
    /// finding on PR #77.
    ///
    /// ADR-0036 names that state and forbids it: close and re-establish
    /// "rather than allowing a transient privilege mix".
    ///
    /// CHECKED ONLY WHEN POLICY MOVES. `PolicySnapshot::revision`
    /// changes on publication, so an unchanged policy skips the walk. It
    /// is not free — the load is an `RwLock` read and an `Arc` clone,
    /// once per wrapper per poll — and "unchanged" is less common than
    /// it sounds, since `publish` fires on every dial outcome and every
    /// connection open and close.
    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        let snapshot = self.policy.load();
        if self.checked_revision != Some(snapshot.revision()) {
            self.checked_revision = Some(snapshot.revision());
            // COLLECTED FIRST, then moved out of `gated` in one pass.
            // Queueing without removing was a defect once already: `pop`
            // removed only the entry it returned, so a poll queued M and
            // dequeued 1 and the next revision queued the rest again.
            let promoted: Vec<(ConnectionId, PeerId)> = self
                .gated
                .iter()
                .filter(|(id, peer)| {
                    !self.close_queued.contains(*id)
                        && TransportIdentity::parse(peer.to_base58()).is_ok_and(|i| {
                            snapshot.classify(&i) == ConnectionClass::DataPlaneTrusted
                        })
                })
                .map(|(id, peer)| (*id, *peer))
                .collect();
            for (id, peer) in promoted {
                // THE ENTRY STAYS IN `gated` until the close actually
                // lands. It is still a hidden connection until then --
                // its counts must still be subtracted, and its
                // `ConnectionClosed` must still be swallowed.
                self.close_queued.insert(id);
                self.closing.push((peer, id));
            }
        }
        if let Some((peer_id, id)) = self.closing.pop() {
            // `close_queued` still holds the id, so a second publication
            // before the close lands cannot queue it again.
            return Poll::Ready(ToSwarm::CloseConnection {
                peer_id,
                connection: CloseConnection::One(id),
            });
        }
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
            // THE ENUM IS `#[non_exhaustive]`, so this arm exists for
            // variants that do not exist yet. All seven current ones are
            // handled above. **A libp2p bump that adds one will silently
            // stop forwarding it to ALLOWED connections** -- the ones
            // that must behave exactly as an unwrapped behaviour --
            // with no compile error. Check this arm on every libp2p
            // upgrade. (Its mirror in `on_swarm_event` falls through to
            // FORWARD, which is the safe default and needs no such
            // note.)
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

    use interweave_transport_runtime::{ConnectionManager, ConnectionPolicy, TrustSources};
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
    fn a_denied_handler_never_asks_to_open_an_outbound_substream() {
        // THE PREMISE OF A TYPE CHOICE, and it had no test.
        //
        // `OutboundProtocol` and `OutboundOpenInfo` are the INNER
        // handler's, uncovered by any `Either`, on the argument that a
        // `Denied` handler never requests an outbound substream. That is
        // load-bearing: if a future edit gives `Denied::poll` anything
        // to emit -- a metric, a close signal -- the type system will
        // happily let a gated connection open the inner behaviour's REAL
        // outbound protocol, `/interweave/direct/2.0.0` or `/meshsub/`,
        // on a connection this wrapper exists to keep bare. No compile
        // error, no other test. Review finding on PR #77.
        let mut denied: ClassGatedHandler<request_response_handler::Stub> =
            ClassGatedHandler::Denied;
        let mut cx = Context::from_waker(std::task::Waker::noop());
        assert!(
            matches!(denied.poll(&mut cx), Poll::Pending),
            "a denied handler must emit nothing at all -- an \
             `OutboundSubstreamRequest` here would carry the inner behaviour's own \
             protocol onto a gated connection"
        );
        assert!(
            !denied.connection_keep_alive(),
            "and must never vote to hold the connection open"
        );
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

    /// A `PeerId` the neutral grammar refuses.
    ///
    /// `TransportIdentity::parse` accepts only `12D3KooW`/`Qm` prefixes,
    /// while `PeerId` accepts any identity or sha2-256 multihash — so an
    /// identity multihash of an unusual length base58-encodes to neither
    /// prefix and drives `admits`'s parse branch directly. Built rather
    /// than hardcoded, so it cannot rot into a valid id.
    fn unparseable_peer() -> PeerId {
        let mut bytes = vec![0x00, 42];
        bytes.extend(std::iter::repeat_n(0xAB, 42));
        PeerId::from_bytes(&bytes).expect("an identity multihash is a valid PeerId")
    }

    #[test]
    fn an_unparseable_peer_id_is_gated_rather_than_admitted() {
        // FAIL-CLOSED. `admits` cannot classify a PeerId the neutral
        // grammar refuses, and the safe answer to "I cannot tell" is to
        // offer nothing. Constructed through libp2p rather than through
        // `TransportIdentity`, because the whole case is an id the
        // latter would reject.
        // The allowlist holds one peer, so an unparseable id is gated
        // by BOTH arms -- the parse and the classification. What makes
        // this test discriminating is not the allowlist but the
        // mutation: with the fail-closed arm returning `true` the parse
        // branch returns before classification ever runs, so the test
        // fails. (`PeerTrustPolicy` has no allow-all constructor, by
        // design, so "trusted for everyone the grammar accepts" is not
        // available and an earlier version of this comment claimed it
        // anyway.) An earlier
        // version used `PeerId::random()`, which IS parseable and is
        // gated by being in neither trust set -- so it passed with the
        // fail-closed arm mutated to `return true`, and documented the
        // branch instead of exercising it. Review finding on PR #77.
        let unparseable = unparseable_peer();
        assert!(
            TransportIdentity::parse(unparseable.to_base58()).is_err(),
            "the fixture must actually be unparseable, or this test proves nothing"
        );

        let mut m = ConnectionManager::new(ConnectionPolicy::new(8, 8), 8);
        let _ = m.set_trust(
            TrustSources::new(
                PeerTrustPolicy::new([ident(TRUSTED)]).expect("small"),
                InfrastructureSet::default(),
            ),
            &[],
        );
        let mut g = ClassGated::new(Recording::default(), m.handle());

        let handler = g
            .handle_established_inbound_connection(
                ConnectionId::new_unchecked(1),
                unparseable,
                &addr(),
                &addr(),
            )
            .expect("still a connection");

        assert!(
            matches!(handler, ClassGatedHandler::Denied),
            "a PeerId that cannot be classified must be gated -- 'I cannot tell' is \
             not a reason to offer the data plane"
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
            g.gated.contains_key(&id),
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
            !g.gated.contains_key(&id),
            "and the id must be released on close, or the map grows without bound"
        );
    }

    /// A `Recording` that keeps the counter it was told, not just a tally.
    ///
    /// The counts are the whole subject of the test below, so a double
    /// that only counted calls could not see the defect.
    #[derive(Default)]
    struct Counts {
        remaining: Arc<Mutex<Vec<usize>>>,
        other: Arc<Mutex<Vec<usize>>>,
    }

    impl NetworkBehaviour for Counts {
        type ConnectionHandler = dummy::ConnectionHandler;
        type ToSwarm = std::convert::Infallible;

        fn handle_established_inbound_connection(
            &mut self,
            _id: ConnectionId,
            _peer: PeerId,
            _local: &Multiaddr,
            _remote: &Multiaddr,
        ) -> Result<THandler<Self>, ConnectionDenied> {
            Ok(dummy::ConnectionHandler)
        }

        fn handle_established_outbound_connection(
            &mut self,
            _id: ConnectionId,
            _peer: PeerId,
            _addr: &Multiaddr,
            _role: Endpoint,
            _port: PortUse,
        ) -> Result<THandler<Self>, ConnectionDenied> {
            Ok(dummy::ConnectionHandler)
        }

        fn on_swarm_event(&mut self, event: FromSwarm<'_>) {
            match event {
                FromSwarm::ConnectionEstablished(e) => {
                    self.other
                        .lock()
                        .expect("not poisoned")
                        .push(e.other_established);
                }
                FromSwarm::ConnectionClosed(c) => {
                    self.remaining
                        .lock()
                        .expect("not poisoned")
                        .push(c.remaining_established);
                }
                _ => {}
            }
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

    fn connected_point() -> libp2p::core::ConnectedPoint {
        libp2p::core::ConnectedPoint::Listener {
            local_addr: addr(),
            send_back_addr: addr(),
        }
    }

    #[test]
    fn a_hidden_connection_is_subtracted_from_the_counts_the_inner_behaviour_is_told() {
        // THE SECOND HALF OF "NEVER CALL INTO B", and it is not the
        // handler and not the events -- it is the NUMBERS the events
        // carry. `other_established` and `remaining_established` come
        // from the Swarm's connection pool, which does not know this
        // wrapper exists, so they count the hidden connections too.
        //
        // Forwarded unaltered they are a lie in the dangerous
        // direction. `libp2p-request-response` asserts
        // `connections.is_empty() == (remaining_established == 0)`;
        // `libp2p-gossipsub` keeps its peer entry alive on the `!= 0`
        // branch and later `.expect()`s a non-empty connection vec --
        // a RELEASE panic in the Swarm task; `libp2p-kad` skips
        // `connected_peers.insert` on the `== 0` branch and dials a
        // peer it is already connected to.
        //
        // Review finding on PR #77. The first version of this wrapper
        // suppressed the events and left the counts alone, which is a
        // subtler version of the crash that made suppression necessary.
        let remaining = Arc::new(Mutex::new(Vec::new()));
        let other = Arc::new(Mutex::new(Vec::new()));
        let mut g = ClassGated::new(
            Counts {
                remaining: Arc::clone(&remaining),
                other: Arc::clone(&other),
            },
            policy(),
        );

        // THE SHAPE, and why it takes three phases. The correction is
        // per peer, so the interesting case needs ONE peer holding both
        // hidden and visible connections -- and a peer's class decides
        // every connection alike, so that state is only reachable by
        // changing the class between them. INFRA therefore takes two
        // gated connections first, is promoted, and then takes an
        // allowed one; TRUSTED is the control that must pass through
        // untouched throughout.
        let allowed_id = ConnectionId::new_unchecked(1);
        let gated_a = ConnectionId::new_unchecked(2);
        let gated_b = ConnectionId::new_unchecked(3);

        let _ = g
            .handle_established_inbound_connection(allowed_id, peer(TRUSTED), &addr(), &addr())
            .expect("trusted is admitted");
        let _ = g
            .handle_established_inbound_connection(gated_a, peer(INFRA), &addr(), &addr())
            .expect("still a connection");
        let _ = g
            .handle_established_inbound_connection(gated_b, peer(INFRA), &addr(), &addr())
            .expect("still a connection");

        // TRUSTED's own close must not be reduced by INFRA's hidden
        // connections: the correction is PER PEER, not global.
        //
        // The count here has to be NON-ZERO for that to be checkable.
        // An earlier version closed TRUSTED's only connection with
        // `remaining_established: 0` and asserted `0` -- which
        // `saturating_sub` clamps to `0` under a global `hidden_for`
        // too, so the assertion whose message claimed to prove
        // pass-through could not distinguish pass-through from any
        // subtraction at all. Review finding on PR #77; the
        // assertion-that-cannot-fail class, in the test written to pin
        // this exact property.
        let trusted_second = ConnectionId::new_unchecked(6);
        let _ = g
            .handle_established_inbound_connection(trusted_second, peer(TRUSTED), &addr(), &addr())
            .expect("trusted is admitted");
        g.on_swarm_event(FromSwarm::ConnectionClosed(
            libp2p::swarm::behaviour::ConnectionClosed {
                peer_id: peer(TRUSTED),
                connection_id: allowed_id,
                endpoint: &connected_point(),
                // TRUSTED's other connection is still open, and it is
                // not hidden -- so this must pass through untouched.
                remaining_established: 1,
                cause: None,
            },
        ));
        assert_eq!(
            *remaining.lock().expect("not poisoned"),
            vec![1],
            "a peer with no hidden connections must have its count passed through. A \
             global rather than per-peer `hidden_for` would subtract INFRA's two here \
             and report 0."
        );

        // Now the case that panics libp2p. Give INFRA an allowed
        // connection as well, by promoting it, and close that one while
        // its two gated connections are still open: the Swarm would say
        // `remaining_established == 2`, and the inner behaviour holds
        // none of them.
        let mut m = ConnectionManager::new(ConnectionPolicy::new(8, 8), 8);
        let _ = m.set_trust(
            TrustSources::new(
                PeerTrustPolicy::new([ident(INFRA)]).expect("small"),
                InfrastructureSet::default(),
            ),
            &[],
        );
        let promoted_id = ConnectionId::new_unchecked(4);
        // Swap the policy so INFRA is now trusted, then admit it.
        g.policy = m.handle();
        let _ = g
            .handle_established_inbound_connection(promoted_id, peer(INFRA), &addr(), &addr())
            .expect("now trusted");

        remaining.lock().expect("not poisoned").clear();
        g.on_swarm_event(FromSwarm::ConnectionClosed(
            libp2p::swarm::behaviour::ConnectionClosed {
                peer_id: peer(INFRA),
                connection_id: promoted_id,
                endpoint: &connected_point(),
                // What the Swarm's pool would report: the two gated
                // connections are still open.
                remaining_established: 2,
                cause: None,
            },
        ));
        assert_eq!(
            *remaining.lock().expect("not poisoned"),
            vec![0],
            "the inner behaviour holds no other connection to this peer, so it must be \
             told zero -- told 2 it keeps a peer entry with an empty connection vec and \
             `libp2p-gossipsub` later panics on `.expect()`"
        );

        // And the same correction on the establish side, which is where
        // `libp2p-kad` silently misbehaves rather than panicking.
        let second_allowed = ConnectionId::new_unchecked(5);
        let _ = g
            .handle_established_inbound_connection(second_allowed, peer(INFRA), &addr(), &addr())
            .expect("trusted");
        g.on_swarm_event(FromSwarm::ConnectionEstablished(
            libp2p::swarm::behaviour::ConnectionEstablished {
                peer_id: peer(INFRA),
                connection_id: second_allowed,
                endpoint: &connected_point(),
                failed_addresses: &[],
                // Pool count: two gated still open, and no allowed one.
                other_established: 2,
            },
        ));
        assert_eq!(
            *other.lock().expect("not poisoned"),
            vec![0],
            "the inner behaviour has no other connection to this peer, so it must be \
             told zero -- told 2, `libp2p-kad` skips `connected_peers.insert` and dials \
             a peer it is already connected to"
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
    fn a_gated_id_is_released_when_the_connection_never_establishes() {
        // THE BOUND, made structural rather than assumed.
        //
        // An id enters `gated` at the established HOOK, which is not the
        // same event as establishment. The derive `?`-propagates, so a
        // behaviour field AFTER this one returning `ConnectionDenied`
        // means the Swarm reports `ListenFailure` or `DialFailure` and
        // never `ConnectionEstablished` or `ConnectionClosed` for that
        // id -- and the entry would be permanent.
        //
        // No behaviour after these ones refuses today, which makes this
        // unreachable by COMPOSITION rather than by construction: a
        // property of three third-party crates and a field ordering.
        // §6 asks that a bound not rest on that. Review finding on
        // PR #77.
        let mut g = gated(Recording::default());
        let listen_id = ConnectionId::new_unchecked(1);
        let dial_id = ConnectionId::new_unchecked(2);

        let _ = g
            .handle_established_inbound_connection(listen_id, peer(INFRA), &addr(), &addr())
            .expect("still a connection");
        let _ = g
            .handle_established_outbound_connection(
                dial_id,
                peer(INFRA),
                &addr(),
                Endpoint::Dialer,
                PortUse::Reuse,
            )
            .expect("still a connection");
        assert_eq!(g.gated.len(), 2, "both are being hidden");

        g.on_swarm_event(FromSwarm::ListenFailure(
            libp2p::swarm::behaviour::ListenFailure {
                local_addr: &addr(),
                send_back_addr: &addr(),
                error: &libp2p::swarm::ListenError::Aborted,
                connection_id: listen_id,
                peer_id: Some(peer(INFRA)),
            },
        ));
        g.on_swarm_event(FromSwarm::DialFailure(
            libp2p::swarm::behaviour::DialFailure {
                peer_id: Some(peer(INFRA)),
                error: &libp2p::swarm::DialError::Aborted,
                connection_id: dial_id,
            },
        ));

        assert!(
            g.gated.is_empty(),
            "an id whose connection never established must be released -- it will never \
             see the close that otherwise drains it, so the map would grow for the life \
             of the process"
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
    fn a_gated_connection_is_closed_when_its_peer_gains_data_plane_trust() {
        // THE MIXED-HANDLER STATE, and why a promotion is not merely
        // "under-privileged until it reconnects".
        //
        // A handler is chosen once and libp2p never rebuilds it. Leave a
        // gated connection in place through a promotion and the peer
        // ends up holding a `Denied` handler and, on its next
        // connection, an `Allowed` one. `kad` dispatches with
        // `NotifyHandler::Any`, which selects by PEER, so the Swarm may
        // route a query to the denied side -- where `on_behaviour_event`
        // drops it, safely and silently, and the query times out with a
        // usable connection sitting right there.
        //
        // ADR-0036 names that state and forbids it: close and
        // re-establish "rather than allowing a transient privilege mix".
        // Review finding on PR #77.
        //
        // The LOSING direction is not here. It is decided by
        // `dialing::connections_to_close`, so that `set_trust`'s count
        // includes it -- see
        // `a_downgraded_peer_is_closed_even_where_its_origin_still_permits`.
        let mut m = ConnectionManager::new(ConnectionPolicy::new(8, 8), 8);
        let _ = m.set_trust(
            TrustSources::new(
                PeerTrustPolicy::new([]).expect("empty"),
                InfrastructureSet::new([ident(INFRA)]).expect("small"),
            ),
            &[],
        );
        let mut g = ClassGated::new(Recording::default(), m.handle());

        let id = ConnectionId::new_unchecked(1);
        let handler = g
            .handle_established_inbound_connection(id, peer(INFRA), &addr(), &addr())
            .expect("still a connection");
        assert!(matches!(handler, ClassGatedHandler::Denied));

        let mut cx = Context::from_waker(std::task::Waker::noop());
        assert!(
            matches!(g.poll(&mut cx), Poll::Pending),
            "an unchanged policy must not close anything"
        );

        // PROMOTED to the data plane while its gated connection is open.
        let _ = m.set_trust(
            TrustSources::new(
                PeerTrustPolicy::new([ident(INFRA)]).expect("small"),
                InfrastructureSet::default(),
            ),
            &[],
        );

        match g.poll(&mut cx) {
            Poll::Ready(ToSwarm::CloseConnection {
                peer_id,
                connection: CloseConnection::One(closed),
            }) => {
                assert_eq!(peer_id, peer(INFRA));
                assert_eq!(closed, id);
            }
            other => panic!(
                "a promoted peer's gated connection must be closed, or it and its next \
                 connection form the mixed-handler pair `NotifyHandler::Any` can route \
                 into; got {other:?}"
            ),
        }

        // AND ONLY ONCE.
        let _ = m.set_trust(
            TrustSources::new(
                PeerTrustPolicy::new([ident(INFRA)]).expect("small"),
                InfrastructureSet::default(),
            ),
            &[],
        );
        assert!(
            matches!(g.poll(&mut cx), Poll::Pending),
            "a connection already queued must not be queued again"
        );

        // THE STEP THAT WOULD HAVE CAUGHT THE CRASH, and did not exist
        // until a review found it. Queueing the close does not close the
        // connection -- the Swarm does that later, and then reports it.
        // An earlier version dequeued by REMOVING the entry from `gated`
        // at queue time, so this event found nothing, fell through to
        // the forwarding arm, and handed the inner behaviour a close for
        // a connection it was never given a handler for: the
        // `libp2p-request-response` `.expect()` panic this wrapper
        // exists to prevent, re-opened by the fix for something else.
        let events = Arc::new(Mutex::new(0));
        g.inner_mut().swarm_events = Arc::clone(&events);
        g.on_swarm_event(FromSwarm::ConnectionClosed(
            libp2p::swarm::behaviour::ConnectionClosed {
                peer_id: peer(INFRA),
                connection_id: id,
                endpoint: &connected_point(),
                remaining_established: 0,
                cause: None,
            },
        ));
        assert_eq!(
            *events.lock().expect("not poisoned"),
            0,
            "the close of a connection this wrapper hid must NOT reach the inner \
             behaviour, queued for closure or not -- it was never told the connection \
             opened"
        );
        assert!(
            g.gated.is_empty() && g.close_queued.is_empty(),
            "and both records must be released, or they grow for the life of the process"
        );
    }

    #[test]
    fn a_still_trusted_peers_connection_survives_an_unrelated_policy_change() {
        // The control. A wrapper that closed on every republication
        // would pass the test above and churn every connection it holds.
        let mut m = ConnectionManager::new(ConnectionPolicy::new(8, 8), 8);
        let _ = m.set_trust(
            TrustSources::new(
                PeerTrustPolicy::new([ident(TRUSTED)]).expect("small"),
                InfrastructureSet::default(),
            ),
            &[],
        );
        let mut g = ClassGated::new(Recording::default(), m.handle());
        let id = ConnectionId::new_unchecked(1);
        let _ = g
            .handle_established_inbound_connection(id, peer(TRUSTED), &addr(), &addr())
            .expect("admitted");

        // Someone ELSE joins the allowlist. `TRUSTED` is untouched.
        let _ = m.set_trust(
            TrustSources::new(
                PeerTrustPolicy::new([ident(TRUSTED), ident(STRANGER)]).expect("small"),
                InfrastructureSet::default(),
            ),
            &[],
        );

        let mut cx = Context::from_waker(std::task::Waker::noop());
        assert!(
            matches!(g.poll(&mut cx), Poll::Pending),
            "a still-trusted peer's connection must survive a policy change that did \
             not concern it"
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

        // NOT a claim that the EXISTING connection keeps its handler
        // forever. It does not: a gating change closes it -- the LOSING
        // direction in `dialing::connections_to_close`, so the closure
        // lands in `set_trust`'s count, and the GAINING direction in
        // `ClassGated::poll`, pinned by
        // `a_gated_connection_is_closed_when_its_peer_gains_data_plane_trust`.
        // What this test pins is narrower: the wrapper reads the LIVE
        // snapshot, so the next connection is decided on current
        // policy.
        //
        // Revocation also closes such connections where it can, and that
        // now has a test of its own rather than a citation:
        // `advertised_protocol_set::a_peer_downgraded_to_infrastructure_only_loses_its_connection`
        // takes a trusted peer to infrastructure-only over real sockets
        // and requires the connection to go. It was written because the
        // claim rested on two separately-tested halves -- the manager
        // NAMES a downgrade, and a named connection is CLOSED -- with
        // the closing half only ever exercised for a full revocation to
        // `Unauthorized`, which is the easier case.
    }
}
