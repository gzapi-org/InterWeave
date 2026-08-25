// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Stage 6 exit gate: direct v2 between real peers over loopback.
//!
//! Two `SwarmRuntime`s, a real TCP transport, a real Noise handshake, and
//! the real `/interweave/direct/2.0.0` codec. Nothing here is mocked,
//! because every property under test is one a mock would grant for free:
//! "the queue admitted it before AcceptedV2 was sent" is a statement
//! about ordering across a socket, and a stub responder answers in the
//! order the test wrote.
//!
//! SPIKE-002 exists because that distinction is not theoretical — every
//! finding it produced came from running the real scheduler, and four of
//! them are inherited by this stage.
#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{
    DirectMessageV2, EndpointId, MediaType, MessageId, Payload, TransportError, TransportIdentity,
};
use interweave_transport_libp2p::runtime::{
    DirectEndpoints, SubstrateConfig, SwarmEvent, SwarmRuntime,
};
use interweave_transport_runtime::{Generation, TrustSources};
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};

fn who() -> (ProfileIdentity, TransportIdentity) {
    let id = ProfileIdentity::generate();
    let peer = id.transport_identity().expect("peer id");
    (id, peer)
}

/// Trust exactly these peers. There is no allow-all constructor by
/// ADR-0012's design, so a test that forgot would fail as unauthorized
/// rather than quietly prove something else.
fn trusting(peers: &[&TransportIdentity]) -> TrustSources {
    TrustSources::new(
        PeerTrustPolicy::new(peers.iter().map(|p| (*p).clone())).expect("a handful"),
        InfrastructureSet::default(),
    )
}

fn endpoint(name: &str) -> EndpointId {
    EndpointId::parse(name).expect("valid endpoint id")
}

fn generation() -> Generation {
    Generation::parse("stage6__________").expect("valid generation")
}

/// `human` and `claude`, with `human` the default.
fn endpoints(queue_bound: usize) -> DirectEndpoints {
    DirectEndpoints {
        endpoints: vec![endpoint("human"), endpoint("claude")],
        default: Some(endpoint("human")),
        queue_bound,
        epoch: generation(),
    }
}

fn frame(destination: Option<&str>, body: &[u8], id: u8) -> DirectMessageV2 {
    DirectMessageV2 {
        message_id: MessageId::from_bytes([id; 16]),
        sent_at_ms: 1_000,
        source_endpoint: endpoint("human"),
        destination_endpoint: destination.map(endpoint),
        payload: Payload::at_ceiling(
            Some(MediaType::parse("text/plain").expect("valid media type")),
            body.to_vec(),
        )
        .expect("within the ceiling"),
    }
}

/// Two connected runtimes: a sender and a receiver that accepts direct
/// messages on `human` and `claude`.
async fn connected_pair(queue_bound: usize) -> (SwarmRuntime, SwarmRuntime, TransportIdentity) {
    let (sender_id, sender_peer) = who();
    let (receiver_id, receiver_peer) = who();

    let mut receiver = SwarmRuntime::start(
        &receiver_id,
        SubstrateConfig::default(),
        trusting(&[&sender_peer]),
    )
    .expect("the receiver starts");
    let sender = SwarmRuntime::start(
        &sender_id,
        SubstrateConfig::default(),
        trusting(&[&receiver_peer]),
    )
    .expect("the sender starts");

    receiver
        .configure_direct(endpoints(queue_bound))
        .await
        .expect("endpoints install");

    let address = receiver
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("a loopback address"))
        .await
        .expect("the receiver listens");

    sender
        .dial(receiver_peer.clone(), address)
        .await
        .expect("the command reaches the task")
        .expect("the dial is admitted");

    // Both sides must have the connection before a request can ride it.
    wait_connected(&mut receiver).await;

    (sender, receiver, receiver_peer)
}

/// Drive a runtime until it reports a connection.
///
/// Bounded, for the reason every loop in SPIKE-002's harness is: a
/// connection that never arrives is a RESULT, and a test that waits
/// forever reports nothing at all.
async fn wait_connected(runtime: &mut SwarmRuntime) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "no connection within 20s");
        match tokio::time::timeout(remaining, runtime.next_event()).await {
            Ok(Some(SwarmEvent::Connected { .. })) => return,
            Ok(Some(_)) => {}
            Ok(None) => panic!("the runtime stopped before connecting"),
            Err(_) => panic!("no connection within 20s"),
        }
    }
}

