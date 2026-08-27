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

use interweave_profile_config::{
    ChannelsConfig, DirectoryConfig, EndpointConfig, EndpointsConfig, ProfileConfig,
    RegistrationPolicy, TrustConfig, TrustPolicyKind,
};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{
    DirectMessageV2, EndpointId, MediaType, MessageId, Payload, TransportError, TransportIdentity,
};
use interweave_transport_libp2p::runtime::{
    DirectEndpoints, SubstrateConfig, SubstrateError, SwarmEvent, SwarmRuntime,
};
use interweave_transport_runtime::{Generation, TrustSources};
use interweave_trust_api::EndpointTrustPolicy;
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

/// A profile carrying these endpoints, which is now the ONLY way to
/// reach `DirectEndpoints` — the runtime derives its state from the
/// canonical validated configuration rather than from a second model
/// assembled here.
fn profile_with(entries: Vec<EndpointConfig>, default: Option<&str>) -> ProfileConfig {
    ProfileConfig {
        schema_version: 2,
        trust: TrustConfig {
            policy: TrustPolicyKind::default(),
            allowed_peers: std::collections::BTreeSet::new(),
        },
        endpoints: EndpointsConfig {
            registration_policy: RegistrationPolicy::default(),
            default_direct_endpoint: default.map(endpoint),
            directory: DirectoryConfig::default(),
            entries,
        },
        channels: ChannelsConfig::default(),
    }
}

/// One endpoint entry with default policies.
fn entry(name: &str) -> EndpointConfig {
    EndpointConfig {
        id: endpoint(name),
        enabled: true,
        advertise: false,
        allowed_client_kinds: Vec::new(),
        inbound: EndpointTrustPolicy::default(),
        outbound: EndpointTrustPolicy::default(),
    }
}

fn generation() -> Generation {
    Generation::parse("stage6__________").expect("valid generation")
}

/// `human` and `claude`, with `human` the default.
fn endpoints(queue_bound: usize) -> DirectEndpoints {
    DirectEndpoints::from_profile(
        &profile_with(vec![entry("human"), entry("claude")], Some("human")),
        queue_bound,
        generation(),
    )
    .expect("a valid profile")
}

