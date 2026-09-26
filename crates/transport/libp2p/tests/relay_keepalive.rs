// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The relay-control keepalive (`transport/libp2p/CONNECTIVITY.md` §14
//! item 5) over real sockets, with [`RelayKeepalive`] in bare Swarms at
//! a fast interval -- the runtime's is the crate's default, fifteen
//! seconds with twenty to answer, and a missed ping would take a minute
//! to show at it.
//!
//! Proved here:
//! - no ping crosses a connection whose peer holds no reservation of
//!   the client's -- the control;
//! - switched on, the client pings and the relay ANSWERS, the relay
//!   never pinging back -- and, the other way round, a relay pings the
//!   peer holding its reservation, which answers without pinging;
//! - a path that stops carrying anything -- a proxy between the two
//!   that goes silent with both sockets open, the shape of a NAT
//!   rebinding or a carrier drop, which raises no event at either end
//!   -- has its connection CLOSED by the client, counted as missed;
//! - a relay that does not speak ping is kept: not a dead path.

#![allow(clippy::expect_used, clippy::panic)]

use std::collections::HashSet;
use std::time::Duration;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::StreamExt as _;
use interweave_transport_libp2p::relay_keepalive::{KeepaliveCounters, RelayKeepalive};
use libp2p::swarm::{SwarmEvent, dummy};
use libp2p::{Multiaddr, PeerId, Swarm, ping};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

const FAST: Duration = Duration::from_millis(200);

fn swarm<B: libp2p::swarm::NetworkBehaviour>(behaviour: B) -> Swarm<B> {
    libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("tcp")
        .with_behaviour(|_| behaviour)
        .expect("behaviour")
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(600)))
        .build()
}

fn keepalive() -> RelayKeepalive {
    RelayKeepalive::with_config(ping::Config::new().with_interval(FAST).with_timeout(FAST))
}

async fn listening<B: libp2p::swarm::NetworkBehaviour>(s: &mut Swarm<B>) -> Multiaddr {
    s.listen_on("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .expect("listens");
    loop {
        if let SwarmEvent::NewListenAddr { address, .. } = s.select_next_some().await {
            return address;
        }
    }
}

/// A TCP proxy in front of `target` that forwards until `silent` is
/// set and then forwards nothing, holding both sockets open: no FIN, no
/// RST, nothing either end can see but silence.
async fn proxy(target: std::net::SocketAddr, silent: Arc<AtomicBool>) -> Multiaddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
    let port = listener.local_addr().expect("bound").port();
    tokio::spawn(async move {
        let (inbound, _) = listener.accept().await.expect("the client arrives");
        let outbound = TcpStream::connect(target).await.expect("the relay answers");
        let (in_r, in_w) = inbound.into_split();
        let (out_r, out_w) = outbound.into_split();
        let pipe = |silent: Arc<AtomicBool>| {
            move |mut r: tokio::net::tcp::OwnedReadHalf, mut w: tokio::net::tcp::OwnedWriteHalf| async move {
                let mut buf = [0u8; 4096];
                while let Ok(n) = r.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    if silent.load(Ordering::Acquire) {
                        // Swallowed, and the halves held: the path is
                        // dead and neither end is told.
                        std::future::pending::<()>().await;
                    }
                    if w.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            }
        };
        tokio::spawn(pipe(silent.clone())(in_r, out_w));
        pipe(silent)(out_r, in_w).await;
    });
    format!("/ip4/127.0.0.1/tcp/{port}").parse().expect("valid")
}

/// What the client saw of the relay's connection.
#[derive(Debug, Default)]
struct Seen {
    established: usize,
    closed: usize,
}

