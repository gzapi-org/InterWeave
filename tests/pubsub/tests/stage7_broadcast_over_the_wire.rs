// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Stage 7 exit gate: signed GossipSub between real peers.
//!
//! Two or more `SwarmRuntime`s, a real TCP transport, a real Noise
//! handshake, real message signing and the real backend mesh. Nothing is
//! mocked, for the reason the direct suite gives: every property here is
//! one a mock grants for free. "The unauthorized publisher was not
//! forwarded" is a statement about what a third peer did or did not
//! receive, and a stub forwards exactly what the test told it to.
#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_profile_config::{
    ChannelsConfig, DirectoryConfig, EndpointConfig, EndpointsConfig, ProfileConfig,
    RegistrationPolicy, TrustConfig, TrustPolicyKind,
};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, EndpointId, MediaType, MessageId, Payload, TransportError,
    TransportIdentity,
};
use interweave_transport_libp2p::runtime::{
    BroadcastChannels, DirectEndpoints, SubstrateConfig, SwarmEvent, SwarmRuntime,
};
use interweave_transport_runtime::{Generation, TrustSources};
use interweave_trust_api::{EndpointTrustPolicy, InfrastructureSet, PeerTrustPolicy};

/// Generous, because a mesh forms over several heartbeats rather than in
/// one round trip. A failure here is a real failure, not a slow machine.
const PATIENCE: Duration = Duration::from_secs(20);

/// How long to wait before concluding something did NOT arrive.
///
/// A negative assertion cannot be proven by waiting forever, so it is
/// bounded deliberately and generously: long enough that a message which
/// was going to be delivered would have been.
const SILENCE: Duration = Duration::from_secs(3);

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

fn channel(name: &str) -> ChannelId {
    ChannelId::parse(name).expect("valid channel id")
}

fn endpoint(name: &str) -> EndpointId {
    EndpointId::parse(name).expect("valid endpoint id")
}

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

fn profile(desired: &[&str]) -> ProfileConfig {
    ProfileConfig {
        schema_version: 2,
        trust: TrustConfig {
            policy: TrustPolicyKind::default(),
            allowed_peers: std::collections::BTreeSet::new(),
        },
        endpoints: EndpointsConfig {
            registration_policy: RegistrationPolicy::default(),
            default_direct_endpoint: Some(endpoint("human")),
            directory: DirectoryConfig::default(),
            entries: vec![entry("human")],
        },
        channels: ChannelsConfig {
            desired: desired.iter().map(|c| channel(c)).collect(),
        },
    }
}

fn envelope(id: u8, body: &[u8]) -> BroadcastMessageV1 {
    BroadcastMessageV1 {
        message_id: MessageId::from_bytes([id; 16]),
        sent_at_ms: 1_786_600_000_000,
        payload: Payload::at_ceiling(
            Some(MediaType::parse("text/plain").expect("valid media type")),
            body.to_vec(),
        )
        .expect("within the ceiling"),
    }
}

/// Start a runtime that trusts `peers`, with both modes configured.
async fn node(
    identity: &ProfileIdentity,
    peers: &[&TransportIdentity],
    desired: &[&str],
    config: SubstrateConfig,
) -> SwarmRuntime {
    let runtime = SwarmRuntime::start(identity, config, trusting(peers)).expect("the node starts");
    runtime
        .configure_direct(
            DirectEndpoints::from_profile(
                &profile(desired),
                64,
                Generation::parse("stage7__________").expect("valid generation"),
            )
            .expect("a valid profile"),
        )
        .await
        .expect("direct endpoints install");
    runtime
        .configure_broadcast(
            BroadcastChannels::from_profile(&profile(desired), 64).expect("a valid profile"),
        )
        .await
        .expect("broadcast channels install");
    runtime
}

/// Connect `dialer` to `listener` and wait for both to see it.
async fn connect(dialer: &mut SwarmRuntime, listener: &mut SwarmRuntime, to: &TransportIdentity) {
    let address = listener
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("a loopback address"))
        .await
        .expect("the listener listens");
    dialer
        .dial(to.clone(), address)
        .await
        .expect("the command reaches the task")
        .expect("the dial is admitted");

    for side in [&mut *dialer, &mut *listener] {
        wait_for(side, "an authenticated connection", |e| {
            matches!(e, SwarmEvent::Connected { .. })
        })
        .await;
    }
}

async fn wait_for<F>(runtime: &mut SwarmRuntime, what: &str, mut predicate: F) -> SwarmEvent
where
    F: FnMut(&SwarmEvent) -> bool,
{
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, runtime.next_event()).await {
            Ok(Some(event)) if predicate(&event) => return event,
            Ok(Some(_)) => {}
            Ok(None) => panic!("the event stream ended while waiting for {what}"),
            Err(_) => panic!("timed out waiting for {what}"),
        }
    }
}