/// Scenario 1: two trusted peers, an accepted direct v2 exchange.
#[tokio::test]
async fn an_explicit_destination_reaches_exactly_that_endpoint() {
    let (sender, receiver, receiver_peer) = connected_pair(8).await;

    let resolved = sender
        .send_direct(receiver_peer, frame(Some("claude"), b"hello", 1))
        .await
        .expect("the command reaches the task")
        .expect("the exchange is accepted");
    assert_eq!(resolved, endpoint("claude"));

    // AcceptedV2 arrived, so the queue had already taken it.
    let delivered = receiver
        .drain_endpoint(endpoint("claude"))
        .await
        .expect("the receiver answers");
    assert_eq!(delivered.len(), 1, "exactly one delivery");
    assert_eq!(delivered[0].payload.bytes(), b"hello");
    assert_eq!(delivered[0].destination_endpoint, endpoint("claude"));

    assert!(
        receiver
            .drain_endpoint(endpoint("human"))
            .await
            .expect("answers")
            .is_empty(),
        "and nowhere else"
    );
}

/// Scenario 4: an omitted destination reaches the configured default,
/// and the response reports which endpoint that was.
#[tokio::test]
async fn an_omitted_destination_reaches_the_configured_default() {
    let (sender, receiver, receiver_peer) = connected_pair(8).await;

    let resolved = sender
        .send_direct(receiver_peer, frame(None, b"hello", 2))
        .await
        .expect("the command reaches the task")
        .expect("the exchange is accepted");
    assert_eq!(
        resolved,
        endpoint("human"),
        "the sender learns the remote's default from the response"
    );

    assert_eq!(
        receiver
            .drain_endpoint(endpoint("human"))
            .await
            .expect("answers")
            .len(),
        1
    );
    assert!(
        receiver
            .drain_endpoint(endpoint("claude"))
            .await
            .expect("answers")
            .is_empty(),
        "omitted means the default, never fan-out"
    );
}

/// Scenario 6: an unknown endpoint is `no_route`, and locally that is
/// `RemoteEndpointUnavailable` — the peer disclosed nothing more.
#[tokio::test]
async fn an_unknown_endpoint_is_indistinguishable_no_route() {
    let (sender, _receiver, receiver_peer) = connected_pair(8).await;

    let error = sender
        .send_direct(receiver_peer, frame(Some("nonexistent"), b"hello", 3))
        .await
        .expect("the command reaches the task")
        .expect_err("an unknown endpoint is refused");
    assert_eq!(error, TransportError::RemoteEndpointUnavailable);
}

/// Scenario 9: a full endpoint queue is `overloaded`, and NOT a false
/// acceptance. The queue bound is 1, so the second message finds it full.
#[tokio::test]
async fn a_full_endpoint_queue_is_overloaded_and_never_falsely_accepted() {
    let (sender, receiver, receiver_peer) = connected_pair(1).await;

    let first = sender
        .send_direct(receiver_peer.clone(), frame(Some("claude"), b"one", 4))
        .await
        .expect("the command reaches the task")
        .expect("the first is accepted");
    assert_eq!(first, endpoint("claude"));

    let error = sender
        .send_direct(receiver_peer, frame(Some("claude"), b"two", 5))
        .await
        .expect("the command reaches the task")
        .expect_err("the second finds the queue full");
    assert_eq!(error, TransportError::Overloaded);

    // EXACTLY ONE was delivered. A false acceptance would show as two.
    let delivered = receiver
        .drain_endpoint(endpoint("claude"))
        .await
        .expect("answers");
    assert_eq!(delivered.len(), 1, "the refused message was not enqueued");
    assert_eq!(delivered[0].payload.bytes(), b"one");
}

