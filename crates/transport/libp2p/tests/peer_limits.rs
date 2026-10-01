// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The two peer ceilings over real sockets: `max_connections_per_peer`
//! and `max_connected_peers`, decided once a connection's peer is
//! authenticated. Raw libp2p peers dial the subject -- they can open as
//! many connections to one peer as they like, which a substrate dialler
//! would not -- and the subject's status says what it kept.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use futures::StreamExt as _;
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::{SubstrateConfig, SwarmRuntime};
use interweave_transport_runtime::TrustSources;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::swarm::dial_opts::{DialOpts, PeerCondition};
use libp2p::swarm::{SwarmEvent, dummy};
use libp2p::{Multiaddr, PeerId, Swarm};

const PATIENCE: Duration = Duration::from_secs(20);

fn raw() -> Swarm<dummy::Behaviour> {
    libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("the substrate's transport stack")
        .with_behaviour(|_| dummy::Behaviour)
        .expect("behaviour")
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(600)))
        .build()
}

fn identity_of(swarm: &Swarm<dummy::Behaviour>) -> TransportIdentity {
    TransportIdentity::parse(swarm.local_peer_id().to_string()).expect("a peer id")
}

/// A subject trusting `peers`, listening on loopback; its address and
/// `PeerId`.
async fn subject(
    config: SubstrateConfig,
    peers: &[TransportIdentity],
) -> (SwarmRuntime, Multiaddr, PeerId) {
    let identity = ProfileIdentity::generate();
    let peer_id: PeerId = identity
        .transport_identity()
        .expect("a peer id")
        .as_str()
        .parse()
        .expect("a libp2p peer id");
    let runtime = SwarmRuntime::start(
        &identity,
        config,
        TrustSources::new(
            PeerTrustPolicy::new(peers.iter().cloned()).expect("peers"),
            InfrastructureSet::default(),
        ),
    )
    .expect("starts");
    let address = runtime
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("listens");
    (runtime, address, peer_id)
}

/// Open one more connection from `dialer` to `target`, whatever it holds
/// already, and drive it until it is established.
async fn connect(dialer: &mut Swarm<dummy::Behaviour>, target: PeerId, address: &Multiaddr) {
    dialer
        .dial(
            DialOpts::peer_id(target)
                .addresses(vec![address.clone()])
                .condition(PeerCondition::Always)
                .build(),
        )
        .expect("dials");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, dialer.select_next_some()).await {
            Ok(SwarmEvent::ConnectionEstablished { .. }) => return,
            Ok(SwarmEvent::OutgoingConnectionError { error, .. }) => panic!("dial failed: {error}"),
            Ok(_) => {}
            Err(elapsed) => panic!("not established within {PATIENCE:?} ({elapsed})"),
        }
    }
}

/// Drive the dialers until the subject holds `want` established
/// connections and has held it for a moment -- a refused one is closed
/// at once, so the count settles rather than overshoots.
async fn settles_at(runtime: &SwarmRuntime, dialers: &mut [Swarm<dummy::Behaviour>], want: usize) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut stable = 0;
    loop {
        for dialer in dialers.iter_mut() {
            let _ =
                tokio::time::timeout(Duration::from_millis(10), dialer.select_next_some()).await;
        }
        let held = runtime
            .status(None)
            .await
            .expect("answered")
            .dial_gate
            .established_connections;
        stable = if held == want { stable + 1 } else { 0 };
        if stable >= 20 {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the subject holds {held}, not {want}"
        );
    }
}

/// `max_connections_per_peer`: one peer's first two connections are
/// held, its third is refused -- and with the default of three, the
/// third is held (the control).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_peer_past_its_connection_ceiling_is_refused() {
    let mut dialer = raw();
    let me = identity_of(&dialer);
    let config = SubstrateConfig {
        max_connections_per_peer: 2,
        ..SubstrateConfig::default()
    };
    let (runtime, address, target) = subject(config, std::slice::from_ref(&me)).await;
    for _ in 0..3 {
        connect(&mut dialer, target, &address).await;
    }
    settles_at(&runtime, std::slice::from_mut(&mut dialer), 2).await;
    runtime.shutdown().await.expect("clean shutdown");

    let mut dialer = raw();
    let me = identity_of(&dialer);
    let (runtime, address, target) =
        subject(SubstrateConfig::default(), std::slice::from_ref(&me)).await;
    for _ in 0..3 {
        connect(&mut dialer, target, &address).await;
    }
    settles_at(&runtime, std::slice::from_mut(&mut dialer), 3).await;
    runtime.shutdown().await.expect("clean shutdown");
}

