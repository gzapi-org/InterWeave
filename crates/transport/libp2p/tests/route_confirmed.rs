// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `SwarmEvent::RouteConfirmed`: the route a dial of this profile's own
//! established, which the peer cache persists (#137 review F3).
//!
//! The dialling end reports the address it dialled, after `Connected`;
//! the ACCEPTING end reports nothing, since an inbound's remote address
//! is the peer's ephemeral source and never a route (#137 re-review N4).
//! That a dial whose address a peer chose (an AutoNAT dial-back, a
//! punch, a relay reservation, a Kademlia query) reports nothing either
//! is the emitter's origin filter; no test here drives one of those.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::{SubstrateConfig, SwarmEvent, SwarmRuntime};
use interweave_transport_runtime::TrustSources;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};

fn trusting(peer: &TransportIdentity) -> TrustSources {
    TrustSources::new(
        PeerTrustPolicy::new([peer.clone()]).expect("one peer"),
        InfrastructureSet::default(),
    )
}

/// Every event `runtime` yields for `window`.
async fn events_for(runtime: &mut SwarmRuntime, window: Duration) -> Vec<SwarmEvent> {
    let deadline = tokio::time::Instant::now() + window;
    let mut seen = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, runtime.next_event()).await {
            Ok(Some(event)) => seen.push(event),
            Ok(None) | Err(_) => return seen,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_dialler_confirms_its_route_after_connected_and_the_listener_confirms_nothing() {
    let listener_id = ProfileIdentity::generate();
    let dialer_id = ProfileIdentity::generate();
    let listener_peer = listener_id.transport_identity().expect("peer id");
    let dialer_peer = dialer_id.transport_identity().expect("peer id");
    let mut listener = SwarmRuntime::start(
        &listener_id,
        SubstrateConfig::default(),
        trusting(&dialer_peer),
    )
    .expect("starts");
    let mut dialer = SwarmRuntime::start(
        &dialer_id,
        SubstrateConfig::default(),
        trusting(&listener_peer),
    )
    .expect("starts");
    let address = listener
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("listens");
    dialer
        .dial(listener_peer.clone(), address.clone())
        .await
        .expect("delivered")
        .expect("admitted");

    let dialled = events_for(&mut dialer, Duration::from_secs(3)).await;
    let accepted = events_for(&mut listener, Duration::from_millis(500)).await;

    let connected = dialled
        .iter()
        .position(|e| matches!(e, SwarmEvent::Connected { .. }))
        .expect("the dialler connected");
    let confirmed = dialled
        .iter()
        .position(|e| matches!(e, SwarmEvent::RouteConfirmed { .. }))
        .expect("the dialler confirmed its route");
    assert!(confirmed > connected, "Connected first: {dialled:?}");
    let SwarmEvent::RouteConfirmed {
        peer,
        address: route,
    } = &dialled[confirmed]
    else {
        unreachable!("matched above");
    };
    assert_eq!(peer, &listener_peer);
    assert!(
        route.starts_with(&address.to_string()),
        "the route is the address dialled: {route} vs {address}"
    );

    assert!(
        accepted
            .iter()
            .any(|e| matches!(e, SwarmEvent::Connected { .. })),
        "the control: the listener saw the connection: {accepted:?}"
    );
    assert!(
        !accepted
            .iter()
            .any(|e| matches!(e, SwarmEvent::RouteConfirmed { .. })),
        "an inbound is never a route: {accepted:?}"
    );

    dialer.shutdown().await.expect("clean shutdown");
    listener.shutdown().await.expect("clean shutdown");
}

/// `SwarmRuntime::shutdown` returns what the substrate emitted and nobody
/// read, rather than dropping it with the receiver (#137 carried N1): a
/// dialler that never reads its events still gets its `Connected` and its
/// `RouteConfirmed` back at shutdown.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_returns_the_events_nobody_read() {
    let listener_id = ProfileIdentity::generate();
    let dialer_id = ProfileIdentity::generate();
    let listener_peer = listener_id.transport_identity().expect("peer id");
    let dialer_peer = dialer_id.transport_identity().expect("peer id");
    let mut listener = SwarmRuntime::start(
        &listener_id,
        SubstrateConfig::default(),
        trusting(&dialer_peer),
    )
    .expect("starts");
    let dialer = SwarmRuntime::start(
        &dialer_id,
        SubstrateConfig::default(),
        trusting(&listener_peer),
    )
    .expect("starts");
    let address = listener
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("listens");
    dialer
        .dial(listener_peer.clone(), address)
        .await
        .expect("delivered")
        .expect("admitted");
    // The listener's side is the witness that the connection is up.
    let seen = events_for(&mut listener, Duration::from_secs(3)).await;
    assert!(
        seen.iter()
            .any(|e| matches!(e, SwarmEvent::Connected { .. })),
        "the control: the connection came up: {seen:?}"
    );

    let unread = dialer.shutdown().await.expect("clean shutdown");
    assert!(
        unread.iter().any(
            |e| matches!(e, SwarmEvent::RouteConfirmed { peer, .. } if peer == &listener_peer)
        ),
        "the unread RouteConfirmed comes back at shutdown: {unread:?}"
    );
    listener.shutdown().await.expect("clean shutdown");
}
