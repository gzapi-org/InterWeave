// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! ADR-0011 §Discovery never writes the address book, at the boundary
//! where it binds: a discovery provider yields candidates and never
//! writes the Swarm's address book (A 2026-09-20).
//!
//! The rule names this crate's emission by version and shape, and says
//! what the wrapper is for: the emission is swallowed here, "the way
//! the relay client's own reservation confirmation is", and the only
//! route from a discovered pair to a dialable address is the provider's
//! normalization, bounds and dedup into `DiscoveryManager` and from
//! there through ConnectionManager admission. It binds every provider,
//! present and next -- so this file is one instance of the rule and not
//! the rule itself.
//!
//! # What the crate does, measured rather than read
//!
//! `libp2p-mdns 0.49.0` pushes, for every newly discovered pair and with
//! no check of any kind:
//!
//! ```text
//! ToSwarm::NewExternalAddrOfPeer { peer_id, address }   // behaviour.rs:340
//! ```
//!
//! Any host on the multicast domain can therefore name an address for an
//! arbitrary `PeerId`, and the Swarm forwards it to every behaviour
//! (`libp2p-swarm 0.48.0` `lib.rs:1165`). That is the shape
//! `discovery/providers/mdns.md` forbids in words -- mDNS grants **zero
//! trust**, and discovery is advisory candidate reachability that does
//! not dial, route or confer authority -- with, until this wrapper,
//! nothing in the code enforcing it. `DISCOVERY-CONFORMANCE.md`'s
//! Decision 2026-09-20 makes the assertion a conformance requirement.
//!
//! # Inert today is not the same as safe
//!
//! No behaviour this crate enables consumes `FromSwarm::
//! NewExternalAddrOfPeer`: `kad`, `gossipsub`, `identify`, `autonat`,
//! `relay`, `dcutr` and the vendored AutoNAT all have zero occurrences
//! (measured 2026-09-20 against the pinned versions). So the injection
//! reaches nobody in today's composition.
//!
//! THAT IS A FACT ABOUT THESE VERSIONS, NOT THE RULE, which is the
//! ADR's own phrasing: "the rule is what keeps a LAN broadcast out of
//! the book when a future version starts consuming it". A libp2p
//! release that makes any of them consume the event would turn a LAN
//! broadcast into address-book content with nothing in this repository
//! changing -- the seam being unreachable is exactly why it would not
//! be noticed. Swallowing it here makes the rule executable:
//! `no_address_reaches_the_swarm` fails if the filter stops.
//!
//! # The one route in
//!
//! A multicast packet reaches this profile through
//! `Event::Discovered`/`Expired`, which the driver hands to
//! `interweave-discovery-mdns`'s normalization, bounds and dedup, and
//! from there to the discovery pipeline -- where a candidate is a
//! candidate and nothing more. `ReservationScope` swallows the relay
//! client's own confirmation for the same reason and in the same shape.
//!
//! Everything else passes untouched. This wrapper decides nothing about
//! dials -- mDNS originates none, measured: no `ToSwarm::Dial` appears
//! anywhere in the crate -- and nothing about who is offered a protocol.

use std::task::{Context, Poll};

use libp2p::core::Endpoint;
use libp2p::core::transport::PortUse;
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler, THandlerInEvent,
    THandlerOutEvent, ToSwarm,
};
use libp2p::{Multiaddr, PeerId};

/// mDNS under ADR-0009: its address injections stop here.
pub struct MdnsScope<B> {
    inner: B,
    suppressed_addresses: usize,
}

impl<B> MdnsScope<B> {
    /// Wrap `inner`.
    pub const fn new(inner: B) -> Self {
        Self {
            inner,
            suppressed_addresses: 0,
        }
    }

    /// Address injections the crate emitted and this wrapper swallowed.
    ///
    /// Counted rather than dropped silently so a test can tell "the
    /// filter ran" from "the crate emitted nothing", which are the same
    /// observation at the Swarm and different facts about the wrapper.
    #[must_use]
    pub const fn suppressed_addresses(&self) -> usize {
        self.suppressed_addresses
    }
}

impl<B: NetworkBehaviour> NetworkBehaviour for MdnsScope<B> {
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

    /// The crate's address injections are swallowed; everything else
    /// passes.
    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        loop {
            match self.inner.poll(cx) {
                Poll::Ready(ToSwarm::NewExternalAddrOfPeer { .. }) => {
                    self.suppressed_addresses += 1;
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
    /// filter is measured against every `ToSwarm` shape it can see
    /// rather than only the one it is written for.
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
    fn no_address_reaches_the_swarm() {
        // ADR-0009: a discovery provider does not put an address into
        // this Swarm. Two injections in a row are swallowed, the event
        // after them passes, and the OTHER address-shaped variants pass
        // -- the wrapper filters one shape and no other, which is what
        // stops it from quietly becoming a general address suppressor.
        let addr: Multiaddr = "/ip4/192.0.2.1/tcp/4001".parse().expect("addr");
        let peer = PeerId::random();
        let mut scope = MdnsScope::new(Scripted {
            queued: VecDeque::from([
                ToSwarm::NewExternalAddrOfPeer {
                    peer_id: peer,
                    address: addr.clone(),
                },
                ToSwarm::NewExternalAddrOfPeer {
                    peer_id: peer,
                    address: addr.clone(),
                },
                ToSwarm::GenerateEvent(()),
                ToSwarm::NewExternalAddrCandidate(addr.clone()),
                ToSwarm::ExternalAddrConfirmed(addr),
            ]),
        });
        let mut cx = noop_cx();

        assert!(
            matches!(scope.poll(&mut cx), Poll::Ready(ToSwarm::GenerateEvent(()))),
            "the discovery event itself must pass -- it is the only route in"
        );
        assert_eq!(
            scope.suppressed_addresses(),
            2,
            "both injections were swallowed, and the count is what tells \
             a live filter from a crate that emitted nothing"
        );
        assert!(
            matches!(
                scope.poll(&mut cx),
                Poll::Ready(ToSwarm::NewExternalAddrCandidate(_))
            ),
            "this profile's OWN candidate address is not mDNS's injection"
        );
        assert!(
            matches!(
                scope.poll(&mut cx),
                Poll::Ready(ToSwarm::ExternalAddrConfirmed(_))
            ),
            "and neither is a confirmation"
        );
        assert_eq!(
            scope.suppressed_addresses(),
            2,
            "nothing after the two injections was swallowed"
        );
    }

    #[test]
    fn a_behaviour_that_injects_nothing_is_untouched() {
        // The control for the count above: with no injection queued the
        // counter stays at zero, so a passing `no_address_reaches_the_
        // swarm` cannot be explained by a wrapper that counts anything
        // it sees.
        let mut scope = MdnsScope::new(Scripted {
            queued: VecDeque::from([ToSwarm::GenerateEvent(())]),
        });
        let mut cx = noop_cx();
        assert!(matches!(
            scope.poll(&mut cx),
            Poll::Ready(ToSwarm::GenerateEvent(()))
        ));
        assert_eq!(scope.suppressed_addresses(), 0);
    }
}