/// Like [`frame`], with the SOURCE endpoint chosen by the caller —
/// which is the thing the lease check governs.
fn frame_from(source: &str, destination: Option<&str>, body: &[u8], id: u8) -> DirectMessageV2 {
    DirectMessageV2 {
        source_endpoint: endpoint(source),
        ..frame(destination, body, id)
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

    // THE SENDER CONFIGURES ITS OWN ENDPOINTS TOO, because a source
    // endpoint must name a lease this node actually holds. Before that
    // was enforced every sender here named `human` while holding
    // nothing — which is precisely the spoofing the check now refuses.
    sender
        .configure_direct(endpoints(queue_bound))
        .await
        .expect("the sender's own endpoints install");

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
        .configure_direct(
            DirectEndpoints::from_profile(
                &profile_with(vec![entry("human"), entry("claude")], Some("claude")),
                8,
                generation(),
            )
            .expect("a valid profile"),
        )
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

    sender
        .configure_direct(endpoints(8))
        .await
        .expect("the sender's own endpoints install");

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

/// Sending to one's own PeerId is a caller error, not a network one.
///
/// `DIRECT.md`: "sending to the local profile PeerId is `InvalidArgument`;
/// self-dial never occurs." libp2p cannot hold a self-connection, so
/// without the check the caller is told `PeerUnreachable` — a network
/// verdict on a local mistake, about a peer that is right here.
#[tokio::test]
async fn sending_to_the_local_peer_is_invalid_argument() {
    let (sender, _receiver, _peer) = connected_pair(8).await;
    let me = sender.local_peer().clone();

    let error = sender
        .send_direct(me, frame(Some("claude"), b"to myself", 32))
        .await
        .expect("the command reaches the task")
        .expect_err("the local peer is not a destination");
    assert_eq!(
        error,
        TransportError::InvalidArgument,
        "a local input error, not PeerUnreachable"
    );
}

/// A source endpoint this node holds no lease for is refused locally.
///
/// The source EndpointId is derived from the local lease, never taken
/// from caller input (CLAUDE.md §5). Before this was enforced, any
/// holder of a runtime handle could name any endpoint at all, and the
/// receiver would key its dedup entry on that label and surface it on
/// the delivered event as though it meant something.
///
/// Refused before the frame reaches the swarm, so nothing crosses the
/// socket and no dedup entry is minted anywhere.
#[tokio::test]
async fn a_source_endpoint_without_a_lease_is_refused() {
    let (sender, receiver, receiver_peer) = connected_pair(8).await;

    let error = sender
        .send_direct(
            receiver_peer.clone(),
            frame_from("not-leased", Some("claude"), b"spoofed", 40),
        )
        .await
        .expect("the command reaches the task")
        .expect_err("a name this node never configured holds no lease");
    assert_eq!(error, TransportError::EndpointNotRegistered);

    // NOTHING CROSSED THE WIRE. A refusal that still sent the frame
    // would leave the receiver holding the spoofed label, which is the
    // defect rather than the fix.
    assert!(
        receiver
            .drain_endpoint(endpoint("claude"))
            .await
            .expect("answers")
            .is_empty(),
        "the receiver never saw it"
    );

    // ...and a source this node DOES hold is unaffected, so the check
    // discriminates rather than refusing everything.
    sender
        .send_direct(
            receiver_peer,
            frame_from("human", Some("claude"), b"real", 41),
        )
        .await
        .expect("the command reaches the task")
        .expect("a leased source endpoint still sends");
    assert_eq!(
        receiver
            .drain_endpoint(endpoint("claude"))
            .await
            .expect("answers")
            .len(),
        1
    );
}

/// Revoking the lease stops the endpoint being a usable SOURCE, not just
/// a destination.
///
/// The registry is one fact read two ways: `revoke_endpoint` ends the
/// lease, and a lease is exactly what the send path requires. Without
/// this a revoked endpoint would go on sending under its own name while
/// refusing to receive under it.
#[tokio::test]
async fn a_revoked_endpoint_can_no_longer_be_a_source() {
    let (sender, _receiver, receiver_peer) = connected_pair(8).await;

    sender
        .revoke_endpoint(endpoint("human"))
        .await
        .expect("the revoke reaches the task");

    let error = sender
        .send_direct(
            receiver_peer,
            frame_from("human", Some("claude"), b"after revoke", 42),
        )
        .await
        .expect("the command reaches the task")
        .expect_err("the lease is gone");
    assert_eq!(error, TransportError::EndpointNotRegistered);
}

/// Revoking a peer's trust stops direct sends immediately, not once the
/// connection finishes closing.
///
/// `set_trust` closes the revoked peer's connections, but the close is
/// asynchronous: until the event arrives the connection is open and
/// `is_connected` still says yes. A send queued in that window used to
/// cross a connection that had already lost data-plane authorization —
/// exactly what the revocation was for. Trust is now re-read from the
/// manager per command rather than inherited from the connection.
///
/// The send goes out on the very next line after `set_trust` returns, so
/// nothing here waits for the close; that is the race, and waiting would
/// test the wrong thing.
#[tokio::test]
async fn revoking_trust_stops_direct_sends_before_the_close_lands() {
    let (sender, receiver, receiver_peer) = connected_pair(8).await;

    // It works while trusted, so the refusal below is the revocation and
    // not some other property of this setup.
    sender
        .send_direct(receiver_peer.clone(), frame(Some("claude"), b"trusted", 50))
        .await
        .expect("the command reaches the task")
        .expect("accepted while trusted");

    // Trust nobody. The connection is still open at this instant.
    sender
        .set_trust(TrustSources::new(
            PeerTrustPolicy::new(std::iter::empty()).expect("an empty allowlist"),
            InfrastructureSet::default(),
        ))
        .await
        .expect("the trust update reaches the task");

    let error = sender
        .send_direct(receiver_peer, frame(Some("claude"), b"revoked", 51))
        .await
        .expect("the command reaches the task")
        .expect_err("an untrusted peer is not a direct destination");
    assert_eq!(error, TransportError::UnauthorizedPeer);

    // AND IT NEVER CROSSED. A refusal that still sent the frame would
    // have delivered it, which is the defect rather than the fix.
    let delivered = receiver
        .drain_endpoint(endpoint("claude"))
        .await
        .expect("the receiver answers");
    assert_eq!(delivered.len(), 1, "only the pre-revocation message");
    assert_eq!(delivered[0].payload.bytes(), b"trusted");
}

/// A configuration the canonical validator refuses never becomes state.
///
/// The endpoint-count ceiling used to be re-checked in
/// `configure_direct`, which is the pattern that caused the endpoint
/// policy defects: every rule restated here is a rule that can drift
/// from `ProfileConfig::validate`. It is not restated any more — this
/// asserts the validator's own verdict arrives.
#[tokio::test]
async fn a_configuration_the_validator_refuses_never_becomes_state() {
    let too_many: Vec<EndpointConfig> = (0..=interweave_profile_config::MAX_ENDPOINTS)
        .map(|i| entry(&format!("endpoint-{i}")))
        .collect();
    assert_eq!(too_many.len(), interweave_profile_config::MAX_ENDPOINTS + 1);

    let error = DirectEndpoints::from_profile(&profile_with(too_many, None), 8, generation())
        .expect_err("one past the ceiling is one too many");
    assert!(
        matches!(error, SubstrateError::InvalidProfile(_)),
        "refused by the profile validator, got {error:?}"
    );

    // EXACTLY AT THE CEILING IS ALLOWED, so this is a ceiling and not an
    // off-by-one that also refuses the largest legal configuration.
    let exactly: Vec<EndpointConfig> = (0..interweave_profile_config::MAX_ENDPOINTS)
        .map(|i| entry(&format!("endpoint-{i}")))
        .collect();
    DirectEndpoints::from_profile(&profile_with(exactly, None), 8, generation())
        .expect("the ceiling itself is permitted");
}

/// A duplicate id, an absent default and a disabled default are all
/// refused, and none of those rules is written in the direct layer.
///
/// This is the whole point of deriving: the runtime inherits every rule
/// the canonical configuration already enforces, including ones nobody
/// remembered to restate.
#[tokio::test]
async fn the_canonical_rules_are_inherited_rather_than_restated() {
    let duplicated = profile_with(vec![entry("human"), entry("human")], Some("human"));
    assert!(
        matches!(
            DirectEndpoints::from_profile(&duplicated, 8, generation()),
            Err(SubstrateError::InvalidProfile(_))
        ),
        "a duplicate endpoint id is refused"
    );

    let absent = profile_with(vec![entry("human")], Some("claude"));
    assert!(
        matches!(
            DirectEndpoints::from_profile(&absent, 8, generation()),
            Err(SubstrateError::InvalidProfile(_))
        ),
        "a default naming an endpoint that does not exist is refused"
    );

    let mut off = entry("human");
    off.enabled = false;
    let disabled = profile_with(vec![off], Some("human"));
    assert!(
        matches!(
            DirectEndpoints::from_profile(&disabled, 8, generation()),
            Err(SubstrateError::InvalidProfile(_))
        ),
        "a default naming a disabled endpoint is refused"
    );
}

/// A queue depth outside `1..=MAX_EVENT_QUEUE` is refused.
///
/// `EndpointQueues::open` raises a zero to one and lowered nothing, so a
/// caller asking for a million got a million — memory a remote peer then
/// fills, bounded only by its rate allowance. Refused rather than
/// clamped: clamping installs a configuration the caller never asked for
/// and never learns about.
///
/// This one IS a property of the runtime rather than of the profile, so
/// it lives beside the derivation instead of in the validator.
#[tokio::test]
async fn a_queue_depth_outside_its_range_is_refused() {
    let profile = profile_with(vec![entry("human")], None);

    for bad in [0, interweave_local_client_api::MAX_EVENT_QUEUE + 1] {
        let error = DirectEndpoints::from_profile(&profile, bad, generation())
            .expect_err("outside the permitted range");
        assert!(
            matches!(
                error,
                SubstrateError::InvalidConfig {
                    field: "direct.queue_bound",
                    ..
                }
            ),
            "refused as a configuration error naming the field, got {error:?}"
        );
    }

    // Both ends of the range itself are permitted.
    for good in [1, interweave_local_client_api::MAX_EVENT_QUEUE] {
        DirectEndpoints::from_profile(&profile, good, generation()).expect("inside the range");
    }
}

/// A full user-event channel cannot freeze a direct exchange.
///
/// The Swarm branch is polled only when the outbox has room, and the
/// slack for that used to count pending LISTENERS and nothing else. A
/// dispatched direct request is answered only by Swarm progress, so a
/// consumer that stops draining events would stop the polling that
/// settles its own request — and `send_direct` would wait past the
/// contract's deadline with nothing able to resolve it. A remote peer
/// can drive that state alone: every accepted delivery appends a
/// `DirectDelivered`.
///
/// The sender here never calls `next_event`, so its outbox fills and
/// stays full.
#[tokio::test]
async fn a_full_event_channel_does_not_freeze_a_direct_exchange() {
    let (sender_id, sender_peer) = who();
    let (receiver_id, receiver_peer) = who();

    // ONE EVENT OF CAPACITY. The connection alone fills it.
    let cramped = SubstrateConfig {
        event_capacity: 1,
        ..SubstrateConfig::default()
    };

    let mut receiver = SwarmRuntime::start(
        &receiver_id,
        SubstrateConfig::default(),
        trusting(&[&sender_peer]),
    )
    .expect("starts");
    let sender =
        SwarmRuntime::start(&sender_id, cramped, trusting(&[&receiver_peer])).expect("starts");

    sender
        .configure_direct(endpoints(8))
        .await
        .expect("the sender's own endpoints install");
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

    // Bounded: without the fix this never returns, and a test that hangs
    // reports nothing at all.
    let answered = tokio::time::timeout(
        Duration::from_secs(20),
        sender.send_direct(
            receiver_peer,
            frame(Some("claude"), b"through a full outbox", 60),
        ),
    )
    .await
    .expect("the exchange settled rather than hanging on a full event channel");

    assert_eq!(
        answered
            .expect("the command reaches the task")
            .expect("accepted"),
        endpoint("claude")
    );
}

/// A draining node starts no NEW outbound exchange either.
///
/// Inbound already refuses after `drain()`. Dispatching fresh local work
/// in the same window contradicts the same contract from the other side:
/// the node has announced it is going out of service and then sends
/// something whose answer it may not be around to receive.
///
/// `ShuttingDown` and not `BackendUnavailable`: the refusal is local and
/// nothing crossed a network boundary, so the caller should not read it
/// as anything the remote said.
#[tokio::test]
async fn a_draining_node_starts_no_new_outbound_exchange() {
    let (sender, receiver, receiver_peer) = connected_pair(8).await;

    sender
        .send_direct(receiver_peer.clone(), frame(Some("claude"), b"before", 70))
        .await
        .expect("the command reaches the task")
        .expect("accepted while serving");

    sender.drain().await.expect("the drain reaches the task");

    let error = sender
        .send_direct(receiver_peer, frame(Some("claude"), b"during", 71))
        .await
        .expect("the command reaches the task")
        .expect_err("a draining node takes on no new work");
    assert_eq!(error, TransportError::ShuttingDown);

    // AND IT NEVER CROSSED — only the pre-drain message is there.
    let delivered = receiver
        .drain_endpoint(endpoint("claude"))
        .await
        .expect("the receiver answers");
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].payload.bytes(), b"before");
}