/// Scenario 10: a retry with matching content returns the ORIGINAL
/// resolved endpoint and does not deliver a second time — even after the
/// remote's default has changed.
#[tokio::test]
async fn a_matching_retry_replays_the_stored_route_after_the_default_moves() {
    let (sender, receiver, receiver_peer) = connected_pair(8).await;
    let retried = frame(None, b"hello", 6);

    let first = sender
        .send_direct(receiver_peer.clone(), retried.clone())
        .await
        .expect("the command reaches the task")
        .expect("accepted");
    assert_eq!(first, endpoint("human"));

    // The remote's default moves to `claude`. Reconfiguring discards
    // queues, so the first delivery is drained before it happens.
    let delivered = receiver
        .drain_endpoint(endpoint("human"))
        .await
        .expect("answers");
    assert_eq!(delivered.len(), 1);
    receiver
        .configure_direct(DirectEndpoints {
            default: Some(endpoint("claude")),
            ..endpoints(8)
        })
        .await
        .expect("reconfigures");

    // Reconfiguring replaced the registry, and with it the dedup cache's
    // relevance — but the cache itself survives, which is the point.
    let again = sender
        .send_direct(receiver_peer, retried)
        .await
        .expect("the command reaches the task")
        .expect("the retry is accepted");
    assert_eq!(
        again,
        endpoint("human"),
        "the stored route wins over the new default"
    );

    assert!(
        receiver
            .drain_endpoint(endpoint("claude"))
            .await
            .expect("answers")
            .is_empty(),
        "and the retry was not delivered again"
    );
    assert!(
        receiver
            .drain_endpoint(endpoint("human"))
            .await
            .expect("answers")
            .is_empty(),
        "nor to the original endpoint"
    );
}

/// Scenario 11: the same message id with a different body is refused and
/// not delivered. One identity cannot mean two messages.
#[tokio::test]
async fn the_same_id_with_a_different_body_is_refused() {
    let (sender, receiver, receiver_peer) = connected_pair(8).await;

    sender
        .send_direct(receiver_peer.clone(), frame(Some("claude"), b"first", 7))
        .await
        .expect("the command reaches the task")
        .expect("accepted");

    let error = sender
        .send_direct(receiver_peer, frame(Some("claude"), b"second", 7))
        .await
        .expect("the command reaches the task")
        .expect_err("a conflicting body is refused");
    assert_eq!(
        error,
        TransportError::ProtocolViolation,
        "a duplicate-id conflict is malformed on the wire"
    );

    let delivered = receiver
        .drain_endpoint(endpoint("claude"))
        .await
        .expect("answers");
    assert_eq!(delivered.len(), 1, "only the first body");
    assert_eq!(delivered[0].payload.bytes(), b"first");
}

/// An untrusted sender is refused, and nothing is delivered. The
/// receiver here trusts nobody.
#[tokio::test]
async fn an_untrusted_peer_is_refused_at_the_data_plane() {
    let (sender_id, sender_peer) = who();
    let (receiver_id, receiver_peer) = who();

    // The receiver trusts the sender for the CONNECTION, so the refusal
    // below is the direct data plane rather than a closed socket.
    let mut receiver = SwarmRuntime::start(
        &receiver_id,
        SubstrateConfig::default(),
        trusting(&[&sender_peer]),
    )
    .expect("starts");
    let sender = SwarmRuntime::start(
        &sender_id,
        SubstrateConfig::default(),
        trusting(&[&receiver_peer]),
    )
    .expect("starts");

    receiver
        .configure_direct(endpoints(8))
        .await
        .expect("endpoints install");
    let address = receiver
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("loopback"))
        .await
        .expect("listens");
    sender
        .dial(receiver_peer.clone(), address)
        .await
        .expect("command")
        .expect("admitted");
    wait_connected(&mut receiver).await;

    // Now revoke: trust nobody. The connection closes AND direct
    // admission moves with it.
    receiver
        .set_trust(TrustSources::default())
        .await
        .expect("revokes");

    let result = sender
        .send_direct(receiver_peer, frame(Some("claude"), b"hello", 8))
        .await
        .expect("the command reaches the task");
    // NOT MERELY `is_err()`. Revocation may surface either way — the
    // connection closes, or direct admission refuses — and which one
    // wins is a race. But naming BOTH is what stops this passing for an
    // unrelated reason: a decoder mutation made every other test in this
    // file fail while this one stayed green, because a protocol error is
    // also an error.
    let error = result.expect_err("a revoked peer is refused");
    assert!(
        matches!(
            error,
            TransportError::UnauthorizedPeer | TransportError::PeerUnreachable
        ),
        "refused BY REVOCATION, not by something else: {error:?}"
    );
    assert!(
        receiver
            .drain_endpoint(endpoint("claude"))
            .await
            .expect("answers")
            .is_empty(),
        "and nothing was delivered"
    );
}

