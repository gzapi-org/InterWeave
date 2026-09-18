// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `RELAY.md` §5 at the behaviour boundary: who gets to say a
//! relay-derived address is advertised.
//!
//! # The crate's own confirmation is not §5's
//!
//! On a reservation's first acceptance the pinned relay client pushes
//! `ToSwarm::ExternalAddrConfirmed` for the circuit address it derived
//! from the address it was asked to listen on (`priv_client.rs`, the
//! `ReservationReqAccepted` arm), and the Swarm would then advertise it
//! through Identify on its own. §5 makes the advertised set the
//! reservation manager's: an address enters it when the manager
//! records the acceptance the LISTENER reports -- the relay's own
//! external addresses, which are what a peer can dial, and which the
//! crate's confirmation does not carry -- and leaves it when the
//! manager records the loss. So the confirmation is swallowed here and
//! counted, the way [`crate::candidate_scope::ScopedCandidates`]
//! swallows the AutoNAT client's -- and so is the crate's own
//! `ExternalAddrExpired`, pushed when the relay's connection closes,
//! which would otherwise remove the address one poll before the
//! listener's close lets the manager withdraw it -- and the runtime
//! adds and removes external addresses from the manager alone. Pinned
//! by `the_crates_own_confirmation_never_reaches_the_swarm`.
//!
//! Everything else passes untouched: the wrapper decides nothing about
//! dials (that is the outbound gate's, through [`crate::attribution::
//! Attributing`] outside it) or about who is offered the protocols
//! (that is [`crate::class_gate::ClassGated`]'s, outside it too).

use std::task::{Context, Poll};

use libp2p::core::Endpoint;
use libp2p::core::transport::PortUse;
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler, THandlerInEvent,
    THandlerOutEvent, ToSwarm,
};
use libp2p::{Multiaddr, PeerId};

/// The relay client under §5: its confirmations stop here.
pub struct ReservationScope<B> {
    inner: B,
    suppressed_confirmations: usize,
}

impl<B> ReservationScope<B> {
    /// Wrap `inner`.
    pub const fn new(inner: B) -> Self {
        Self {
            inner,
            suppressed_confirmations: 0,
        }
    }

    /// Confirmations and expiries the crate emitted and this wrapper
    /// swallowed.
    #[must_use]
    pub const fn suppressed_confirmations(&self) -> usize {
        self.suppressed_confirmations
    }
}

impl<B: NetworkBehaviour> NetworkBehaviour for ReservationScope<B> {
    type ConnectionHandler = B::ConnectionHandler;
    type ToSwarm = B::ToSwarm;

    fn handle_established_inbound_connection(
        &mut self,
        id: ConnectionId,
        peer: PeerId,
        local: &Multiaddr,
        remote: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.inner
            .handle_established_inbound_connection(id, peer, local, remote)
    }

    fn handle_established_outbound_connection(
        &mut self,
        id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role: Endpoint,
        port: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.inner
            .handle_established_outbound_connection(id, peer, addr, role, port)
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

    fn on_swarm_event(&mut self, event: FromSwarm<'_>) {
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

    /// The crate's confirmation and expiry are swallowed; everything
    /// else passes.
    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        loop {
            match self.inner.poll(cx) {
                Poll::Ready(
                    ToSwarm::ExternalAddrConfirmed(_) | ToSwarm::ExternalAddrExpired(_),
                ) => {
                    self.suppressed_confirmations += 1;
                }
                other => return other,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    use libp2p::swarm::dummy;
    use std::collections::VecDeque;

    /// A behaviour that emits whatever it is queued, so the wrapper's
    /// filter is measured against every `ToSwarm` shape it can see.
    struct Scripted {
        queued: VecDeque<ToSwarm<(), libp2p::swarm::THandlerInEvent<Self>>>,
    }

    impl NetworkBehaviour for Scripted {
        type ConnectionHandler = dummy::ConnectionHandler;
        type ToSwarm = ();

        fn handle_established_inbound_connection(
            &mut self,
            _: ConnectionId,
            _: PeerId,
            _: &Multiaddr,
            _: &Multiaddr,
        ) -> Result<THandler<Self>, ConnectionDenied> {
            Ok(dummy::ConnectionHandler)
        }

        fn handle_established_outbound_connection(
            &mut self,
            _: ConnectionId,
            _: PeerId,
            _: &Multiaddr,
            _: Endpoint,
            _: PortUse,
        ) -> Result<THandler<Self>, ConnectionDenied> {
            Ok(dummy::ConnectionHandler)
        }

        fn on_swarm_event(&mut self, _: FromSwarm<'_>) {}

        fn on_connection_handler_event(
            &mut self,
            _: PeerId,
            _: ConnectionId,
            _: THandlerOutEvent<Self>,
        ) {
        }

        fn poll(
            &mut self,
            _: &mut Context<'_>,
        ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
            self.queued.pop_front().map_or(Poll::Pending, Poll::Ready)
        }
    }

    fn noop_cx() -> Context<'static> {
        Context::from_waker(std::task::Waker::noop())
    }

    #[test]
    fn the_crates_own_confirmation_never_reaches_the_swarm() {
        // RELAY.md section 5: the advertised set is the manager's. Two
        // confirmations and an expiry in a row are swallowed, the event
        // after them passes, and a candidate passes -- the wrapper
        // filters two shapes and no other.
        let addr: Multiaddr = "/ip4/192.0.2.1/tcp/4001/p2p-circuit".parse().expect("addr");
        let mut scope = ReservationScope::new(Scripted {
            queued: VecDeque::from([
                ToSwarm::ExternalAddrConfirmed(addr.clone()),
                ToSwarm::ExternalAddrConfirmed(addr.clone()),
                ToSwarm::ExternalAddrExpired(addr.clone()),
                ToSwarm::GenerateEvent(()),
                ToSwarm::NewExternalAddrCandidate(addr),
            ]),
        });
        let mut cx = noop_cx();
        assert!(matches!(
            scope.poll(&mut cx),
            Poll::Ready(ToSwarm::GenerateEvent(()))
        ));
        assert_eq!(scope.suppressed_confirmations(), 3);
        assert!(matches!(
            scope.poll(&mut cx),
            Poll::Ready(ToSwarm::NewExternalAddrCandidate(_))
        ));
        assert!(scope.poll(&mut cx).is_pending());
        assert_eq!(
            scope.suppressed_confirmations(),
            3,
            "nothing else was counted"
        );
    }
}