/// A profile's effective payload limit binds below the frozen ceiling.
///
/// The wire ceiling is 49,152 bytes, but a profile may configure less.
/// Every frame used to be decoded against the architecture maximum, so a
/// payload above the profile's limit and below the ceiling was accepted
/// and queued — the limit existed in the contract and reached no
/// decoder.
///
/// Both directions: the sender refuses locally, and a receiver
/// configured lower refuses on the wire with `too_large`.
#[tokio::test]
async fn a_profile_payload_limit_binds_below_the_ceiling() {
    let (sender_id, sender_peer) = who();
    let (receiver_id, receiver_peer) = who();

    let narrow = SubstrateConfig {
        max_payload_bytes: 1_024,
        ..SubstrateConfig::default()
    };

    let mut receiver =
        SwarmRuntime::start(&receiver_id, narrow, trusting(&[&sender_peer])).expect("starts");
    // The SENDER keeps the full ceiling, so the refusal below is the
    // receiver's configuration and not the sender declining to send.
    let sender = SwarmRuntime::start(
        &sender_id,
        SubstrateConfig::default(),
        trusting(&[&receiver_peer]),
    )
    .expect("starts");

    sender
        .configure_direct(endpoints(8))
        .await
        .expect("the sender's endpoints install");
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

    let body = vec![b'x'; 2_048];
    let oversized = DirectMessageV2 {
        message_id: MessageId::from_bytes([80; 16]),
        sent_at_ms: 1,
        source_endpoint: endpoint("human"),
        destination_endpoint: Some(endpoint("claude")),
        payload: Payload::at_ceiling(None, body).expect("under the FROZEN ceiling"),
    };

    let error = sender
        .send_direct(receiver_peer.clone(), oversized)
        .await
        .expect("the command reaches the task")
        .expect_err("above the receiver's configured limit");
    assert_eq!(error, TransportError::PayloadTooLarge);
    assert!(
        receiver
            .drain_endpoint(endpoint("claude"))
            .await
            .expect("answers")
            .is_empty(),
        "and nothing was queued"
    );

    // UNDER the limit still works, so this is a limit and not a wall.
    sender
        .send_direct(
            receiver_peer,
            DirectMessageV2 {
                message_id: MessageId::from_bytes([81; 16]),
                sent_at_ms: 1,
                source_endpoint: endpoint("human"),
                destination_endpoint: Some(endpoint("claude")),
                payload: Payload::at_ceiling(None, vec![b'x'; 512]).expect("well under"),
            },
        )
        .await
        .expect("command")
        .expect("under the configured limit");
    assert_eq!(
        receiver
            .drain_endpoint(endpoint("claude"))
            .await
            .expect("answers")
            .len(),
        1
    );
}

