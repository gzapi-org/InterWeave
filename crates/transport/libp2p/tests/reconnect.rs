// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `SwarmRuntime::reconnect`: the composition root's dial toward a peer
//! discovery found, under `DialOrigin::DiscoveryReconnect`
//! (`transport/libp2p/CONNECTIVITY.md` §11).
//!
//! - it reaches a data-plane peer through the book, over real sockets;
//! - it dials nothing while the peer holds a connection;
//! - toward an infrastructure-only peer it is refused at the gate, a
//!   policy refusal though the book holds a route -- a reconnection loop
//!   does not re-establish infrastructure (§4).
//!
//! NOT OBSERVED HERE: the origin itself. `DiscoveryReconnect` and
//! `Manual` are admitted alike today (both name an application
//! destination), so a reconnect dialled under `Manual` would pass these
//! tests; the origin is the label plan §11 asks every dial to carry, and
//! nothing yet reports a command-path dial's origin.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::{DialRefusal, SubstrateConfig, SwarmEvent, SwarmRuntime};
use interweave_transport_runtime::TrustSources;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};

const PATIENCE: Duration = Duration::from_secs(20);

fn data_plane(peer: &TransportIdentity) -> TrustSources {
    TrustSources::new(
        PeerTrustPolicy::new([peer.clone()]).expect("one peer"),
        InfrastructureSet::default(),
    )
}

async fn wait_connected(runtime: &mut SwarmRuntime, peer: &TransportIdentity) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, runtime.next_event()).await {
            Ok(Some(SwarmEvent::Connected { peer: got, .. })) if &got == peer => return,
            Ok(Some(_)) => {}
            Ok(None) => panic!("the runtime stopped"),
            Err(elapsed) => panic!("no connection within {PATIENCE:?} ({elapsed})"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reconnect_reaches_a_learned_peer_and_dials_nothing_once_connected() {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let subject_id = ProfileIdentity::generate();
    let target_id = ProfileIdentity::generate();
    let subject_peer = subject_id.transport_identity().expect("peer id");
    let target_peer = target_id.transport_identity().expect("peer id");
    let mut subject = SwarmRuntime::start(
        &subject_id,
        SubstrateConfig::default(),
        data_plane(&target_peer),
    )
    .expect("starts");
    let target = SwarmRuntime::start(
        &target_id,
        SubstrateConfig::default(),
        data_plane(&subject_peer),
    )
    .expect("starts");
    let _ = subject
        .listen(format!("/ip4/{ip}/tcp/0").parse().expect("valid"))
        .await
        .expect("listens");
    let target_addr = target
        .listen(format!("/ip4/{ip}/tcp/0").parse().expect("valid"))
        .await
        .expect("listens");

    assert_eq!(
        subject
            .learn(target_peer.clone(), [target_addr.to_string()])
            .await
            .expect("delivered"),
        1
    );
    subject
        .reconnect(target_peer.clone())
        .await
        .expect("delivered")
        .expect("admitted");
    wait_connected(&mut subject, &target_peer).await;

    // CONNECTED, SO NOTHING IS DIALLED: no pending dial and one slot.
    subject
        .reconnect(target_peer.clone())
        .await
        .expect("delivered")
        .expect("answered without a dial");
    let gate = subject.status(None).await.expect("answered").dial_gate;
    assert_eq!(
        (gate.established_connections, gate.pending_dials),
        (1, 0),
        "{gate:?}"
    );

    subject.shutdown().await.expect("clean shutdown");
    target.shutdown().await.expect("clean shutdown");
}

#[tokio::test]
async fn a_reconnect_toward_an_infrastructure_only_peer_is_refused_under_its_own_origin() {
    let subject = ProfileIdentity::generate();
    let infra = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id");
    let trust = TrustSources::new(
        PeerTrustPolicy::new(std::iter::empty()).expect("an empty allowlist"),
        InfrastructureSet::new([infra.clone()]).expect("one peer"),
    );
    let runtime = SwarmRuntime::start(&subject, SubstrateConfig::default(), trust).expect("starts");
    assert_eq!(
        runtime
            .learn(infra.clone(), ["/ip4/1.2.3.4/tcp/4001".to_owned()])
            .await
            .expect("delivered"),
        1,
        "the book keys an infrastructure-only peer: it is classified"
    );

    let refused = runtime.reconnect(infra.clone()).await.expect("delivered");
    assert!(
        matches!(refused, Err(DialRefusal::Policy(_))),
        "the gate refuses it -- a book entry exists, so this is not NoKnownAddress: {refused:?}"
    );
    assert_eq!(
        runtime
            .status(None)
            .await
            .expect("answered")
            .dial_gate
            .pending_dials,
        0,
        "and no dial is in flight toward it"
    );

    runtime.shutdown().await.expect("clean shutdown");
}

/// Repeated reconnects to an address that never answers hold ONE dial,
/// not one per ask (#137 review F1): the composition root asks every
/// round, and a dial per round filled the pending-dial ceiling and
/// settled one outage as one failure per round. A documentation-range
/// address the host routes and nothing answers keeps the dial pending
/// across the asks.
#[tokio::test]
async fn repeated_reconnects_to_an_unanswering_peer_hold_one_pending_dial() {
    let subject = ProfileIdentity::generate();
    let peer = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id");
    let runtime = SwarmRuntime::start(&subject, SubstrateConfig::default(), data_plane(&peer))
        .expect("starts");
    assert_eq!(
        runtime
            .learn(peer.clone(), ["/ip4/192.0.2.1/tcp/4001".to_owned()])
            .await
            .expect("delivered"),
        0,
        "the discovery door refuses a documentation-range address"
    );
    // The operator's door holds it, so the book has a route to dial.
    assert!(
        runtime
            .add_address(
                peer.clone(),
                "/ip4/192.0.2.1/tcp/4001".parse().expect("valid")
            )
            .await
            .expect("delivered")
    );

    for _ in 0..3 {
        runtime
            .reconnect(peer.clone())
            .await
            .expect("delivered")
            .expect("admitted or already in flight");
    }
    let gate = runtime.status(None).await.expect("answered").dial_gate;
    assert_eq!(
        (gate.pending_dials, gate.connection_slots),
        (1, 1),
        "three asks, one dial: {gate:?}"
    );

    runtime.shutdown().await.expect("clean shutdown");
}