/// Drain `session` until something arrives, or conclude nothing will.
///
/// Polls rather than sleeping once, so a delivery that is merely slow is
/// not read as an absence.
async fn drain_until(
    runtime: &SwarmRuntime,
    session: &str,
    patience: Duration,
) -> Vec<interweave_transport_runtime::session_queue::BroadcastEvent> {
    let deadline = tokio::time::Instant::now() + patience;
    loop {
        let held = runtime
            .drain_session(session)
            .await
            .expect("the task answers");
        if !held.is_empty() || tokio::time::Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// THE EXIT GATE. Broadcast and direct are independently functional on
/// one pair of peers, and neither substitutes for the other.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn broadcast_and_direct_are_independently_functional() {
    let (a_id, a_peer) = who();
    let (b_id, b_peer) = who();

    let mut a = node(&a_id, &[&b_peer], &["general"], SubstrateConfig::default()).await;
    let mut b = node(&b_id, &[&a_peer], &["general"], SubstrateConfig::default()).await;
    connect(&mut a, &mut b, &b_peer).await;

    a.join(channel("general"), "pub")
        .await
        .expect("the command lands")
        .expect("the join is accepted");
    b.join(channel("general"), "sub")
        .await
        .expect("the command lands")
        .expect("the join is accepted");

    // BROADCAST WORKS.
    publish_repeatedly(&a, "pub", 1, b"over the mesh").await;
    let held = drain_until(&b, "sub", PATIENCE).await;
    assert_eq!(held.len(), 1, "the broadcast reached the subscriber");
    assert_eq!(held[0].payload.bytes(), b"over the mesh");
    assert_eq!(held[0].channel, channel("general"));
    assert_eq!(held[0].source_peer, a_peer);

    // AND DIRECT STILL WORKS, on the same pair, afterwards. The gate is
    // that neither mode's machinery has quietly taken the other over.
    let frame = interweave_transport_api::DirectMessageV2 {
        message_id: MessageId::from_bytes([9; 16]),
        sent_at_ms: 1_786_600_000_000,
        source_endpoint: endpoint("human"),
        destination_endpoint: None,
        payload: Payload::at_ceiling(None, b"directly".to_vec()).expect("within the ceiling"),
    };
    a.send_direct(b_peer.clone(), frame)
        .await
        .expect("the command lands")
        .expect("the send is accepted");

    let direct = b
        .drain_endpoint(endpoint("human"))
        .await
        .expect("the task answers");
    assert_eq!(direct.len(), 1, "the direct message arrived too");
    assert_eq!(direct[0].payload.bytes(), b"directly");

    // And the broadcast did NOT arrive on an endpoint queue, nor the
    // direct message on a session queue: the two paths stay separate.
    assert!(
        drain_until(&b, "sub", SILENCE).await.is_empty(),
        "the direct message must not reach a broadcast session"
    );

    a.shutdown().await.expect("a stops");
    b.shutdown().await.expect("b stops");
}

/// Publishing without the caller's own join is refused BEFORE the wire.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn publishing_without_a_join_is_refused_and_nothing_reaches_the_wire() {
    let (a_id, a_peer) = who();
    let (b_id, b_peer) = who();

    let mut a = node(&a_id, &[&b_peer], &["general"], SubstrateConfig::default()).await;
    let mut b = node(&b_id, &[&a_peer], &["general"], SubstrateConfig::default()).await;
    connect(&mut a, &mut b, &b_peer).await;

    // B joins and would receive; A never joins.
    b.join(channel("general"), "sub")
        .await
        .expect("the command lands")
        .expect("the join is accepted");

    let refusal = a
        .publish(channel("general"), "pub", envelope(2, b"unjoined"))
        .await
        .expect("the command lands");
    assert_eq!(
        refusal,
        Err(TransportError::ChannelNotJoined),
        "a caller with no join of its own may not publish"
    );

    // AND THE SUBSCRIBER SEES NOTHING, which is what proves the refusal
    // happened before any byte reached the backend rather than after.
    assert!(
        drain_until(&b, "sub", SILENCE).await.is_empty(),
        "a refused publish must not have been sent"
    );

    a.shutdown().await.expect("a stops");
    b.shutdown().await.expect("b stops");
}