/// The sender's own limit refuses before anything crosses the wire.
///
/// The other direction of the same configuration. A profile that refuses
/// a payload on the way in has configured nothing if it sends the same
/// payload out, and the refusal must be LOCAL — no frame, no exchange.
#[tokio::test]
async fn a_narrow_sender_refuses_its_own_oversized_payload() {
    let (sender_id, sender_peer) = who();
    let (receiver_id, receiver_peer) = who();

    let narrow = SubstrateConfig {
        max_payload_bytes: 1_024,
        ..SubstrateConfig::default()
    };
    let mut receiver = SwarmRuntime::start(
        &receiver_id,
        SubstrateConfig::default(),
        trusting(&[&sender_peer]),
    )
    .expect("starts");
    let sender =
        SwarmRuntime::start(&sender_id, narrow, trusting(&[&receiver_peer])).expect("starts");

    sender
        .configure_direct(endpoints(8))
        .await
        .expect("the sender's endpoints install");
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

    let error = sender
        .send_direct(
            receiver_peer,
            DirectMessageV2 {
                message_id: MessageId::from_bytes([82; 16]),
                sent_at_ms: 1,
                source_endpoint: endpoint("human"),
                destination_endpoint: Some(endpoint("claude")),
                payload: Payload::at_ceiling(None, vec![b'x'; 2_048])
                    .expect("under the FROZEN ceiling"),
            },
        )
        .await
        .expect("the command reaches the task")
        .expect_err("above this profile's own limit");
    assert_eq!(error, TransportError::PayloadTooLarge);

    // THE RECEIVER, which is configured wide, saw nothing — so the
    // refusal was local rather than the remote's answer.
    assert!(
        receiver
            .drain_endpoint(endpoint("claude"))
            .await
            .expect("answers")
            .is_empty(),
        "nothing crossed the wire"
    );
}

