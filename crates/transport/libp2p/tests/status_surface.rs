// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The status surface (plan §15), read through the public handle while
//! real connections come and go.
//!
//! WHAT IS WATCHED MOVING HERE, and where the rest is, since a field read
//! at one value asserts nothing about what it reads (#135 review F2):
//!
//! - here, over real sockets: `established_connections`,
//!   `connection_slots` and `pending_dials` apart from each other with a
//!   dial in flight and together once it settles; `revision`;
//!   `scheduled_retries`, `address_entries` and `peer_entries` after a
//!   failed dial;
//! - `status.rs`'s unit test: every dial-gate field through the same
//!   function the task calls, `peer_retry_due` reaching `true` included
//!   (on a running node the scheduler claims a due retry at its next
//!   tick, so the window is not one a wire test can hold open);
//! - here too: `broadcast_join_references` through joins and leaves,
//!   and the pre-authentication counts through a handshake held open and
//!   dropped;
//! - `tests/direct-v2` and `tests/pubsub`: the ingress limiters'
//!   tracked peers, each lane counting its own senders and not the
//!   other's;
//! - `tests/connectivity`: the relay reservations and readiness
//!   (`relay_client.rs`) and the relayed peer paths (`relayed_paths.rs`).
//!
//! NOT WATCHED MOVING anywhere: `direct_inbound` (loopback gives AutoNAT
//! no verdict), `hole_punch_inflight` (a punch's window is not caught),
//! and the three diagnostics -- AutoNAT's refused candidates, Kademlia's
//! dropped record writes, the dedup reservations -- which are read here
//! only in their off state. Each is its source's own reader, tested at
//! the source; the wiring from source to field is not.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{
    ChannelId, DirectInboundState, PathReadiness, PreferredPathPolicy, TransportIdentity,
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
            Err(elapsed) => panic!("no {what} within {PATIENCE:?} ({elapsed})"),
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
            gate.established_connections,
            gate.connection_slots,
            gate.pending_dials,
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
    assert_eq!(
        (
            status.ingress.direct_tracked_peers,
            status.ingress.broadcast_tracked_peers
        ),
        (0, 0),
        "no peer has sent on either lane"
    );

    let unasked = runtime.status(None).await.expect("answered");
    assert_eq!(unasked.dial_gate.peer_retry_due, None);

    runtime.shutdown().await.expect("clean shutdown");
}

/// Each (channel, session) join is one reference, read through the task's
/// own wiring. Releasing a session ends its leases and NOT its joins --
/// which is why a binding's session leaves its channels when it ends --
/// so only the last leave brings the count back to zero.
#[tokio::test]
async fn join_references_move_with_joins_and_leaves() {
    let subject = ProfileIdentity::generate();
    let peer = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id");
    let runtime =
        SwarmRuntime::start(&subject, SubstrateConfig::default(), trusting(&peer)).expect("starts");
    let channel = ChannelId::parse("ops").expect("legal");
    let references = || async {
        runtime
            .status(None)
            .await
            .expect("answered")
            .broadcast_join_references
    };

    assert_eq!(references().await, 0);
    for session in ["a", "b"] {
        runtime
            .join(channel.clone(), session)
            .await
            .expect("answered")
            .expect("joins");
    }
    assert_eq!(references().await, 2);
    runtime.leave(channel.clone(), "a").await.expect("answered");
    assert_eq!(references().await, 1);
    runtime.release_session("b").await.expect("answered");
    assert_eq!(references().await, 1, "releasing leases leaves joins");
    runtime.leave(channel, "b").await.expect("answered");
    assert_eq!(references().await, 0);

    runtime.shutdown().await.expect("clean shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connection_is_one_established_connection_in_one_slot() {
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
    assert_eq!(before.dial_gate.established_connections, 0);
    assert_eq!(after.dial_gate.established_connections, 1, "{after:?}");
    assert_eq!(after.dial_gate.connection_slots, 1, "{after:?}");
    assert_eq!(after.dial_gate.pending_dials, 0, "settled");
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
    let gate = &status.dial_gate;
    assert_eq!(gate.scheduled_retries, 1, "{status:?}");
    assert_eq!(
        gate.peer_retry_due,
        Some(false),
        "scheduled behind its backoff, not due at once"
    );
    assert_eq!(gate.address_entries, 1, "the address is scored: {status:?}");
    assert_eq!(gate.peer_entries, 1, "and the peer backed off: {status:?}");
    assert_eq!(
        (gate.connection_slots, gate.pending_dials),
        (0, 0),
        "settled"
    );

    runtime.shutdown().await.expect("clean shutdown");
}

/// A dial in flight holds a slot and a pending count and is not an
/// established connection -- the three fields apart, which is what makes
/// them three (#135 review F1). A documentation-range address the host
/// routes and nothing answers, so the dial is still in flight when read.
#[tokio::test]
async fn a_dial_in_flight_holds_a_slot_and_is_not_established() {
    let subject = ProfileIdentity::generate();
    let peer = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id");
    let runtime =
        SwarmRuntime::start(&subject, SubstrateConfig::default(), trusting(&peer)).expect("starts");

    runtime
        .dial(peer, "/ip4/192.0.2.1/tcp/4001".parse().expect("valid"))
        .await
        .expect("delivered")
        .expect("admitted");
    let status = runtime.status(None).await.expect("answered");
    let gate = &status.dial_gate;
    assert_eq!(
        (
            gate.established_connections,
            gate.connection_slots,
            gate.pending_dials
        ),
        (0, 1, 1),
        "one dial in flight, nothing open: {status:?}"
    );

    runtime.shutdown().await.expect("clean shutdown");
}

/// The pre-authentication counts move with a handshake: a raw TCP
/// connection that never speaks holds one open -- one pending, its source
/// tracked -- and dropping it releases the slot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_handshake_held_open_is_pending_and_its_source_tracked() {
    let subject = ProfileIdentity::generate();
    let peer = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id");
    let runtime =
        SwarmRuntime::start(&subject, SubstrateConfig::default(), trusting(&peer)).expect("starts");
    let address = runtime
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("listens");
    let port = address
        .iter()
        .find_map(|p| match p {
            libp2p::multiaddr::Protocol::Tcp(port) => Some(port),
            _ => None,
        })
        .expect("a tcp port");
    let before = runtime.status(None).await.expect("answered").pre_auth;
    assert_eq!(
        (before.pending, before.tracked_sources),
        (0, 0),
        "{before:?}"
    );

    let held = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connects");
    let mut seen = before;
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while seen.pending == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "never pending: {seen:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        seen = runtime.status(None).await.expect("answered").pre_auth;
    }
    assert_eq!(seen.pending, 1, "{seen:?}");
    assert_eq!(seen.tracked_sources, 1, "{seen:?}");

    drop(held);
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while seen.pending != 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "never released: {seen:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        seen = runtime.status(None).await.expect("answered").pre_auth;
    }
    runtime.shutdown().await.expect("clean shutdown");
}
