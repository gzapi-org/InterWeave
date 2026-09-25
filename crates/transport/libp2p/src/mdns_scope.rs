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
//! # Live, not inert
//!
//! This file used to say no enabled behaviour consumes `FromSwarm::
//! NewExternalAddrOfPeer`. That was wrong: `libp2p-request-response
//! 0.30.0` feeds it into its own `PeerAddresses` (`lib.rs:837`, via
//! `libp2p-swarm 0.48.0` `behaviour/peer_addresses.rs:26`) and extends
//! every dial it makes from that cache, and `direct` and `endpoints` are
//! request-response behaviours. So an unswallowed injection would have
//! reached a dial today, not merely under some future version.
//! `no_address_reaches_the_swarm` fails if the filter stops.
//!
//! # The second door: the pending hook
//!
//! The crate is ALSO an address book in its own right: its
//! `handle_pending_outbound_connection` (`behaviour.rs:225-242`) answers
//! every dial toward a PeerId with every address it ever heard for it
//! from multicast, unchecked, and the Swarm appends that answer to any
//! dial built with `extend_addresses_through_behaviour` -- Kademlia's,
//! the relay client's reservation dial, request-response's and
//! gossipsub's among them. Swallowing the emission and forwarding this
//! hook closed the door nothing used and left open the one every
//! extended dial uses (#111 mDNS review F1). This wrapper answers the
//! hook with NOTHING, so the crate contributes no address to any dial;
//! `the_crates_discovered_addresses_extend_no_dial` pins it, with the
//! unwrapped crate's answer as the control. Identify's crate cache is
//! the same shape and is turned off in `behaviour.rs` for the same
//! reason.
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
//! Everything else passes untouched. mDNS originates no dial --
//! measured: no `ToSwarm::Dial` appears anywhere in the crate -- so the
//! pending hook above is the only way it could shape one, and this
//! wrapper decides nothing about who is offered a protocol.

use std::task::{Context, Poll};

use libp2p::core::Endpoint;
use libp2p::core::transport::PortUse;
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler, THandlerInEvent,
    THandlerOutEvent, ToSwarm,
};
use libp2p::{Multiaddr, PeerId};

/// mDNS under ADR-0011: its address injections stop here.
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

    /// The wrapped crate, for what it reports about itself: ADR-0053's
    /// drop counts, captured by the runtime before the Swarm owns it.
    pub const fn inner(&self) -> &B {
        &self.inner
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

    /// NOTHING: the crate's answer is every address multicast named for
    /// the peer, unchecked, and a discovered pair reaches a dial only
    /// through the discovery pipeline (module doc, "The second door").
    /// The inner hook is not called: the crate's is a pure lookup.
    fn handle_pending_outbound_connection(
        &mut self,
        _: ConnectionId,
        _: Option<PeerId>,
        _: &[Multiaddr],
        _: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        Ok(Vec::new())
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
        /// What the crate's pending hook answers: the multicast book.
        heard: Vec<Multiaddr>,
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

        fn handle_pending_outbound_connection(
            &mut self,
            _: ConnectionId,
            _: Option<PeerId>,
            _: &[Multiaddr],
            _: Endpoint,
        ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
            Ok(self.heard.clone())
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
        // ADR-0011: a discovery provider does not put an address into
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
            heard: Vec::new(),
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
            heard: Vec::new(),
        });
        let mut cx = noop_cx();
        assert!(matches!(
            scope.poll(&mut cx),
            Poll::Ready(ToSwarm::GenerateEvent(()))
        ));
        assert_eq!(scope.suppressed_addresses(), 0);
    }

    /// #111 mDNS review F1. The crate answers a dial's pending hook with
    /// what multicast told it; the wrapper answers with nothing, whatever
    /// the dial and whatever the crate heard. THE CONTROL is the same
    /// scripted crate asked directly: it does contribute, so the empty
    /// answer is the wrapper's doing and not an empty book.
    #[test]
    fn the_crates_discovered_addresses_extend_no_dial() {
        let heard: Vec<Multiaddr> = ["/ip4/192.168.1.9/tcp/4001", "/ip4/127.0.0.1/tcp/22"]
            .iter()
            .map(|a| a.parse().expect("addr"))
            .collect();
        let peer = PeerId::random();
        let id = ConnectionId::new_unchecked(1);
        let mut crate_alone = Scripted {
            queued: VecDeque::new(),
            heard: heard.clone(),
        };
        assert_eq!(
            crate_alone
                .handle_pending_outbound_connection(id, Some(peer), &[], Endpoint::Dialer)
                .expect("no denial"),
            heard,
            "the control: unwrapped, the crate extends the dial with what it heard"
        );
        let mut scope = MdnsScope::new(Scripted {
            queued: VecDeque::new(),
            heard: heard.clone(),
        });
        for (target, explicit) in [
            (Some(peer), &heard[..1]),
            (Some(peer), &[][..]),
            (None, &[][..]),
        ] {
            assert_eq!(
                scope
                    .handle_pending_outbound_connection(id, target, explicit, Endpoint::Dialer)
                    .expect("no denial"),
                Vec::<Multiaddr>::new(),
                "wrapped, it extends no dial: {target:?}, explicit {explicit:?}"
            );
        }
    }
}