/// A 48 KiB payload — the exact ceiling — survives the round trip.
#[tokio::test]
async fn a_payload_at_the_ceiling_survives_the_wire() {
    use interweave_transport_api::MAX_PAYLOAD_BYTES;

    let (sender, receiver, receiver_peer) = connected_pair(8).await;
    let body = vec![0xABu8; MAX_PAYLOAD_BYTES];
    let at_ceiling = DirectMessageV2 {
        message_id: MessageId::from_bytes([9; 16]),
        sent_at_ms: 1,
        source_endpoint: endpoint("human"),
        destination_endpoint: Some(endpoint("claude")),
        payload: Payload::at_ceiling(None, body.clone()).expect("exactly the ceiling"),
    };

    let resolved = sender
        .send_direct(receiver_peer, at_ceiling)
        .await
        .expect("the command reaches the task")
        .expect("the ceiling is legal");
    assert_eq!(resolved, endpoint("claude"));

    let delivered = receiver
        .drain_endpoint(endpoint("claude"))
        .await
        .expect("answers");
    assert_eq!(delivered.len(), 1);
    assert_eq!(
        delivered[0].payload.bytes().len(),
        MAX_PAYLOAD_BYTES,
        "every byte crossed the wire"
    );
    assert_eq!(delivered[0].payload.bytes(), body.as_slice());
    assert_eq!(
        delivered[0].payload.media_type(),
        None,
        "absent media stayed absent across the wire"
    );
}

/// Scenario 15: an endpoint lease disconnect removes the route
/// immediately, and takes its undelivered backlog with it.
///
/// The two are one fact. A revoke that ended the lease but left the
/// queue open would hold a daemon-side backlog for an endpoint nothing
/// holds — which `testing.md` forbids and ADR-0044 puts in the human
/// application instead.
#[tokio::test]
async fn revoking_a_lease_removes_the_route_and_discards_its_backlog() {
    let (sender, receiver, receiver_peer) = connected_pair(8).await;

    // One message lands and is left undrained.
    sender
        .send_direct(
            receiver_peer.clone(),
            frame(Some("claude"), b"undelivered", 20),
        )
        .await
        .expect("the command reaches the task")
        .expect("accepted");

    let discarded = receiver
        .revoke_endpoint(endpoint("claude"))
        .await
        .expect("the revoke reaches the task");
    assert_eq!(discarded, 1, "the undelivered event went with the lease");

    // THE ROUTE IS GONE, and indistinguishably so: an unleased endpoint
    // is `no_route` with every other routing failure.
    let error = sender
        .send_direct(receiver_peer, frame(Some("claude"), b"after", 21))
        .await
        .expect("the command reaches the task")
        .expect_err("a revoked endpoint has no route");
    assert_eq!(error, TransportError::RemoteEndpointUnavailable);

    assert!(
        receiver
            .drain_endpoint(endpoint("claude"))
            .await
            .expect("answers")
            .is_empty(),
        "and nothing survived to be drained"
    );
}

/// A draining node refuses new direct work on a connection that is
/// already open.
///
/// Draining is deliberately not stopping: `drain()` tells the connection
/// manager to admit no NEW connections, and existing ones stay up so
/// in-flight work can finish. That left a gap — a peer connected before
/// the drain could still send, and admission would enqueue the message
/// and answer `AcceptedV2` for work the following `shutdown()` discards.
///
/// `AcceptedV2` means a bounded queue took the message (ADR-0018). A
/// node about to drop that queue cannot honestly say it, so the answer is
/// `shutting_down` and the queue stays untouched.
#[tokio::test]
async fn a_draining_node_refuses_new_work_on_an_open_connection() {
    let (sender, receiver, receiver_peer) = connected_pair(8).await;

    // Before draining, the same send is accepted — so the refusal below
    // is attributable to the drain and to nothing else about this setup.
    sender
        .send_direct(receiver_peer.clone(), frame(Some("claude"), b"before", 30))
        .await
        .expect("the command reaches the task")
        .expect("accepted while serving");

    receiver.drain().await.expect("the drain reaches the task");

    let error = sender
        .send_direct(receiver_peer, frame(Some("claude"), b"during", 31))
        .await
        .expect("the command reaches the task")
        .expect_err("a draining node takes on no new work");
    assert_eq!(
        error,
        TransportError::BackendUnavailable,
        "`shutting_down` on the wire, not a routing or overload answer"
    );

    // AND NOTHING WAS ENQUEUED. The refusal is the whole point only if
    // the message did not also land in the queue on its way to being
    // refused: exactly the one accepted before the drain is there.
    let delivered = receiver
        .drain_endpoint(endpoint("claude"))
        .await
        .expect("the receiver answers");
    assert_eq!(delivered.len(), 1, "only the pre-drain message");
    assert_eq!(delivered[0].payload.bytes(), b"before");
}
