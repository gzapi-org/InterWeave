// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `RELAY.md` §8 at the behaviour boundary: what a reservation this
//! profile GRANTS carries is its direct external addresses.
//!
//! # A circuit through a circuit is not an address
//!
//! The pinned relay server sends a client EVERY external address the
//! Swarm holds when it accepts a reservation, and offers no filter.
//! A profile that is a relay server AND a relay client with an active
//! reservation holds relay-derived circuit addresses in that set, so
//! it would hand its clients `/../p2p-circuit/p2p/<self>/p2p-circuit`
//! -- a nested circuit the pinned server cannot serve (SPIKE-004 F10's
//! sibling, recorded on PR #100). The server learns the set from the
//! Swarm's `ExternalAddrConfirmed` and `ExternalAddrExpired`, so the
//! circuit ones are withheld from it here, before it can learn them;
//! every other event passes untouched. Pinned by
//! `a_relay_derived_address_never_reaches_the_server`, and on the wire
//! by `tests/connectivity/tests/relay_server.rs`'s dual-role test.
//!
//! The wrapper decides nothing else: who is offered the hop protocol
//! is [`crate::class_gate::ClassGated`]'s, outside it.

use std::task::{Context, Poll};

use libp2p::core::Endpoint;
use libp2p::core::transport::PortUse;
use libp2p::multiaddr::Protocol;
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler, THandlerInEvent,
    THandlerOutEvent, ToSwarm,
};
use libp2p::{Multiaddr, PeerId};

/// The relay server told only the direct external addresses.
pub struct ServedAddresses<B> {
    inner: B,
    withheld: usize,
}

impl<B> ServedAddresses<B> {
    /// Wrap `inner`.
    pub const fn new(inner: B) -> Self {
        Self { inner, withheld: 0 }
    }

    /// Circuit-address confirmations and expiries withheld from the
    /// server.
    #[must_use]
    pub const fn withheld(&self) -> usize {
        self.withheld
    }
}

/// Whether an external address runs through a relay.
fn is_relayed(address: &Multiaddr) -> bool {
    address.iter().any(|p| matches!(p, Protocol::P2pCircuit))
}

impl<B: NetworkBehaviour> NetworkBehaviour for ServedAddresses<B> {
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

    /// A confirmation or expiry of a circuit address stops here; every
    /// other event reaches the server.
    fn on_swarm_event(&mut self, event: FromSwarm<'_>) {
        match &event {
            FromSwarm::ExternalAddrConfirmed(e) if is_relayed(e.addr) => {
                self.withheld += 1;
            }
            FromSwarm::ExternalAddrExpired(e) if is_relayed(e.addr) => {
                self.withheld += 1;
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

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        self.inner.poll(cx)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    use libp2p::swarm::behaviour::{ExternalAddrConfirmed, ExternalAddrExpired};
    use libp2p::swarm::dummy;

    /// A behaviour that records the external addresses the Swarm told
    /// it, the way the relay server's `ExternalAddresses` does.
    #[derive(Default)]
    struct Recording {
        confirmed: Vec<Multiaddr>,
        expired: Vec<Multiaddr>,
        others: usize,
    }

    impl NetworkBehaviour for Recording {
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

        fn on_swarm_event(&mut self, event: FromSwarm<'_>) {
            match event {
                FromSwarm::ExternalAddrConfirmed(e) => self.confirmed.push(e.addr.clone()),
                FromSwarm::ExternalAddrExpired(e) => self.expired.push(e.addr.clone()),
                _ => self.others += 1,
            }
        }

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
            Poll::Pending
        }
    }

    #[test]
    fn a_relay_derived_address_never_reaches_the_server() {
        // RELAY.md section 8: a circuit address confirmed or expired is
        // withheld; a direct one, and every other event, reaches the
        // server -- the wrapper filters two shapes by one predicate
        // and no other.
        let direct: Multiaddr = "/ip4/192.0.2.1/tcp/4001".parse().expect("addr");
        let circuit: Multiaddr = "/ip4/192.0.2.2/tcp/4001/p2p/12D3KooWCLxLXFHqvfsHVLDcNsSpZBQq1M1KMRgQRLLLnHTv7oQD/p2p-circuit"
            .parse()
            .expect("addr");
        let mut served = ServedAddresses::new(Recording::default());
        served.on_swarm_event(FromSwarm::ExternalAddrConfirmed(ExternalAddrConfirmed {
            addr: &circuit,
        }));
        served.on_swarm_event(FromSwarm::ExternalAddrConfirmed(ExternalAddrConfirmed {
            addr: &direct,
        }));
        served.on_swarm_event(FromSwarm::ExternalAddrExpired(ExternalAddrExpired {
            addr: &circuit,
        }));
        served.on_swarm_event(FromSwarm::ExternalAddrExpired(ExternalAddrExpired {
            addr: &direct,
        }));
        served.on_swarm_event(FromSwarm::NewListenAddr(
            libp2p::swarm::behaviour::NewListenAddr {
                listener_id: libp2p::core::transport::ListenerId::next(),
                addr: &circuit,
            },
        ));
        assert_eq!(
            served.withheld(),
            2,
            "the circuit's confirmation and expiry"
        );
        assert_eq!(served.inner.confirmed, vec![direct.clone()]);
        assert_eq!(served.inner.expired, vec![direct]);
        assert_eq!(
            served.inner.others, 1,
            "a listener's circuit address is not an external one and passes"
        );
    }
}
