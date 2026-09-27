// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The status surface's dial-gate half (plan §15), read through the
//! public handle while real connections come and go.
//!
//! Each count is watched MOVING, from a fresh runtime's zeros: a count
//! that is only ever read at one value asserts nothing about what it
//! reads. The connectivity half is `status.rs`'s unit tests for the rule
//! and `tests/connectivity` for a relay's reservation moving it.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{
    DirectInboundState, PathReadiness, PreferredPathPolicy, TransportIdentity,
};
use interweave_transport_libp2p::{SubstrateConfig, SwarmEvent, SwarmRuntime};
use interweave_transport_runtime::TrustSources;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::Multiaddr;

const PATIENCE: Duration = Duration::from_secs(20);

fn trusting(peer: &TransportIdentity) -> TrustSources {
    TrustSources::new(
        PeerTrustPolicy::new([peer.clone()]).expect("one peer"),
        InfrastructureSet::default(),
    )
}

async fn wait_for(
    runtime: &mut SwarmRuntime,
    what: &str,
    mut predicate: impl FnMut(&SwarmEvent) -> bool,
) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, runtime.next_event()).await {
            Ok(Some(event)) if predicate(&event) => return,
            Ok(Some(_)) => {}
            Ok(None) => panic!("the runtime stopped while waiting for {what}"),
            Err(_) => panic!("no {what} within {PATIENCE:?}"),
        }
    }
}

#[tokio::test]
async fn a_fresh_runtimes_status_is_all_zeros_and_the_summary_says_nothing_is_there() {
    let subject = ProfileIdentity::generate();
    let peer = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id");
    let runtime =
        SwarmRuntime::start(&subject, SubstrateConfig::default(), trusting(&peer)).expect("starts");

    let status = runtime.status(Some(peer)).await.expect("answered");
    let gate = &status.dial_gate;
    assert_eq!(
        (
            gate.connections,
            gate.published_connections,
            gate.published_pending_dials,
            gate.scheduled_retries,
            gate.address_entries,
            gate.peer_entries,
        ),
        (0, 0, 0, 0, 0, 0)
    );
    assert_eq!(
        gate.peer_retry_due,
        Some(false),
        "asked, and nothing is due"
    );
    let summary = &status.connectivity;
    assert_eq!(summary.direct_inbound, DirectInboundState::Unknown);
    assert_eq!(summary.relay_inbound, PathReadiness::Unavailable);
    assert_eq!(
        summary.preferred_path_policy,
        PreferredPathPolicy::DirectFirst
    );
    assert_eq!(
        (
            summary.active_relay_reservations,
            summary.target_relay_reservations,
            summary.active_relayed_peer_paths,
            summary.hole_punch_inflight,
        ),
        (0, 0, 0, 0)
    );
    assert!(summary.updated_at > 0, "a wall-clock photograph time");
    assert_eq!(
        status.autonat_rejected_candidates, None,
        "the client is off"
    );
    assert_eq!(
        status.kademlia_record_writes_dropped, None,
        "Kademlia is off"
    );
    assert_eq!(status.direct_reservations_outstanding, 0);

    let unasked = runtime.status(None).await.expect("answered");
    assert_eq!(unasked.dial_gate.peer_retry_due, None);

    runtime.shutdown().await.expect("clean shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connection_is_counted_by_the_manager_and_its_published_snapshot() {
    let listener_id = ProfileIdentity::generate();
    let dialer_id = ProfileIdentity::generate();
    let listener_peer = listener_id.transport_identity().expect("peer id");
    let dialer_peer = dialer_id.transport_identity().expect("peer id");
    let listener = SwarmRuntime::start(
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
    let before = dialer.status(None).await.expect("answered");

    dialer
        .dial(listener_peer.clone(), address)
        .await
        .expect("delivered")
        .expect("admitted");
    wait_for(&mut dialer, "the connection", |e| {
        matches!(e, SwarmEvent::Connected { .. })
    })
    .await;

    let after = dialer.status(None).await.expect("answered");
    assert_eq!(before.dial_gate.connections, 0);
    assert_eq!(after.dial_gate.connections, 1, "{after:?}");
    assert_eq!(after.dial_gate.published_connections, 1, "{after:?}");
    assert_eq!(after.dial_gate.published_pending_dials, 0, "settled");
    assert!(
        after.dial_gate.revision > before.dial_gate.revision,
        "the settlement republished the policy"
    );
    assert_eq!(
        after.connectivity.active_relayed_peer_paths, 0,
        "a direct connection is not a relayed path"
    );

    dialer.shutdown().await.expect("clean shutdown");
    listener.shutdown().await.expect("clean shutdown");
}

#[tokio::test]
async fn a_failed_dial_schedules_a_retry_that_is_not_yet_due() {
    let subject = ProfileIdentity::generate();
    let peer = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id");
    let stranger = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id");
    let mut runtime =
        SwarmRuntime::start(&subject, SubstrateConfig::default(), trusting(&peer)).expect("starts");

    // A port nothing listens on: bound, read, released.
    let closed: Multiaddr = {
        let socket = std::net::TcpListener::bind("127.0.0.1:0").expect("binds");
        let port = socket.local_addr().expect("bound").port();
        format!("/ip4/127.0.0.1/tcp/{port}").parse().expect("valid")
    };
    runtime
        .dial(peer.clone(), closed)
        .await
        .expect("delivered")
        .expect("admitted");
    wait_for(&mut runtime, "the dial's failure", |e| {
        matches!(e, SwarmEvent::DialFailed { .. })
    })
    .await;

    let status = runtime.status(Some(peer)).await.expect("answered");
    assert_eq!(status.dial_gate.scheduled_retries, 1, "{status:?}");
    assert_eq!(
        status.dial_gate.peer_retry_due,
        Some(false),
        "scheduled behind its backoff, not due at once"
    );
    assert!(
        status.dial_gate.address_entries >= 1,
        "the failed address is scored in the bounded table: {status:?}"
    );
    assert_eq!(status.dial_gate.published_pending_dials, 0, "settled");
    let control = runtime.status(Some(stranger)).await.expect("answered");
    assert_eq!(
        control.dial_gate.peer_retry_due,
        Some(false),
        "the control: a peer with no schedule is not due either"
    );

    runtime.shutdown().await.expect("clean shutdown");
}