/// The source endpoint's outbound policy narrows a send the PROFILE
/// still permits.
///
/// `EndpointRegistry::authorize_outbound` applies profile trust first
/// and the endpoint's outbound subset second, and it had no production
/// caller at all — the narrowing existed in the domain layer and bound
/// nothing, so an endpoint configured to reach only some peers could
/// reach any profile-trusted one.
///
/// The peer here IS profile-trusted and the connection IS available, so
/// a refusal can only be the endpoint's own policy — which the send
/// from an INHERITING endpoint at the end confirms, since it reaches the
/// same peer over the same connection.
///
/// `UnauthorizedPeer` because `ENDPOINTS.md` outbound step 3 requires
/// it. The code alone cannot distinguish this from a profile denial, and
/// deliberately: the contract does not offer the sender a finer answer.
#[tokio::test]
async fn the_source_endpoints_outbound_policy_narrows_a_trusted_peer() {
    let (sender_id, sender_peer) = who();
    let (receiver_id, receiver_peer) = who();

    // `human` may reach nobody; `claude` inherits profile trust. Both
    // are leased by the sender, so the lease check cannot be what
    // refuses below.
    let mut narrowed = entry("human");
    narrowed.outbound = EndpointTrustPolicy::StaticSubset {
        allowed_peers: std::collections::BTreeSet::new(),
    };
    let sender_profile = profile_with(vec![narrowed, entry("claude")], Some("claude"));

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

    sender
        .configure_direct(
            DirectEndpoints::from_profile(&sender_profile, 8, generation())
                .expect("a valid profile"),
        )
        .await
        .expect("the sender's endpoints install");
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

    let error = sender
        .send_direct(
            receiver_peer.clone(),
            frame_from("human", Some("claude"), b"narrowed out", 90),
        )
        .await
        .expect("the command reaches the task")
        .expect_err("the endpoint's outbound subset excludes this peer");
    assert_eq!(
        error,
        TransportError::UnauthorizedPeer,
        "ENDPOINTS.md outbound step 3: narrowing denial is UnauthorizedPeer"
    );

    // THE SAME PEER, FROM AN ENDPOINT THAT INHERITS, still sends — so
    // the refusal was narrowing and not a broken connection.
    sender
        .send_direct(
            receiver_peer,
            frame_from("claude", Some("claude"), b"permitted", 91),
        )
        .await
        .expect("command")
        .expect("an inheriting endpoint reaches a profile-trusted peer");

    let delivered = receiver
        .drain_endpoint(endpoint("claude"))
        .await
        .expect("answers");
    assert_eq!(delivered.len(), 1, "only the permitted one arrived");
    assert_eq!(delivered[0].payload.bytes(), b"permitted");
}

