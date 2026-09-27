// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `RELAY.md` §8's verified-address gate: the relay server offers the
//! hop protocol -- and so accepts a reservation or a circuit -- only
//! while this profile holds at least one verified DIRECT external
//! address.
//!
//! # Why a relay with no verified address must refuse
//!
//! The pinned server serves under a forced `Status::Enable`
//! (`relay_server_driver::build_behaviour`) and hands a client every
//! external address the Swarm holds. With none, it accepts a reservation
//! that carries none; the client refuses it (`NoAddressesInReservation`)
//! while the relay counts it against its ceiling until the connection
//! closes -- and `libp2p-relay` 0.22.0 reads a re-ask over the same
//! connection as a renewal that skips the per-peer ceiling and is
//! refused only on the TOTAL, which those phantoms hold. SPIKE-004
//! phase B measured it (node row `early`, 2026-09-26).
//!
//! # Per request, not per connection
//!
//! The gate answers every inbound stream, not the connection: the
//! Swarm asks a handler's `listen_protocol` once per inbound stream
//! (`libp2p-swarm-0.48.0` `connection.rs:440`), and [`HopGatedHandler`]
//! reads the gate there each time, substituting the denied upgrade
//! while it is shut. So a connection opened while the gate was open is
//! refused its next RESERVE -- a renewal -- or CONNECT once the served
//! set empties. [`crate::class_gate::ClassGated`] could not do this: it
//! chooses a handler once, at establishment, and never revisits an
//! admitted connection. Closing the connections instead would drop
//! every reservation at once. The client sees the hop protocol as
//! unsupported (`ReserveError::Unsupported`), not a status: the crate's
//! denial status is `pub(crate)`, so nothing above the crate can send
//! one, and the forced `Status::Enable` below is left alone.
//!
//! # And again when the request arrives
//!
//! Negotiation is not the request. The crate reads RESERVE or CONNECT
//! from a negotiated stream later, with a sixty-second timeout and up to
//! ten streams per connection (`libp2p-relay` 0.22.0
//! `behaviour/handler.rs:56-57,428-440`, `protocol/inbound_hop.rs:179-209`),
//! and grants from the addresses it holds THEN. So a stream opened while
//! the gate was open could carry a request after it shut, and be granted
//! a reservation with no address -- the phantom the gate exists to
//! prevent (#129 review F2). [`HopGated`] therefore reads the gate a
//! second time, where the request reaches the behaviour, and drops one
//! that arrives shut: the crate records nothing before it accepts, and
//! the request owns its stream, so dropping it closes the stream and the
//! client sees it end (`ReserveError::Io`), not a status.
//!
//! The request is recognised by its `Debug` form, because the handler's
//! event type is `pub(crate)` in the crate. A libp2p bump that renamed it
//! would stop the match silently, so [`HopCounters::requests`] counts
//! every one recognised, and `tests/relay_hop_gate.rs` asserts a granted
//! reservation was among them.
//!
//! # Advertisement follows the gate, and needs nothing from it
//!
//! What Identify advertises is recomputed from `listen_protocol` when a
//! connection is polled (`connection.rs:456-467`), and an idle
//! connection is not polled. No waker is needed here for that: the gate
//! moves only on an external address's confirmation or expiry, and
//! Identify answers exactly those events by notifying EVERY connection's
//! handler (`libp2p-identify` 0.48.0 `behaviour.rs:551-568`,
//! `AddressesChanged`), which polls the connection, which recomputes the
//! set and pushes it. A waker registry was built here first and removed:
//! a mutation that stopped it waking anything left the test that pins
//! the advertisement green (`tests/relay_hop_gate.rs`, "hop advertised
//! after the flip" -- the step that fails if Identify stops doing it).

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use either::Either;
use libp2p::core::Endpoint;
use libp2p::core::transport::PortUse;
use libp2p::core::upgrade::DeniedUpgrade;
use libp2p::swarm::handler::{
    ConnectionEvent, ConnectionHandler, ConnectionHandlerEvent, FullyNegotiatedInbound,
    ListenUpgradeError, SendWrapper,
};
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, SubstreamProtocol, THandler,
    THandlerInEvent, THandlerOutEvent, ToSwarm,
};
use libp2p::{Multiaddr, PeerId};

use crate::served_addresses::is_relayed;

/// Whether hop is offered, shared by the behaviour and every handler.
#[derive(Debug, Default)]
struct Gate(AtomicBool);