/// `max_connected_peers`: two peers are held, a third is refused -- and
/// a second connection from a peer already held is not a new place, so
/// it is kept beside them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_peer_past_the_connected_peer_ceiling_is_refused() {
    let mut dialers = vec![raw(), raw(), raw()];
    let peers: Vec<TransportIdentity> = dialers.iter().map(identity_of).collect();
    let config = SubstrateConfig {
        max_connected_peers: 2,
        ..SubstrateConfig::default()
    };
    let (runtime, address, target) = subject(config, &peers).await;
    for dialer in &mut dialers {
        connect(dialer, target, &address).await;
    }
    settles_at(&runtime, &mut dialers, 2).await;

    connect(&mut dialers[0], target, &address).await;
    settles_at(&runtime, &mut dialers, 3).await;
    runtime.shutdown().await.expect("clean shutdown");
}

/// The ceiling holds OUTBOUND too: the subject's own second dial to a
/// peer it holds at its ceiling of one is established, settled as a
/// working route, and not kept -- while under a ceiling of two the same
/// second dial is kept (the control).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_outbound_past_the_per_peer_ceiling_is_not_kept() {
    let mut listener = raw();
    let them = identity_of(&listener);
    listener
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .expect("listens");
    let address = loop {
        if let SwarmEvent::NewListenAddr { address, .. } = listener.select_next_some().await {
            break address;
        }
    };
    // The control first: under a ceiling of two the second dial IS a
    // second connection, so the refusal below is the ceiling's and not a
    // dial that never happened.
    for (ceiling, held) in [(2, [1, 2]), (1, [1, 1])] {
        let config = SubstrateConfig {
            max_connections_per_peer: ceiling,
            ..SubstrateConfig::default()
        };
        let (runtime, _, _) = subject(config, std::slice::from_ref(&them)).await;
        for want in held {
            runtime
                .dial(them.clone(), address.clone())
                .await
                .expect("delivered")
                .expect("admitted");
            settles_at(&runtime, std::slice::from_mut(&mut listener), want).await;
        }
        runtime.shutdown().await.expect("clean shutdown");
    }
}

/// A RECONNECT to a new peer waits for room: with the connected-peer
/// ceiling full, discovery's reconnect does not dial a peer it would only
/// refuse at retention -- which, settled as a working route, left nothing
/// to stop the next round dialling it again (#159 review F1). The
/// control: under a ceiling with room, the same reconnect connects.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reconnect_waits_while_the_connected_peer_ceiling_is_full() {
    async fn listening(swarm: &mut Swarm<dummy::Behaviour>) -> Multiaddr {
        swarm
            .listen_on("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
            .expect("listens");
        loop {
            if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
                return address;
            }
        }
    }
    for (ceiling, want_y) in [(1, 0), (2, 1)] {
        let mut x = raw();
        let mut y = raw();
        let (x_id, y_id) = (identity_of(&x), identity_of(&y));
        let (x_addr, y_addr) = (listening(&mut x).await, listening(&mut y).await);
        let config = SubstrateConfig {
            max_connected_peers: ceiling,
            ..SubstrateConfig::default()
        };
        let (runtime, _, _) = subject(config, &[x_id.clone(), y_id.clone()]).await;
        runtime
            .dial(x_id.clone(), x_addr)
            .await
            .expect("delivered")
            .expect("admitted");
        settles_at(&runtime, std::slice::from_mut(&mut x), 1).await;
        runtime
            .add_address(y_id.clone(), y_addr)
            .await
            .expect("added");

        // Five discovery rounds' worth of reconnects, Y's incoming counted.
        let mut arrivals = 0;
        for _ in 0..5 {
            let _ = runtime.reconnect(y_id.clone()).await.expect("delivered");
            let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
            while let Ok(event) = tokio::time::timeout_at(
                deadline,
                futures::future::select(y.select_next_some(), x.select_next_some()),
            )
            .await
            {
                // INCOMING, not established: a refused connection is torn
                // down before Y completes it, so counting establishments
                // saw nothing whether the subject dialled or not.
                if let futures::future::Either::Left((SwarmEvent::IncomingConnection { .. }, _)) =
                    event
                {
                    arrivals += 1;
                }
            }
        }
        if want_y == 0 {
            assert_eq!(arrivals, 0, "ceiling {ceiling}: no dial while it is full");
        } else {
            assert!(arrivals >= 1, "ceiling {ceiling}: the control connects");
        }
        runtime.shutdown().await.expect("clean shutdown");
    }
}
