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
/// The pre-authentication bucket a connection is charged to.
///
/// The ordinary case is the remote's IP, which is what an anonymous
/// party cannot mint more of cheaply. A multiaddr with no IP component
/// -- a memory transport -- yields the address as written; for that
/// transport it is the fail-closed direction, since it cannot merge two
/// peers into one bucket, only fail to merge two addresses that belong
/// together.
///
/// # A relayed inbound is charged to the RELAY
///
/// SPIKE-004 measured what one presents before Noise: a `remote_addr`
/// of `/p2p/<source>` with no IP anywhere, while the relay's own
/// identity sits in the LOCAL address. Reading only the remote made the
/// bucket the SOURCE's PeerId -- one bucket per identity over one relay
/// connection, and identities are free to mint. That was divergence D3,
/// against `contracts/CONNECTIVITY.md` §10, which requires charging the
/// authenticated relay transport connection and relay PeerId plus the
/// global caps and says a destination "MUST NOT create unbounded
/// pseudo-source buckets from circuit metadata". **Fixed in Stage 11
/// step 2**, before any relay feature is compiled.
///
/// The discriminator is the LOCAL address, and that is what makes this
/// safe: `local_addr` is this node's own listen address as the
/// transport built it, never anything the remote supplies, so a source
/// cannot make an ordinary connection look relayed and escape its IP
/// bucket. libp2p-relay 0.21.1 builds it as
/// `relay_addr.with(Protocol::P2pCircuit)` from the established relay
/// connection (`priv_client/transport.rs:404`), so the `/p2p-circuit`
/// component is present exactly when the connection rode a circuit.
///
/// The relay's PeerId is preferred over its IP because §10 names the
/// relay PeerId and because it is the AUTHENTICATED half: the relay
/// connection completed Noise before it could carry a circuit. The IP
/// is the fallback for a circuit whose local address somehow carries no
/// relay identity, which keeps the function total. Both are prefixed
/// `relay:` so a relayed bucket can never collide with a direct peer's
/// IP bucket -- otherwise a direct connection from the relay's own
/// address would share the budget of every circuit riding it.
fn source_label(local_addr: &Multiaddr, remote_addr: &Multiaddr) -> String {
    use libp2p::multiaddr::Protocol;

    // THE REMOTE IP FIRST, because a relayed connection has none and a
    // direct one is the overwhelmingly common case. A direct connection
    // is never charged to a relay bucket, whatever its local address
    // says, because this returns before the circuit check runs.
    for component in remote_addr {
        match component {
            Protocol::Ip4(ip) => return ip.to_string(),
            Protocol::Ip6(ip) => return ip.to_string(),
            _ => {}
        }
    }

    if local_addr.iter().any(|c| matches!(c, Protocol::P2pCircuit)) {
        let mut relay_ip = None;
        for component in local_addr {
            match component {
                Protocol::P2p(peer) => return format!("relay:{peer}"),
                Protocol::Ip4(ip) => relay_ip.get_or_insert(ip.to_string()),
                Protocol::Ip6(ip) => relay_ip.get_or_insert(ip.to_string()),
                _ => continue,
            };
        }
        if let Some(ip) = relay_ip {
            return format!("relay:{ip}");
        }
        // THE CIRCUIT BRANCH IS TERMINAL, and that is a third case
        // rather than a tidy-up. Falling through from here returns the
        // REMOTE, which on a circuit is `/p2p/<source>` -- D3
        // verbatim, one bucket per identity and identities free to
        // mint. The shape that reaches it is a circuit whose local
        // address carries neither a relay PeerId nor an IP, such as
        // `/memory/1/p2p-circuit`. The whole local address is the
        // coarsest label available here that the SOURCE does not
        // choose, so it is what the connection is charged to.
        // `a_circuit_with_neither_a_relay_identity_nor_an_ip_is_still_not_the_source`
        // fails if this returns anything derived from the remote.
        return format!("relay:{local_addr}");
    }

    remote_addr.to_string()
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
        local_addr: &Multiaddr,
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
        match self.gate.admit(&source_label(local_addr, remote_addr), now) {
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
            source_label(
                &addr("/ip4/203.0.113.1/tcp/4001"),
                &addr("/ip4/198.51.100.7/tcp/5001")
            ),
            "198.51.100.7"
        );
        assert_eq!(
            source_label(
                &addr("/ip4/203.0.113.1/tcp/4001"),
                &addr("/ip4/198.51.100.7/tcp/5002")
            ),
            "198.51.100.7",
            "a different port is the same host and must not be a second bucket"
        );
        assert_eq!(
            source_label(
                &addr("/ip4/203.0.113.1/tcp/4001"),
                &addr("/ip6/2001:db8::1/tcp/5001")
            ),
            "2001:db8::1"
        );
    }

    /// The documented fallback, asserted because the doc comment above
    /// `source_label` states it as a rule.
    #[test]
    fn an_address_with_no_ip_buckets_on_itself() {
        assert_eq!(
            source_label(&addr("/memory/1"), &addr("/memory/42")),
            "/memory/42"
        );
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
    /// This test was written in the DEFECT's shape so that the fix
    /// would fail here rather than pass silently. Stage 11 step 2 made
    /// the fix, and it did fail; it now asserts the required
    /// behaviour. Still unreachable in a shipped build — no relay
    /// feature is compiled — and live the moment the relay client
    /// lands, which is why it was fixed before that rather than after.
    #[test]
    fn a_relayed_inbound_is_charged_to_the_relay_not_the_source() {
        let local = addr(&format!("/ip4/127.0.0.1/tcp/4001/p2p/{RELAY}/p2p-circuit"));
        let remote = addr(&format!("/p2p/{SOURCE_A}"));
        assert_eq!(
            source_label(&local, &remote),
            format!("relay:{RELAY}"),
            "a relayed inbound is charged to the authenticated relay PeerId, which §10 \
             names, and never to the source identity the circuit carries"
        );
    }

    /// The fallback, so the function stays total.
    ///
    /// A circuit whose local address carries no relay identity is
    /// charged to the relay's IP rather than falling through to the
    /// source. Falling through is the one outcome §10 forbids, so the
    /// absence of a PeerId must not produce it.
    #[test]
    fn a_circuit_with_no_relay_identity_still_avoids_the_source_bucket() {
        let local = addr("/ip4/127.0.0.1/tcp/4001/p2p-circuit");
        let remote = addr(&format!("/p2p/{SOURCE_A}"));
        let label = source_label(&local, &remote);
        assert_eq!(label, "relay:127.0.0.1");
        assert!(
            !label.contains(SOURCE_A),
            "the source identity must never become the bucket, PeerId present or not"
        );
    }

    /// The claim above says "PeerId present or not", and this is the
    /// input that makes it true rather than lucky.
    ///
    /// The test above feeds the one no-PeerId shape that still carries
    /// an IP, so it agrees with the code for free: it never reaches
    /// the end of the circuit branch. A local address with a circuit
    /// component and NEITHER a relay identity NOR an IP does, and
    /// while the branch fell through it returned the remote --
    /// `/p2p/<source>`, which is D3 exactly. Review finding on PR #74.
    #[test]
    fn a_circuit_with_neither_a_relay_identity_nor_an_ip_is_still_not_the_source() {
        let local = addr("/memory/1/p2p-circuit");
        let remote = addr(&format!("/p2p/{SOURCE_A}"));
        let label = source_label(&local, &remote);
        assert!(
            !label.contains(SOURCE_A),
            "a circuit with no relay identity and no relay IP must still not bucket on              the source; got {label}"
        );
        // AND THE TWO SOURCES SHARE IT, which is the property §10
        // asks for rather than merely "not the source": a label that
        // avoided SOURCE_A while still varying per identity would
        // satisfy the assertion above and none of the requirement.
        let other = source_label(&local, &addr(&format!("/p2p/{SOURCE_B}")));
        assert_eq!(
            label, other,
            "two identities over one relay must share one bucket whatever the relay's              address looks like"
        );
    }

    /// A DIRECT connection is never charged to a relay bucket.
    ///
    /// The discriminator is the local address, so this pins the
    /// direction that would be a regression rather than a defect: an
    /// ordinary inbound whose local address happens to carry a circuit
    /// component must still be charged to the remote's own IP, because
    /// the remote IP branch returns first.
    #[test]
    fn a_direct_inbound_keeps_its_ip_bucket_whatever_the_local_address_says() {
        let local = addr(&format!("/ip4/127.0.0.1/tcp/4001/p2p/{RELAY}/p2p-circuit"));
        assert_eq!(
            source_label(&local, &addr("/ip4/198.51.100.7/tcp/5001")),
            "198.51.100.7",
            "a remote with an IP is charged to it, and the local address does not override"
        );
    }

    /// The FIX's shape, not merely its value: one relay connection,
    /// two source identities, ONE bucket.
    ///
    /// §10's concern is proliferation — identities are free to mint, so
    /// the number of buckets was chosen by whoever was attacking. This
    /// asserted the two buckets DIFFERED, which was the property that
    /// made the per-source ceiling escapable; it now asserts they are
    /// the same, which is the property that closes it. Asserting one
    /// bucket's string alone would not: the point is that the count of
    /// buckets does not grow with the count of identities.
    #[test]
    fn two_relayed_sources_over_one_relay_share_one_bucket() {
        // The local address both connections arrive on, as SPIKE-004
        // measured it: one relay connection, named by the relay's own
        // PeerId, shared by every circuit riding it.
        let local = addr(&format!("/ip4/127.0.0.1/tcp/4001/p2p/{RELAY}/p2p-circuit"));
        assert!(
            local.to_string().contains("p2p-circuit"),
            "the shared connection is a circuit, which is what makes §10's rule apply"
        );
        let one = source_label(&local, &addr(&format!("/p2p/{SOURCE_A}")));
        let two = source_label(&local, &addr(&format!("/p2p/{SOURCE_B}")));
        assert_eq!(
            one, two,
            "two sources over ONE relay connection share one pre-auth budget"
        );

        // AND THE SHARED BUCKET IS THE RELAY'S IDENTITY, which is the
        // one §10 says both connections should have shared. Without
        // this the test would pass for a function that returned a
        // constant, which shares a bucket by erasing every distinction
        // rather than by charging the right party.
        assert!(
            one.contains(RELAY),
            "the shared bucket names the relay ({one}), so it is the connection they \
             actually share rather than an accident of collapsing every label"
        );
        assert!(
            !one.contains(SOURCE_A) && !one.contains(SOURCE_B),
            "neither source identity appears in the bucket ({one}), which is the metadata \
             §10 forbids bucketing on"
        );
    }
}