impl Gate {
    fn is_open(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    fn set(&self, open: bool) {
        self.0.store(open, Ordering::Release);
    }
}

/// What the gate counted.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HopCounters {
    /// RESERVE and CONNECT requests recognised reaching the server.
    pub requests: u64,
    /// Of those, dropped because they arrived with the gate shut.
    pub refused_late: u64,
}

/// A handle on the gate's counters that outlives the move into the
/// Swarm, the shape `HolePunchCounterHandle` has: a late refusal leaves
/// no other trace (the client sees its stream end), so this count is the
/// only place it is visible -- the gate records its own refusals
/// (`SwarmRuntime::relay_hop_counters`).
#[derive(Debug, Clone, Default)]
pub struct HopCounterHandle {
    inner: Arc<std::sync::Mutex<HopCounters>>,
}

impl HopCounterHandle {
    /// The counters as they stand.
    #[must_use]
    pub fn snapshot(&self) -> HopCounters {
        *self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn update(&self, f: impl FnOnce(&mut HopCounters)) {
        f(&mut self.inner.lock().unwrap_or_else(|e| e.into_inner()));
    }
}

/// Whether a relay handler event is an inbound RESERVE or CONNECT, by the
/// `Debug` form of `libp2p-relay` 0.22.0's `handler::Event`
/// (`behaviour/handler.rs:235-266`) inside the behaviour's `Either`.
fn is_request(event: &impl std::fmt::Debug) -> bool {
    let shown = format!("{event:?}");
    shown.starts_with("Left(Event::ReservationReqReceived")
        || shown.starts_with("Left(Event::CircuitReqReceived")
}

/// The relay server with hop offered only while a verified direct
/// external address is held.
pub struct HopGated<B> {
    inner: B,
    gate: Arc<Gate>,
    counters: HopCounterHandle,
    /// The direct external addresses the Swarm has confirmed and not
    /// expired. A circuit address never enters: it is not an address a
    /// reservation from this relay can carry (`served_addresses`).
    served: HashSet<Multiaddr>,
}

impl<B> HopGated<B> {
    /// Wrap `inner`, shut until the first direct address is confirmed.
    #[must_use]
    pub fn new(inner: B) -> Self {
        Self {
            inner,
            gate: Arc::default(),
            counters: HopCounterHandle::default(),
            served: HashSet::new(),
        }
    }

    /// What was counted so far.
    #[must_use]
    pub fn counters(&self) -> HopCounters {
        self.counters.snapshot()
    }

    /// A handle on the counters, to be read after the Swarm owns this.
    #[must_use]
    pub fn counter_handle(&self) -> HopCounterHandle {
        self.counters.clone()
    }

    /// Whether hop is offered now.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.gate.is_open()
    }

    /// The wrapped behaviour.
    pub const fn inner(&self) -> &B {
        &self.inner
    }

    fn handler<H>(&self, inner: H) -> HopGatedHandler<H> {
        HopGatedHandler {
            inner,
            gate: Arc::clone(&self.gate),
        }
    }
}

