// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Pre-Noise admission, applied where libp2p can still say no.
//!
//! # Why a behaviour and not a check in the event loop
//!
//! `SwarmEvent::IncomingConnection` arrives after the transport has
//! accepted the socket and while the upgrade is already running, and
//! the Swarm offers no way to abort a connection that has not
//! established. A check there would therefore bound the number of
//! handshakes this process REMEMBERS, not the number it performs --
//! which is the cost `SECURITY.md` is about.
//!
//! `NetworkBehaviour::handle_pending_inbound_connection` is called
//! before the upgrade begins and its `Err` aborts it. That is the only
//! place in libp2p where "pre-Noise work is bounded" can be true rather
//! than merely intended, so that is where [`PreAuthGate`] is consulted.
//!
//! # What the peer is told
//!
//! Nothing. `PreAuthDenial` distinguishes seven reasons, and every one
//! of them describes the shape of the gate to whoever is probing it, so
//! the denial reported to libp2p carries a fixed string and the reason
//! stays local. That is the module's own rule from the runtime crate,
//! restated here because this is the code that could break it.
//!
//! # The source is not an identity
//!
//! The bucket comes from the remote multiaddr the socket layer reports.
//! It is unauthenticated, shared by everything behind one NAT, and
//! cheap for an attacker with address space to change. It buys exactly
//! one thing -- that one bucket cannot spend the whole budget -- and
//! nothing here reads it for any other purpose.

use std::collections::HashMap;
use std::task::{Context, Poll};

use libp2p::PeerId;
use libp2p::core::transport::PortUse;
use libp2p::core::{Endpoint, Multiaddr};
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler, THandlerInEvent,
    THandlerOutEvent, ToSwarm, dummy,
};

use interweave_transport_runtime::preauth::{HandshakeSlot, PreAuthGate, PreAuthLimits};

/// What the peer is told when it is refused.
///
/// One string for all seven denials, on purpose: see the module note.
const REFUSAL: &str = "connection refused";

/// The label the gate accounts an inbound connection under.
///
/// `source_bucket` takes a socket address or a bare IP and deliberately
/// does NOT parse a multiaddr -- that grammar is libp2p's, and the
/// neutral crate has no business knowing it. So the translation happens
/// here, in the crate that does.
///
/// A multiaddr with no IP component -- a memory transport, a relayed
/// address -- yields the address as written.
///
/// For the memory transport that is the fail-closed direction: it
/// cannot merge two peers into one bucket, only fail to merge two
/// addresses that belong together.
///
/// **For a relayed address it is the wrong direction, and SPIKE-004
/// measured it.** A relayed inbound arrives with `remote_addr` of
/// `/p2p/<source>` and no IP anywhere, so the bucket becomes the source
/// PeerId -- one bucket per identity, over one relay connection, and
/// identities are free to mint. `contracts/CONNECTIVITY.md` §10
/// requires the opposite: charge the relay transport connection and
/// relay PeerId, and "MUST NOT create unbounded pseudo-source buckets
/// from circuit metadata". The relay's PeerId is present in
/// `local_addr` -- libp2p-relay 0.21.1 builds it as
/// `relay_addr.with(Protocol::P2pCircuit)` from the established relay
/// connection (`priv_client/transport.rs:404`) and the Swarm appends
/// `/p2p/<peer>` to a dialled address -- and this function is not given
/// it. That the shape holds on the pinned crate is read from those two
/// sources; what the tests below pin is what this function does with
/// the address it IS given.
///
/// The risk §10 names is proliferation, not merging, which is why the
/// memory-transport reasoning does not carry over. Unreachable today --
/// no relay feature is compiled, so no relayed inbound can arrive --
/// and Stage 11 must fix it before the relay client lands. Recorded as
/// divergence D3 in `spikes/spike-004/README.md`.
fn source_label(address: &Multiaddr) -> String {
    use libp2p::multiaddr::Protocol;

    for component in address {
        match component {
            Protocol::Ip4(ip) => return ip.to_string(),
            Protocol::Ip6(ip) => return ip.to_string(),
            _ => {}
        }
    }
    address.to_string()
}

