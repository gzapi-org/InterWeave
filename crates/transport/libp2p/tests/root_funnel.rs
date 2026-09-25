// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! ADR-0052 A 2026-09-25 D2: the measurement the root funnel was built
//! to, before it was wired around the real composition.
//!
//! D1 rests on one claim about the pinned Swarm: that a dial built with
//! `extend_addresses_through_behaviour` is extended ONLY from what the
//! ROOT behaviour's `handle_pending_outbound_connection` returns, so a
//! wrapper around the composite sees -- and can prune -- every address
//! any field contributes. That was read in `libp2p-swarm 0.48.0`
//! `Swarm::dial`. These tests measure it, on real sockets, with the
//! behaviour that dials that way: Kademlia (`DialOpts::peer_id(p)
//! .build()`, `libp2p-kad 0.49.0`).
//!
//! # The observable, and why it has a control
//!
//! A raw TCP listener on loopback stands where a peer-supplied address
//! points. "A socket opened" is its `accept` returning -- the TCP
//! connect, before libp2p's handshake fails against something that is
//! not a libp2p node. The funnel's claim is an ABSENCE (no socket), so
//! it is only evidence beside a control proving the same setup DOES
//! open one without the funnel: `the_control_kademlia_dials_what_its_
//! table_holds`. Without that, "no connection" is equally explained by
//! a query that never dialled anybody.
//!
//! Loopback is the address because it is what the floor refuses and
//! what a single host can listen on. The `/dns4` name is measured by the
//! funnel's own count, since resolving it would need a nameserver this
//! test does not control.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use futures::StreamExt;
use interweave_transport_libp2p::root_funnel::RootFunnel;
use libp2p::kad::{self, store::MemoryStore};
use libp2p::swarm::{NetworkBehaviour, Swarm, dial_opts::DialOpts};
use libp2p::{Multiaddr, PeerId, noise, tcp, yamux};
use tokio::net::TcpListener;

/// Long enough for a Kademlia query to reach its first dial on a loaded
/// CI runner; the negative runs wait the same, so a slow dial cannot
/// pass as a pruned one.
const WINDOW: Duration = Duration::from_secs(5);

#[derive(NetworkBehaviour)]
#[behaviour(prelude = "libp2p::swarm::derive_prelude")]
struct Composite {
    kad: kad::Behaviour<MemoryStore>,
}

fn composite(key: &libp2p::identity::Keypair) -> Composite {
    let local = key.public().to_peer_id();
    Composite {
        kad: kad::Behaviour::new(local, MemoryStore::new(local)),
    }
}

fn swarm_of<B: NetworkBehaviour + Send + 'static>(
    make: impl FnOnce(&libp2p::identity::Keypair) -> B,
) -> Swarm<B> {
    libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        // Default configurations only: a yamux `Config` SETTER swaps the
        // muxer onto a version with a remote-panic DoS
        // (`check_yamux_muxer.sh`), so none is called.
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .expect("tcp")
        .with_behaviour(|key| make(key))
        .expect("behaviour")
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(5)))
        .build()
}

/// A loopback listener, and the multiaddr that names it.
async fn trap() -> (TcpListener, Multiaddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let addr: Multiaddr = format!("/ip4/127.0.0.1/tcp/{port}").parse().expect("valid");
    (listener, addr)
}

/// Drive `swarm` for `WINDOW` and report whether `listener` saw a TCP
/// connection in that time.
async fn socket_opened<B: NetworkBehaviour>(swarm: &mut Swarm<B>, listener: &TcpListener) -> bool
where
    B::ToSwarm: std::fmt::Debug,
{
    let deadline = tokio::time::sleep(WINDOW);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            accepted = listener.accept() => return accepted.is_ok(),
            _ = swarm.select_next_some() => {}
            () = &mut deadline => return false,
        }
    }
}

/// THE CONTROL. Without the funnel, a Kademlia walk dials the loopback
/// address its table holds for a peer, and the socket opens. Every
/// absence below is evidence only because this presence holds.
#[tokio::test]
async fn the_control_kademlia_dials_what_its_table_holds() {
    let (listener, trapped) = trap().await;
    let mut swarm = swarm_of(composite);
    let peer = PeerId::random();
    swarm.behaviour_mut().kad.add_address(&peer, trapped);
    let _ = swarm
        .behaviour_mut()
        .kad
        .get_closest_peers(PeerId::random());

    assert!(
        socket_opened(&mut swarm, &listener).await,
        "the control: an unwrapped Kademlia walk must dial the address in its table -- \
         if it does not, the funnel test below proves nothing"
    );
}

/// D1, MEASURED. The same walk, the same table, the composite wrapped in
/// the root funnel: the loopback address is pruned before a socket and
/// counted; the `/dns4` name beside it is pruned before a resolver.
#[tokio::test]
async fn the_root_funnel_prunes_what_a_behaviour_extends_a_dial_with() {
    let (listener, trapped) = trap().await;
    let mut swarm = swarm_of(|key| RootFunnel::new(composite(key)));
    let counters = swarm.behaviour().counters();
    let peer = PeerId::random();
    let name: Multiaddr = "/dns4/a-name-a-peer-chose.invalid/tcp/4001"
        .parse()
        .expect("valid");
    swarm
        .behaviour_mut()
        .inner_mut()
        .kad
        .add_address(&peer, trapped);
    swarm
        .behaviour_mut()
        .inner_mut()
        .kad
        .add_address(&peer, name);
    let _ = swarm
        .behaviour_mut()
        .inner_mut()
        .kad
        .get_closest_peers(PeerId::random());

    assert!(
        !socket_opened(&mut swarm, &listener).await,
        "a loopback address Kademlia contributed must be pruned at the root before any \
         socket opens (ADR-0052 A 2026-09-25 D1)"
    );
    let seen = counters.snapshot();
    assert!(
        seen.candidates_removed
            .get("special_use")
            .copied()
            .unwrap_or(0)
            >= 1,
        "the loopback address must be counted as pruned, by class: {seen:?}"
    );
    assert!(
        seen.candidates_removed
            .get("not_literal")
            .copied()
            .unwrap_or(0)
            >= 1,
        "the /dns4 name beside it must be pruned too, before any resolver: {seen:?}"
    );
    assert!(
        seen.dials_denied >= 1,
        "with every contributed address removed and none of its own, the dial is DENIED \
         -- told to Kademlia -- rather than left to fail as NoAddresses: {seen:?}"
    );
    assert_eq!(
        seen.passed, 0,
        "and nothing passed, since both contributed addresses were refused: {seen:?}"
    );
}

/// D1'S OTHER HALF: the funnel prunes only the EXTENSION. A dial's own
/// explicit address is the caller's (rule 5), and it opens its socket
/// through the funnel exactly as it would without it.
#[tokio::test]
async fn an_explicit_address_passes_the_root_funnel_untouched() {
    let (listener, trapped) = trap().await;
    let mut swarm = swarm_of(|key| RootFunnel::new(composite(key)));
    let counters = swarm.behaviour().counters();

    swarm
        .dial(
            DialOpts::peer_id(PeerId::random())
                .addresses(vec![trapped])
                .build(),
        )
        .expect("the dial starts");

    assert!(
        socket_opened(&mut swarm, &listener).await,
        "an explicit address is the caller's and must reach its socket: the funnel \
         prunes what behaviours ADD, never what the caller asked for"
    );
    assert_eq!(
        counters.snapshot().candidates_removed_total(),
        0,
        "and nothing was pruned, because nothing was extended"
    );
}