impl<B: NetworkBehaviour> NetworkBehaviour for HopGated<B>
where
    THandlerOutEvent<B>: std::fmt::Debug,
{
    type ConnectionHandler = HopGatedHandler<B::ConnectionHandler>;
    type ToSwarm = B::ToSwarm;

    fn handle_established_inbound_connection(
        &mut self,
        id: ConnectionId,
        peer: PeerId,
        local: &Multiaddr,
        remote: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        let inner = self
            .inner
            .handle_established_inbound_connection(id, peer, local, remote)?;
        Ok(self.handler(inner))
    }

    fn handle_established_outbound_connection(
        &mut self,
        id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role: Endpoint,
        port: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        let inner = self
            .inner
            .handle_established_outbound_connection(id, peer, addr, role, port)?;
        Ok(self.handler(inner))
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

    /// Every event reaches the server; a direct address's confirmation
    /// or expiry also moves the gate -- shut BEFORE the server forgets
    /// its last address and opened only AFTER it has learned the first,
    /// so no request the gate lets through, at negotiation and again on
    /// arrival, is answered by a server holding nothing to put in the
    /// reservation.
    fn on_swarm_event(&mut self, event: FromSwarm<'_>) {
        match &event {
            FromSwarm::ExternalAddrConfirmed(e) if !is_relayed(e.addr) => {
                self.served.insert(e.addr.clone());
            }
            FromSwarm::ExternalAddrExpired(e) if !is_relayed(e.addr) => {
                self.served.remove(e.addr);
            }
            _ => {}
        }
        if self.served.is_empty() {
            self.gate.set(false);
        }
        self.inner.on_swarm_event(event);
        if !self.served.is_empty() {
            self.gate.set(true);
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer: PeerId,
        id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        if is_request(&event) {
            let shut = !self.gate.is_open();
            self.counters.update(|c| {
                c.requests += 1;
                c.refused_late += u64::from(shut);
            });
            if shut {
                return;
            }
        }
        self.inner.on_connection_handler_event(peer, id, event);
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        self.inner.poll(cx)
    }
}

/// The relay server's handler, refusing its inbound streams while the
/// gate is shut. The server offers exactly one inbound protocol, hop
/// (`libp2p-relay` 0.22.0 `behaviour/handler.rs:517-524`; a relayed
/// connection gets the crate's dummy, which offers none), so shutting
/// every inbound stream of this handler is shutting hop and nothing
/// else.
pub struct HopGatedHandler<H> {
    inner: H,
    gate: Arc<Gate>,
}

impl<H: ConnectionHandler> ConnectionHandler for HopGatedHandler<H> {
    type FromBehaviour = H::FromBehaviour;
    type ToBehaviour = H::ToBehaviour;
    // Two-sided, as `ClassGatedHandler`'s is and for its reason: the shut
    // side must offer SOMETHING, and `DeniedUpgrade` is the something
    // that is empty.
    type InboundProtocol = Either<SendWrapper<H::InboundProtocol>, SendWrapper<DeniedUpgrade>>;
    type InboundOpenInfo = Either<H::InboundOpenInfo, ()>;
    type OutboundProtocol = H::OutboundProtocol;
    type OutboundOpenInfo = H::OutboundOpenInfo;

    /// Read on every inbound stream, so the gate's state NOW decides --
    /// never the state the connection was established under.
    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        let protocol = self.inner.listen_protocol();
        if self.gate.is_open() {
            protocol
                .map_upgrade(|u| Either::Left(SendWrapper(u)))
                .map_info(Either::Left)
        } else {
            // An empty protocol set (`DeniedUpgrade::protocol_info()` is
            // `iter::empty()`), the same denial the crate offers under
            // `Status::Disable`: the stream fails negotiation and nothing
            // reaches the server. The inner timeout is kept.
            protocol
                .map_upgrade(|_| Either::Right(SendWrapper(DeniedUpgrade)))
                .map_info(|_| Either::Right(()))
        }
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        self.inner.on_behaviour_event(event);
    }

    fn connection_keep_alive(&self) -> bool {
        self.inner.connection_keep_alive()
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>,
    > {
        self.inner.poll(cx)
    }

    fn poll_close(&mut self, cx: &mut Context<'_>) -> Poll<Option<Self::ToBehaviour>> {
        self.inner.poll_close(cx)
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
        let inner = &mut self.inner;
        match event {
            ConnectionEvent::FullyNegotiatedInbound(inbound) => {
                // The upgrade's Output is `futures::future::Either`, the
                // info this handler's `either::Either` -- as in
                // `class_gate`. The shut side negotiates nothing, so its
                // arm cannot arrive, and is dropped rather than asserted.
                let (futures::future::Either::Left(protocol), Either::Left(info)) =
                    (inbound.protocol, inbound.info)
                else {
                    return;
                };
                inner.on_connection_event(ConnectionEvent::FullyNegotiatedInbound(
                    FullyNegotiatedInbound { protocol, info },
                ));
            }
            ConnectionEvent::ListenUpgradeError(err) => {
                let (Either::Left(info), Either::Left(error)) = (err.info, err.error) else {
                    return;
                };
                inner.on_connection_event(ConnectionEvent::ListenUpgradeError(
                    ListenUpgradeError { info, error },
                ));
            }
            ConnectionEvent::FullyNegotiatedOutbound(outbound) => {
                inner.on_connection_event(ConnectionEvent::FullyNegotiatedOutbound(outbound));
            }
            ConnectionEvent::AddressChange(change) => {
                inner.on_connection_event(ConnectionEvent::AddressChange(change));
            }
            ConnectionEvent::DialUpgradeError(err) => {
                inner.on_connection_event(ConnectionEvent::DialUpgradeError(err));
            }
            ConnectionEvent::LocalProtocolsChange(change) => {
                inner.on_connection_event(ConnectionEvent::LocalProtocolsChange(change));
            }
            ConnectionEvent::RemoteProtocolsChange(change) => {
                inner.on_connection_event(ConnectionEvent::RemoteProtocolsChange(change));
            }
            // `#[non_exhaustive]`: a variant a libp2p bump adds is NOT
            // forwarded -- check this arm on every upgrade, as
            // `class_gate`'s identical arm says.
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use libp2p::core::upgrade::DeniedUpgrade;
    use libp2p::swarm::behaviour::ExternalAddrConfirmed;

    /// A handler event that prints as the relay's does: the arrival
    /// check knows a request only by that (#129 re-review N2).
    enum Shown {
        Request,
        Circuit,
        Other,
    }

    impl std::fmt::Debug for Shown {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Request => f.write_str(
                    "Left(Event::ReservationReqReceived { inbound_reservation_req: .., endpoint: .., renewed: false })",
                ),
                Self::Circuit => f.write_str(
                    "Left(Event::CircuitReqReceived { inbound_circuit_req: .., endpoint: .. })",
                ),
                Self::Other => f.write_str("Left(Event::ReservationTimedOut)"),
            }
        }
    }

    struct StubHandler;

    impl ConnectionHandler for StubHandler {
        type FromBehaviour = ();
        type ToBehaviour = Shown;
        type InboundProtocol = DeniedUpgrade;
        type OutboundProtocol = DeniedUpgrade;
        type InboundOpenInfo = ();
        type OutboundOpenInfo = ();

        fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol> {
            SubstreamProtocol::new(DeniedUpgrade, ())
        }

        fn on_behaviour_event(&mut self, (): ()) {}

        fn poll(
            &mut self,
            _: &mut Context<'_>,
        ) -> Poll<ConnectionHandlerEvent<Self::OutboundProtocol, (), Self::ToBehaviour>> {
            Poll::Pending
        }

        fn on_connection_event(
            &mut self,
            _: ConnectionEvent<'_, Self::InboundProtocol, Self::OutboundProtocol, (), ()>,
        ) {
        }
    }

    /// Counts what reaches the wrapped server.
    #[derive(Default)]
    struct Server {
        requests: usize,
        others: usize,
    }

    impl NetworkBehaviour for Server {
        type ConnectionHandler = StubHandler;
        type ToSwarm = ();

        fn handle_established_inbound_connection(
            &mut self,
            _: ConnectionId,
            _: PeerId,
            _: &Multiaddr,
            _: &Multiaddr,
        ) -> Result<THandler<Self>, ConnectionDenied> {
            Ok(StubHandler)
        }

        fn handle_established_outbound_connection(
            &mut self,
            _: ConnectionId,
            _: PeerId,
            _: &Multiaddr,
            _: Endpoint,
            _: PortUse,
        ) -> Result<THandler<Self>, ConnectionDenied> {
            Ok(StubHandler)
        }

        fn on_swarm_event(&mut self, _: FromSwarm<'_>) {}

        fn on_connection_handler_event(&mut self, _: PeerId, _: ConnectionId, event: Shown) {
            match event {
                Shown::Request | Shown::Circuit => self.requests += 1,
                Shown::Other => self.others += 1,
            }
        }

        fn poll(&mut self, _: &mut Context<'_>) -> Poll<ToSwarm<(), ()>> {
            Poll::Pending
        }
    }

    #[test]
    fn a_request_arriving_while_shut_is_dropped_and_counted_and_nothing_else_is() {
        let mut gated = HopGated::new(Server::default());
        let (peer, id) = (PeerId::random(), ConnectionId::new_unchecked(1));
        // SHUT: the request stops here, counted; the other event passes.
        gated.on_connection_handler_event(peer, id, Shown::Request);
        gated.on_connection_handler_event(peer, id, Shown::Circuit);
        gated.on_connection_handler_event(peer, id, Shown::Other);
        assert_eq!(
            gated.inner().requests,
            0,
            "the shut gate kept the RESERVE and the CONNECT from the server"
        );
        assert_eq!(gated.inner().others, 1);
        assert_eq!(
            gated.counters(),
            HopCounters {
                requests: 2,
                refused_late: 2
            }
        );
        // OPEN: the same request reaches the server -- the control.
        let direct: Multiaddr = "/ip4/198.51.100.7/tcp/4001".parse().expect("valid");
        gated.on_swarm_event(FromSwarm::ExternalAddrConfirmed(ExternalAddrConfirmed {
            addr: &direct,
        }));
        gated.on_connection_handler_event(peer, id, Shown::Request);
        assert_eq!(gated.inner().requests, 1);
        assert_eq!(
            gated.counter_handle().snapshot(),
            HopCounters {
                requests: 3,
                refused_late: 2
            },
            "and the handle the runtime reads says the same"
        );
    }
}