/// The pre-authentication funnel, as a `NetworkBehaviour`.
///
/// Handles no protocol and opens no stream: it exists to be asked
/// whether a connection may begin.
#[derive(Debug)]
pub struct PreAuthAdmission {
    gate: PreAuthGate,
    /// Slots for handshakes libp2p has started and not yet resolved.
    ///
    /// Bounded by the gate's own ceilings rather than by the peer set,
    /// because an entry is created by an anonymous party connecting: a
    /// slot exists only where `admit` granted one, and `admit` grants
    /// at most `max_pending_total` at a time.
    in_flight: HashMap<ConnectionId, HandshakeSlot>,
    /// Monotonic origin for the gate's clock.
    started: std::time::Instant,
}

impl PreAuthAdmission {
    /// Build the funnel with the limits it will enforce.
    #[must_use]
    pub fn new(limits: PreAuthLimits) -> Self {
        Self {
            gate: PreAuthGate::new(limits),
            in_flight: HashMap::new(),
            started: std::time::Instant::now(),
        }
    }

    /// Handshakes in flight, for tests and diagnostics.
    #[must_use]
    pub const fn pending(&self) -> usize {
        self.gate.pending()
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Release the slot a connection was holding, if it held one.
    fn release(&mut self, id: ConnectionId) {
        if let Some(slot) = self.in_flight.remove(&id) {
            self.gate.completed(&slot);
        }
    }
}

impl NetworkBehaviour for PreAuthAdmission {
    type ConnectionHandler = dummy::ConnectionHandler;
    type ToSwarm = std::convert::Infallible;

    /// BEFORE THE UPGRADE. `Err` here aborts the connection without a
    /// Noise handshake being attempted, which is the whole point of
    /// answering in this hook rather than on the event.
    fn handle_pending_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        _local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<(), ConnectionDenied> {
        let now = self.now_ms();
        // NO DEADLINE SWEEP HERE, and the absence is deliberate. A
        // behaviour cannot close a connection that has not established
        // -- which is every connection this gate holds a slot for -- so
        // a sweep could only build a list nothing drained. What ends a
        // handshake that says nothing is the transport's connection
        // timeout, configured from these same limits; the listen
        // failure that follows releases the slot below.
        match self.gate.admit(&source_label(remote_addr), now) {
            Ok(slot) => {
                self.in_flight.insert(connection_id, slot);
                Ok(())
            }
            Err(_denial) => Err(ConnectionDenied::new(std::io::Error::other(REFUSAL))),
        }
    }

    /// The handshake completed, so its slot is no longer in flight.
    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        _peer: PeerId,
        _local_addr: &Multiaddr,
        _remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.release(connection_id);
        Ok(dummy::ConnectionHandler)
    }