/// A destination endpoint's INBOUND policy refuses a profile-trusted
/// sender, coarsely.
///
/// `DirectEndpoints` used to carry bare ids and `configure` filled in
/// `RegisteredEndpoint::default()`, whose inbound policy inherits
/// profile trust — so a configured narrowing was discarded on the way in
/// and the excluded peer was accepted and queued.
///
/// The wire answer must be `no_route`, indistinguishable from unknown,
/// disabled and offline: telling a peer "that endpoint exists but
/// refuses you" is the endpoint-presence oracle the collapse exists to
/// prevent.
#[tokio::test]
async fn a_destination_endpoints_inbound_policy_is_coarse_no_route() {
    let (sender_id, sender_peer) = who();
    let (receiver_id, receiver_peer) = who();

    // `claude` admits nobody; `human` inherits. The sender is
    // profile-trusted either way.
    let mut narrowed = entry("claude");
    narrowed.inbound = EndpointTrustPolicy::StaticSubset {
        allowed_peers: std::collections::BTreeSet::new(),
    };
    let receiver_profile = profile_with(vec![entry("human"), narrowed], Some("human"));

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

    sender
        .configure_direct(endpoints(8))
        .await
        .expect("the sender's endpoints install");
    receiver
        .configure_direct(
            DirectEndpoints::from_profile(&receiver_profile, 8, generation())
                .expect("a valid profile"),
        )
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

    let error = sender
        .send_direct(
            receiver_peer.clone(),
            frame(Some("claude"), b"excluded", 92),
        )
        .await
        .expect("the command reaches the task")
        .expect_err("the endpoint's inbound policy excludes this peer");
    assert_eq!(
        error,
        TransportError::RemoteEndpointUnavailable,
        "coarse no_route — the SAME answer an unknown endpoint gives"
    );

    // NOTHING WAS QUEUED. A policy that refused on the wire and enqueued
    // anyway would be the defect wearing a different answer.
    assert!(
        receiver
            .drain_endpoint(endpoint("claude"))
            .await
            .expect("answers")
            .is_empty(),
        "the excluded message reached no queue"
    );

    // INDISTINGUISHABLE FROM ABSENT, which is what makes it coarse: an
    // endpoint that does not exist answers exactly the same way.
    let unknown = sender
        .send_direct(receiver_peer.clone(), frame(Some("nonexistent"), b"x", 93))
        .await
        .expect("command")
        .expect_err("no such endpoint");
    assert_eq!(unknown, error, "policy denial and absence are one answer");

    // ...and the endpoint that inherits still accepts, so the receiver
    // is not simply refusing everything.
    sender
        .send_direct(receiver_peer, frame(Some("human"), b"welcome", 94))
        .await
        .expect("command")
        .expect("an inheriting endpoint admits a profile-trusted peer");
    assert_eq!(
        receiver
            .drain_endpoint(endpoint("human"))
            .await
            .expect("answers")
            .len(),
        1
    );
}