/// A desired channel keeps the mesh warm and delivers to nobody.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_desired_channel_with_no_join_delivers_nothing_and_replays_nothing() {
    let (a_id, a_peer) = who();
    let (b_id, b_peer) = who();

    let mut a = node(&a_id, &[&b_peer], &["general"], SubstrateConfig::default()).await;
    let mut b = node(&b_id, &[&a_peer], &["general"], SubstrateConfig::default()).await;
    connect(&mut a, &mut b, &b_peer).await;

    a.join(channel("general"), "pub")
        .await
        .expect("the command lands")
        .expect("the join is accepted");

    // B DESIRES the channel but no session has joined it.
    publish_repeatedly(&a, "pub", 3, b"unheard").await;
    assert!(
        drain_until(&b, "late", SILENCE).await.is_empty(),
        "a warm mesh with no local consumer delivers to nobody"
    );

    // And a join afterwards does not replay what arrived before it.
    b.join(channel("general"), "late")
        .await
        .expect("the command lands")
        .expect("the join is accepted");
    assert!(
        drain_until(&b, "late", SILENCE).await.is_empty(),
        "a join must not replay messages that predate it"
    );

    a.shutdown().await.expect("a stops");
    b.shutdown().await.expect("b stops");
}

/// Only sessions that joined receive; a queue is not a subscription.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_the_joined_session_of_two_is_delivered_to() {
    let (a_id, a_peer) = who();
    let (b_id, b_peer) = who();

    let mut a = node(&a_id, &[&b_peer], &["general"], SubstrateConfig::default()).await;
    let mut b = node(&b_id, &[&a_peer], &["general"], SubstrateConfig::default()).await;
    connect(&mut a, &mut b, &b_peer).await;

    a.join(channel("general"), "pub")
        .await
        .expect("the command lands")
        .expect("accepted");
    // Only `human` joins. `claude` exists as a session elsewhere in the
    // profile but holds no join on this channel.
    b.join(channel("general"), "human")
        .await
        .expect("the command lands")
        .expect("accepted");

    publish_repeatedly(&a, "pub", 4, b"for the joined").await;

    let joined = drain_until(&b, "human", PATIENCE).await;
    assert_eq!(joined.len(), 1, "the joined session received it");
    assert!(
        drain_until(&b, "claude", SILENCE).await.is_empty(),
        "a session that never joined receives nothing"
    );

    a.shutdown().await.expect("a stops");
    b.shutdown().await.expect("b stops");
}

/// Leaving stops delivery; another session's join is untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leaving_stops_delivery_for_that_session_alone() {
    let (a_id, a_peer) = who();
    let (b_id, b_peer) = who();

    let mut a = node(&a_id, &[&b_peer], &["general"], SubstrateConfig::default()).await;
    let mut b = node(&b_id, &[&a_peer], &["general"], SubstrateConfig::default()).await;
    connect(&mut a, &mut b, &b_peer).await;

    a.join(channel("general"), "pub")
        .await
        .expect("lands")
        .expect("accepted");
    for session in ["stays", "leaves"] {
        b.join(channel("general"), session)
            .await
            .expect("lands")
            .expect("accepted");
    }

    publish_repeatedly(&a, "pub", 5, b"first").await;
    assert_eq!(drain_until(&b, "stays", PATIENCE).await.len(), 1);
    assert_eq!(drain_until(&b, "leaves", PATIENCE).await.len(), 1);

    b.leave(channel("general"), "leaves")
        .await
        .expect("the leave lands");

    a.publish(channel("general"), "pub", envelope(6, b"second"))
        .await
        .expect("lands")
        .expect("accepted");

    assert_eq!(
        drain_until(&b, "stays", PATIENCE).await.len(),
        1,
        "the remaining join still receives"
    );
    assert!(
        drain_until(&b, "leaves", SILENCE).await.is_empty(),
        "the session that left receives nothing further"
    );

    a.shutdown().await.expect("a stops");
    b.shutdown().await.expect("b stops");
}

/// Revoking trust stops broadcast delivery.
///
/// # What this proves, and what it does not
///
/// It proves the OUTCOME: after revocation the publisher's messages stop
/// arriving. It does not isolate WHICH of three layers stopped them, and
/// the comment here says so rather than implying otherwise — removing
/// `broadcast_state.adopt_trust` leaves this test passing, because
/// `set_trust` also closes the connection and blacklists the peer from
/// the mesh.
///
/// That is a real limit of an end-to-end test against a design with
/// defence in depth, not a reason to weaken the layers. The trust copy
/// remains the one that catches a message already in flight when the
/// revocation lands, which neither closure nor blacklisting can. Its
/// unit-level behaviour is covered where it is isolable, in
/// `broadcast_inbound`'s own tests, where an unauthorized publisher is
/// `Ignore` with no connection involved.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoking_trust_stops_broadcast_delivery() {
    let (a_id, a_peer) = who();
    let (b_id, b_peer) = who();

    let mut a = node(&a_id, &[&b_peer], &["general"], SubstrateConfig::default()).await;
    let mut b = node(&b_id, &[&a_peer], &["general"], SubstrateConfig::default()).await;
    connect(&mut a, &mut b, &b_peer).await;

    a.join(channel("general"), "pub")
        .await
        .expect("lands")
        .expect("accepted");
    b.join(channel("general"), "sub")
        .await
        .expect("lands")
        .expect("accepted");

    publish_repeatedly(&a, "pub", 7, b"while trusted").await;
    assert_eq!(
        drain_until(&b, "sub", PATIENCE).await.len(),
        1,
        "delivered while the publisher was trusted"
    );

    // B revokes A. Trust is emptied, so nothing is authorized.
    b.set_trust(trusting(&[]))
        .await
        .expect("the trust change lands");

    let _ = a
        .publish(channel("general"), "pub", envelope(8, b"after revocation"))
        .await
        .expect("the command lands");

    assert!(
        drain_until(&b, "sub", SILENCE).await.is_empty(),
        "a revoked publisher's message must not be admitted"
    );

    a.shutdown().await.expect("a stops");
    b.shutdown().await.expect("b stops");
}

