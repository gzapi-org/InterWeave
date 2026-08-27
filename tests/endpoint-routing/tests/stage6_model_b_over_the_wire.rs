// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Model B over the wire: one PeerId, several local endpoints.
//!
//! ADR-0030's central claim, and the reason this suite is separate from
//! `tests/direct-v2`: that one asks whether the wire is correct, this
//! asks whether a message addressed to `human` reaches ONLY `human` on a
//! profile where `claude` holds a lease on the same PeerId. A failure
//! here is a registry or routing defect, and a reader should not have to
//! tell the two apart by reading the assertion.
//!
//! Run against real peers because the property is about what crosses a
//! socket. A mock delivers wherever the test told it to.
#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_profile_config::{
    ChannelsConfig, DirectoryConfig, EndpointConfig, EndpointsConfig, ProfileConfig,
    RegistrationPolicy, TrustConfig, TrustPolicyKind,
};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{
    DirectMessageV2, EndpointId, MediaType, MessageId, Payload, TransportIdentity,
};
use interweave_transport_libp2p::runtime::{
    DirectEndpoints, SubstrateConfig, SwarmEvent, SwarmRuntime,
};
use interweave_transport_runtime::{Generation, TrustSources};
use interweave_trust_api::EndpointTrustPolicy;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};

fn who() -> (ProfileIdentity, TransportIdentity) {
    let id = ProfileIdentity::generate();
    let peer = id.transport_identity().expect("peer id");
    (id, peer)
}

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

/// ONE PROFILE, SEVERAL ENDPOINTS. `human` is the default; `claude` and
/// a third name that does not exist yet share the same PeerId, which is
/// the arrangement Model B describes.
fn endpoints() -> DirectEndpoints {
    DirectEndpoints::from_profile(
        &profile_with(
            vec![entry("human"), entry("claude"), entry("gpt-5")],
            Some("human"),
        ),
        8,
        Generation::parse("modelb__________").expect("valid generation"),
    )
    .expect("a valid profile")
}

/// A frame from `from`, to `to`, carrying `body` under `id`.
fn frame(from: &str, to: Option<&str>, body: &[u8], id: u8) -> DirectMessageV2 {
    DirectMessageV2 {
        message_id: MessageId::from_bytes([id; 16]),
        sent_at_ms: 1,
        source_endpoint: endpoint(from),
        destination_endpoint: to.map(endpoint),
        payload: Payload::at_ceiling(
            Some(MediaType::parse("text/plain").expect("valid media type")),
            body.to_vec(),
        )
        .expect("within the ceiling"),
    }
}

async fn connected_pair() -> (SwarmRuntime, SwarmRuntime, TransportIdentity) {
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

    // The sender holds leases for the endpoints it sends FROM: a source
    // endpoint must name a lease this node holds, not a label it chose.
    sender
        .configure_direct(endpoints())
        .await
        .expect("the sender's own endpoints install");

    receiver
        .configure_direct(endpoints())
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

    (sender, receiver, receiver_peer)
}

/// Bounded: a connection that never arrives is a RESULT, and a test that
/// waits forever reports nothing at all.
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

/// Scenarios 2 and 3: a send to `human` reaches only human, a send to
/// `claude` reaches only Claude — on ONE PeerId.
#[tokio::test]
async fn each_endpoint_receives_only_what_was_addressed_to_it() {
    let (sender, receiver, peer) = connected_pair().await;

    sender
        .send_direct(
            peer.clone(),
            frame("human", Some("human"), b"for the human", 1),
        )
        .await
        .expect("command")
        .expect("accepted");
    sender
        .send_direct(
            peer.clone(),
            frame("human", Some("claude"), b"for claude", 2),
        )
        .await
        .expect("command")
        .expect("accepted");
    sender
        .send_direct(peer, frame("human", Some("gpt-5"), b"for gpt-5", 3))
        .await
        .expect("command")
        .expect("accepted");

    for (name, expected) in [
        ("human", b"for the human".as_slice()),
        ("claude", b"for claude"),
        ("gpt-5", b"for gpt-5"),
    ] {
        let delivered = receiver
            .drain_endpoint(endpoint(name))
            .await
            .expect("answers");
        assert_eq!(delivered.len(), 1, "`{name}` received exactly one message");
        assert_eq!(
            delivered[0].payload.bytes(),
            expected,
            "`{name}` received the one addressed to it"
        );
        assert_eq!(delivered[0].destination_endpoint, endpoint(name));
    }
}

/// `gpt-5` is not a special case. Model B's endpoints are configured
/// labels, so a model nobody had heard of when this stage was written
/// routes exactly as `human` and `claude` do — no code knows the names.
#[tokio::test]
async fn an_endpoint_this_stage_never_heard_of_routes_like_any_other() {
    let (sender, receiver, peer) = connected_pair().await;

    let resolved = sender
        .send_direct(peer, frame("human", Some("gpt-5"), b"hello", 4))
        .await
        .expect("command")
        .expect("a configured endpoint is a configured endpoint");
    assert_eq!(resolved, endpoint("gpt-5"));

    assert_eq!(
        receiver
            .drain_endpoint(endpoint("gpt-5"))
            .await
            .expect("answers")
            .len(),
        1
    );
    for other in ["human", "claude"] {
        assert!(
            receiver
                .drain_endpoint(endpoint(other))
                .await
                .expect("answers")
                .is_empty(),
            "`{other}` received nothing"
        );
    }
}

/// Scenario 13: one message id from one peer, but two SOURCE endpoints,
/// produces independent deliveries.
///
/// The source endpoint is a dedup dimension, so two clients on one
/// profile may each use the same idempotency key without one silencing
/// the other. Collapsing the key to `(peer, message_id)` would make the
/// second arrival a duplicate of the first and drop it.
#[tokio::test]
async fn one_id_from_two_source_endpoints_delivers_twice() {
    let (sender, receiver, peer) = connected_pair().await;

    sender
        .send_direct(
            peer.clone(),
            frame("human", Some("claude"), b"from human", 5),
        )
        .await
        .expect("command")
        .expect("accepted");
    sender
        .send_direct(peer, frame("gpt-5", Some("claude"), b"from gpt-5", 5))
        .await
        .expect("command")
        .expect("the same id from a different source is not a duplicate");

    let delivered = receiver
        .drain_endpoint(endpoint("claude"))
        .await
        .expect("answers");
    assert_eq!(delivered.len(), 2, "both were delivered");

    let sources: Vec<&str> = delivered
        .iter()
        .map(|e| e.source_endpoint.as_str())
        .collect();
    assert!(sources.contains(&"human"), "the human's copy arrived");
    assert!(sources.contains(&"gpt-5"), "and the other one did too");
    assert_eq!(
        delivered[0].message_id, delivered[1].message_id,
        "under one shared id"
    );
}

/// The same id from the SAME source is a duplicate, which is the other
/// half of the claim above — without it "two deliveries" could simply
/// mean dedup is not working at all.
#[tokio::test]
async fn one_id_from_one_source_delivers_once() {
    let (sender, receiver, peer) = connected_pair().await;
    let repeated = frame("human", Some("claude"), b"same body", 6);

    sender
        .send_direct(peer.clone(), repeated.clone())
        .await
        .expect("command")
        .expect("accepted");
    sender
        .send_direct(peer, repeated)
        .await
        .expect("command")
        .expect("the retry is accepted from cache");

    assert_eq!(
        receiver
            .drain_endpoint(endpoint("claude"))
            .await
            .expect("answers")
            .len(),
        1,
        "one enqueue, however many times it is sent"
    );
}