/// Drive the client, and the relay when `relay` is given, for `window`
/// or until `done` holds of what the client saw.
async fn drive<R: libp2p::swarm::NetworkBehaviour>(
    client: &mut Swarm<RelayKeepalive>,
    mut relay: Option<&mut Swarm<R>>,
    seen: &mut Seen,
    window: Duration,
    done: impl Fn(&Seen, KeepaliveCounters) -> bool,
) {
    let deadline = tokio::time::Instant::now() + window;
    while !done(seen, client.behaviour().counters()) {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        let relay_next = async {
            match relay.as_mut() {
                Some(r) => {
                    let _ = r.select_next_some().await;
                }
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            event = client.select_next_some() => match event {
                SwarmEvent::ConnectionEstablished { .. } => seen.established += 1,
                SwarmEvent::ConnectionClosed { .. } => seen.closed += 1,
                _ => {}
            },
            () = relay_next => {}
            // Re-read the counters often: a ping raises no Swarm event.
            () = tokio::time::sleep(remaining.min(Duration::from_millis(50))) => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_control_connection_is_pinged_and_closed_when_the_relay_stops_answering() {
    let mut relay = swarm(keepalive());
    let mut client = swarm(keepalive());
    let relay_peer: PeerId = *relay.local_peer_id();
    let addr = listening(&mut relay).await;
    let Some(libp2p::multiaddr::Protocol::Tcp(port)) = addr.iter().nth(1) else {
        panic!("a tcp listener: {addr}");
    };
    let silent = Arc::new(AtomicBool::new(false));
    let through = proxy(([127, 0, 0, 1], port).into(), silent.clone()).await;
    client.dial(through).expect("dials");
    let mut seen = Seen::default();
    drive(
        &mut client,
        Some(&mut relay),
        &mut seen,
        Duration::from_secs(10),
        |s, _| s.established == 1,
    )
    .await;
    assert_eq!(seen.established, 1, "connected");

    // THE CONTROL: no reservation, no ping -- ten intervals of nothing.
    drive(
        &mut client,
        Some(&mut relay),
        &mut seen,
        FAST * 10,
        |_, _| false,
    )
    .await;
    assert_eq!(client.behaviour().counters(), KeepaliveCounters::default());
    assert_eq!(relay.behaviour().counters().echoed, 0);

    // SWITCHED ON: the client pings, the relay answers and never pings.
    client
        .behaviour_mut()
        .set_relays(HashSet::from([relay_peer]));
    drive(
        &mut client,
        Some(&mut relay),
        &mut seen,
        Duration::from_secs(10),
        |_, c| c.answered >= 3,
    )
    .await;
    let answered = client.behaviour().counters().answered;
    assert!(answered >= 3, "{:?}", client.behaviour().counters());
    let relay_counts = relay.behaviour().counters();
    assert!(relay_counts.echoed >= 3, "{relay_counts:?}");
    assert_eq!(relay_counts.answered, 0, "the relay never pings");

    // THE PATH GOES SILENT: nothing crosses it, nothing closes it. The
    // client closes the connection -- once the crate reports its SECOND
    // failure (the first is forgiven), and that second one is the
    // stream it opens anew failing to negotiate, which takes the
    // Swarm's ten-second upgrade timeout whatever the ping interval.
    silent.store(true, Ordering::Release);
    drive(
        &mut client,
        Some(&mut relay),
        &mut seen,
        Duration::from_secs(30),
        |s, _| s.closed == 1,
    )
    .await;
    assert_eq!(
        seen.closed,
        1,
        "closed for the missed ping: {seen:?} {:?}",
        client.behaviour().counters()
    );
    assert_eq!(client.behaviour().counters().missed, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relay_pings_the_peer_holding_its_reservation_which_only_answers() {
    let mut relay = swarm(keepalive());
    let mut holder = swarm(keepalive());
    let holder_peer: PeerId = *holder.local_peer_id();
    let addr = listening(&mut relay).await;
    holder.dial(addr).expect("dials");
    let mut seen = Seen::default();
    drive(
        &mut relay,
        Some(&mut holder),
        &mut seen,
        Duration::from_secs(10),
        |s, _| s.established == 1,
    )
    .await;
    relay
        .behaviour_mut()
        .set_reserved(HashSet::from([holder_peer]));
    drive(
        &mut relay,
        Some(&mut holder),
        &mut seen,
        Duration::from_secs(10),
        |_, c| c.answered >= 3,
    )
    .await;
    assert!(
        relay.behaviour().counters().answered >= 3,
        "{:?}",
        relay.behaviour().counters()
    );
    let holder_counts = holder.behaviour().counters();
    assert!(holder_counts.echoed >= 3, "{holder_counts:?}");
    assert_eq!(holder_counts.answered, 0, "the holder never pings");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relay_that_does_not_speak_ping_is_kept() {
    let mut relay = swarm(dummy::Behaviour);
    let mut client = swarm(keepalive());
    let relay_peer: PeerId = *relay.local_peer_id();
    let addr = listening(&mut relay).await;
    client.dial(addr).expect("dials");
    let mut seen = Seen::default();
    drive(
        &mut client,
        Some(&mut relay),
        &mut seen,
        Duration::from_secs(10),
        |s, _| s.established == 1,
    )
    .await;
    client
        .behaviour_mut()
        .set_relays(HashSet::from([relay_peer]));
    drive(
        &mut client,
        Some(&mut relay),
        &mut seen,
        Duration::from_secs(10),
        |_, c| c.unsupported == 1,
    )
    .await;
    // Ten intervals more: nothing missed, nothing closed.
    drive(
        &mut client,
        Some(&mut relay),
        &mut seen,
        FAST * 10,
        |_, _| false,
    )
    .await;
    let counts = client.behaviour().counters();
    assert_eq!(counts.unsupported, 1, "{counts:?}");
    assert_eq!(counts.missed, 0, "{counts:?}");
    assert_eq!(seen.closed, 0, "kept: {seen:?}");
}