/// A broadcast flood must not wedge the direct path.
///
/// `polling_room`'s own comment records it has been wrong three times,
/// and every instance was one event class spending room that belonged to
/// another. Broadcast is a new class arriving at the same outbox, so it
/// is the obvious candidate for a fourth.
///
/// # What this deliberately does NOT do
///
/// It does not leave the receiver's event stream undrained. That case is
/// already answered by the existing design rather than by broadcast: once
/// the outbox reaches base capacity `polling_room` stops polling the
/// Swarm for EVERY class, so a consumer that never drains stops the node
/// serving anything — direct included, with no broadcast involved. Making
/// that the assertion would test the backpressure contract and report it
/// as a broadcast defect.
///
/// What is broadcast-specific is whether its deliveries consume the room
/// a direct exchange needs while the node is otherwise healthy. So the
/// receiver drains, the flood runs concurrently, and the direct exchange
/// must still settle.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_broadcast_flood_does_not_wedge_the_direct_path() {
    let (a_id, a_peer) = who();
    let (b_id, b_peer) = who();

    // A SMALL EVENT CHANNEL, so the flood genuinely presses on the outbox
    // rather than fitting in it, while the receiver still drains and so
    // still polls its Swarm.
    let cramped = SubstrateConfig {
        event_capacity: 2,
        ..SubstrateConfig::default()
    };
    let mut a = node(&a_id, &[&b_peer], &["general"], SubstrateConfig::default()).await;
    let mut b = node(&b_id, &[&a_peer], &["general"], cramped).await;
    connect(&mut a, &mut b, &b_peer).await;

    a.join(channel("general"), "pub")
        .await
        .expect("lands")
        .expect("accepted");
    b.join(channel("general"), "sub")
        .await
        .expect("lands")
        .expect("accepted");

    // Flood, draining one event per publish so the receiver keeps
    // polling. The outbox stays under pressure throughout.
    for i in 0..32u8 {
        let _ = a
            .publish(channel("general"), "pub", envelope(i, b"flood"))
            .await
            .expect("the command lands");
        let _ = tokio::time::timeout(Duration::from_millis(20), b.next_event()).await;
    }

    // AND THE DIRECT EXCHANGE STILL SETTLES. Bounded, because the failure
    // this guards against is a hang, and a test that hangs reports
    // nothing at all.
    let frame = interweave_transport_api::DirectMessageV2 {
        message_id: MessageId::from_bytes([200; 16]),
        sent_at_ms: 1_786_600_000_000,
        source_endpoint: endpoint("human"),
        destination_endpoint: None,
        payload: Payload::at_ceiling(None, b"through the flood".to_vec())
            .expect("within the ceiling"),
    };
    let answered = tokio::time::timeout(PATIENCE, a.send_direct(b_peer.clone(), frame))
        .await
        .expect("the exchange settled rather than hanging behind a broadcast flood")
        .expect("the command lands");
    assert!(
        answered.is_ok(),
        "a broadcast flood must not stop the receiver answering direct: {answered:?}"
    );

    // And the flood did arrive: a test where nothing was published would
    // pass this trivially.
    assert!(
        !drain_until(&b, "sub", SILENCE).await.is_empty(),
        "the flood must actually have reached the subscriber"
    );

    a.shutdown().await.expect("a stops");
    b.shutdown().await.expect("b stops");
}

/// Publish the same envelope a few times while the mesh forms.
///
/// A single publish immediately after connecting reaches nobody: GossipSub
/// needs the subscription to have propagated, which takes heartbeats. That
/// is a property of the protocol, not a flake to paper over — so the
/// message is re-sent rather than the test being given a longer sleep and
/// a hope.
///
/// Re-sending is SAFE rather than merely tolerable: the runtime dedup key
/// is (publisher, channel, message_id) and every attempt carries the same
/// id, so at most one delivery can result however many attempts land.
async fn publish_repeatedly(publisher: &SwarmRuntime, session: &str, id: u8, body: &[u8]) {
    for _ in 0..12 {
        let _ = publisher
            .publish(channel("general"), session, envelope(id, body))
            .await
            .expect("the command lands");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