    /// Outbound connections are not accounted here.
    ///
    /// This gate bounds work an ANONYMOUS party can make this process
    /// do. An outbound handshake was asked for by the root dial
    /// admission, which has already bounded it with a pending-dial
    /// ceiling and a connection ceiling of its own; counting it twice
    /// would let a remote peer's inbound traffic refuse this node's own
    /// dials.
    fn handle_established_outbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        _peer: PeerId,
        _addr: &Multiaddr,
        _role_override: Endpoint,
        _port_use: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(dummy::ConnectionHandler)
    }

    fn on_swarm_event(&mut self, event: FromSwarm<'_>) {
        // A PENDING INBOUND CONNECTION THAT DIED. libp2p reports it as
        // a listen failure and never establishes it, so without this
        // the slot would be held until the timeout swept it -- and an
        // attacker who opens connections and drops them would hold the
        // budget for the length of the timeout, every time, for free.
        if let FromSwarm::ListenFailure(failure) = event {
            self.release(failure.connection_id);
        }
    }

    fn on_connection_handler_event(
        &mut self,
        _peer: PeerId,
        _connection: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        // `dummy::ConnectionHandler` produces no events, so this is
        // unreachable by construction rather than by convention.
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

    /// Two identities, written out rather than generated, so the
    /// assertions below name the exact strings a reader can compare
    /// against the multiaddrs.
    const SOURCE_A: &str = "12D3KooWQ18zw7LaSTqratMZYnGnqSZygHcr1unr14b5LYghdybh";
    const SOURCE_B: &str = "12D3KooWG553HqwkbrTCG3WrcAMR4W8QcCeFhGcJeAzfrufjou1m";
    const RELAY: &str = "12D3KooWMWxoR6HFit2U3KZxcQ49MuJUiYFdDVeuzsuTM1rDEZMQ";

    fn addr(s: &str) -> Multiaddr {
        s.parse().expect("a multiaddr")
    }

    /// The ordinary case, and the control for everything below: a
    /// direct inbound connection buckets on its source IP, so every
    /// connection from one host shares one bucket.
    #[test]
    fn a_direct_inbound_buckets_on_its_source_ip() {
        assert_eq!(
            source_label(&addr("/ip4/198.51.100.7/tcp/5001")),
            "198.51.100.7"
        );
        assert_eq!(
            source_label(&addr("/ip4/198.51.100.7/tcp/5002")),
            "198.51.100.7",
            "a different port is the same host and must not be a second bucket"
        );
        assert_eq!(
            source_label(&addr("/ip6/2001:db8::1/tcp/5001")),
            "2001:db8::1"
        );
    }

    /// The documented fallback, asserted because the doc comment above
    /// `source_label` states it as a rule.
    #[test]
    fn an_address_with_no_ip_buckets_on_itself() {
        assert_eq!(source_label(&addr("/memory/42")), "/memory/42");
    }

    /// D3, PINNED RATHER THAN ENDORSED.
    ///
    /// SPIKE-004 measured what a relayed inbound connection presents
    /// before Noise: a remote address of `/p2p/<source>` with no IP
    /// anywhere, while the relay's own PeerId sits in the LOCAL
    /// address. `source_label` reads only the remote, finds no IP, and
    /// returns it as written — so the bucket is the SOURCE's PeerId.
    ///
    /// `contracts/CONNECTIVITY.md` §10 requires the opposite: charge
    /// the authenticated relay transport connection and relay PeerId
    /// plus the global caps, and "MUST NOT create unbounded
    /// pseudo-source buckets from circuit metadata".
    ///
    /// This test exists so that the fix FAILS here rather than passing
    /// silently, and so that the claim in the comment above
    /// `source_label` is enforced rather than merely written down. It
    /// is unreachable in a shipped build today — no relay feature is
    /// compiled — and becomes live the moment the relay client lands.
    #[test]
    fn a_relayed_inbound_buckets_on_the_source_peer_id_which_ss10_forbids() {
        let remote = addr(&format!("/p2p/{SOURCE_A}"));
        assert_eq!(
            source_label(&remote),
            format!("/p2p/{SOURCE_A}"),
            "the relayed remote carries no IP, so the bucket is the source's own identity"
        );
    }

    /// The defect's SHAPE, not merely its value: one relay connection,
    /// two source identities, two different buckets.
    ///
    /// §10's concern is proliferation — identities are free to mint, so
    /// the number of buckets is chosen by whoever is attacking. A test
    /// asserting one bucket's string would still pass if the second
    /// source somehow shared it; this asserts they differ, which is the
    /// property that makes the per-source ceiling escapable.
    #[test]
    fn two_relayed_sources_over_one_relay_get_different_buckets() {
        // The local address both connections arrive on, as SPIKE-004
        // measured it: one relay connection, named by the relay's own
        // PeerId, shared by every circuit riding it.
        let local = addr(&format!("/ip4/127.0.0.1/tcp/4001/p2p/{RELAY}/p2p-circuit"));
        assert!(
            local.to_string().contains("p2p-circuit"),
            "the shared connection is a circuit, which is what makes §10's rule apply"
        );
        let one = source_label(&addr(&format!("/p2p/{SOURCE_A}")));
        let two = source_label(&addr(&format!("/p2p/{SOURCE_B}")));
        assert_ne!(
            one, two,
            "two sources over ONE relay connection get separate pre-auth budgets"
        );

        // AND NEITHER BUCKET IS THE RELAY'S IDENTITY, which is the one
        // §10 says both connections should have shared.
        //
        // Asserted against the relay's PeerId rather than against
        // `source_label(&local)`: that call returns `127.0.0.1`, the
        // relay's IP, so comparing with it would pass for reasons
        // having nothing to do with identity. The bucket §10 names is
        // the relay PeerId, so that is what must be absent.
        assert!(
            !one.contains(RELAY) && !two.contains(RELAY),
            "neither bucket names the relay ({one}, {two}), so the connection they share              is not what either was charged to"
        );
        assert!(
            one.contains(SOURCE_A) && two.contains(SOURCE_B),
            "each bucket is its own SOURCE's identity, which is the metadata §10 forbids              bucketing on ({one}, {two})"
        );
    }
}