/// An endpoint restricted to a real client kind still works.
///
/// The synthetic in-process lease used a hard-coded `in-process` kind.
/// That passed while `DirectEndpoints` rebuilt every endpoint from
/// `RegisteredEndpoint::default()`, whose kind list is empty and
/// restricts nothing. Once the real profile reached the registry, any
/// endpoint restricted to `human-client` or `claude-channel` — which the
/// example profiles are — refused the claim outright.
///
/// The refusal was then SWALLOWED by an `.is_ok()`: no lease, no queue,
/// and `configure_direct` still reported success. Every send from that
/// endpoint answered `EndpointNotRegistered` and every message to it
/// `no_route`, for a configuration the caller was told had installed.
#[tokio::test]
async fn an_endpoint_restricted_to_a_client_kind_still_leases() {
    let restricted = |name: &str, kind: &str| {
        let mut e = entry(name);
        e.allowed_client_kinds =
            vec![interweave_profile_config::ClientKind::parse(kind).expect("a valid client kind")];
        e
    };
    let profile = profile_with(
        vec![
            restricted("human", "human-client"),
            restricted("claude", "claude-channel"),
        ],
        Some("human"),
    );

    let (sender_id, sender_peer) = who();
    let (receiver_id, receiver_peer) = who();
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

    let installed =
        DirectEndpoints::from_profile(&profile, 8, generation()).expect("a valid profile");
    sender
        .configure_direct(installed.clone())
        .await
        .expect("the sender's restricted endpoints install");
    receiver
        .configure_direct(installed)
        .await
        .expect("the receiver's restricted endpoints install");

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

    // A LEASE EXISTS ON BOTH SIDES, which is the whole claim. The send
    // needs the sender's lease and the delivery needs the receiver's, so
    // one message proves both.
    let resolved = sender
        .send_direct(receiver_peer, frame(Some("claude"), b"restricted", 95))
        .await
        .expect("the command reaches the task")
        .expect("a restricted endpoint is still a leased endpoint");
    assert_eq!(resolved, endpoint("claude"));

    let delivered = receiver
        .drain_endpoint(endpoint("claude"))
        .await
        .expect("answers");
    assert_eq!(delivered.len(), 1, "and its queue was opened");
    assert_eq!(delivered[0].payload.bytes(), b"restricted");
}

/// Nothing to dial and could-not-reach are different answers.
///
/// `DIRECT.md` separates them: no usable candidate addresses is
/// `PeerUnknown`, without ad hoc discovery; a peer with candidates that
/// is simply not connected is `PeerUnreachable`. Both used to be
/// `PeerUnreachable`, on a comment claiming this layer "knows only that
/// there is no connection" — while the ConnectionManager, in scope at
/// that call, knows whether any address was ever recorded.
///
/// The distinction is what an operator acts on: nothing to dial is a
/// configuration or discovery problem, something that did not answer is
/// a network one.
#[tokio::test]
async fn an_unknown_peer_and_an_unreachable_one_are_told_apart() {
    let (sender_id, _sender_peer) = who();
    let (_a_id, never_seen) = who();
    let (_b_id, has_an_address) = who();

    let sender = SwarmRuntime::start(
        &sender_id,
        SubstrateConfig::default(),
        trusting(&[&never_seen, &has_an_address]),
    )
    .expect("starts");
    sender
        .configure_direct(endpoints(8))
        .await
        .expect("endpoints install");

    // NEITHER IS CONNECTED. The only difference is whether the manager
    // holds an address for it.
    sender
        .add_address(
            has_an_address.clone(),
            "/ip4/127.0.0.1/tcp/1".parse().expect("a loopback address"),
        )
        .await
        .expect("the command reaches the task");

    let unknown = sender
        .send_direct(never_seen, frame(Some("claude"), b"nowhere", 96))
        .await
        .expect("the command reaches the task")
        .expect_err("no candidate addresses");
    assert_eq!(
        unknown,
        TransportError::PeerUnknown,
        "nothing to dial is a configuration answer, not a network one"
    );

    let unreachable = sender
        .send_direct(has_an_address, frame(Some("claude"), b"somewhere", 97))
        .await
        .expect("the command reaches the task")
        .expect_err("an address, but no connection");
    assert_eq!(
        unreachable,
        TransportError::PeerUnreachable,
        "something to dial that is not connected is a network answer"
    );
}
