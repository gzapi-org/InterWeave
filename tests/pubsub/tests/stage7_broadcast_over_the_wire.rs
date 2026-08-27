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
    BroadcastMessageV1, ChannelId, EndpointId, MAX_MEDIA_TYPE_BYTES, MAX_PAYLOAD_BYTES, MediaType,
    MessageId, Payload, TransportError, TransportIdentity,
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
    node_with_queue(identity, peers, desired, config, 64).await
}

/// The same, with an explicit per-session queue bound.
async fn node_with_queue(
    identity: &ProfileIdentity,
    peers: &[&TransportIdentity],
    desired: &[&str],
    config: SubstrateConfig,
    queue_bound: usize,
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
            BroadcastChannels::from_profile(&profile(desired), queue_bound)
                .expect("a valid profile"),
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
/// It also does not carry the precise slack property. That one —
/// a delivery may not spend the room an in-flight exchange bought — is
/// deterministic and already unit-tested as
/// `a_delivery_may_not_spend_the_slack_an_exchange_bought`, next to
/// `polling_room` itself. Reproducing it end to end would mean timing the
/// outbox to be near-full exactly while an exchange is in flight, which
/// is a race, not a test.
///
/// What is left for this test is the coarse claim worth having over real
/// sockets: a flood does not WEDGE the direct path. The receiver drains,
/// the flood runs concurrently, and the exchange must still settle.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_broadcast_flood_does_not_wedge_the_direct_path() {
    let (a_id, a_peer) = who();
    let (b_id, b_peer) = who();

    // A SMALL EVENT CHANNEL, so the flood genuinely presses on the outbox
    // rather than fitting in it, while the receiver still drains and so
    // still polls its Swarm.
    let cramped = SubstrateConfig {
        event_capacity: 8,
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
        // Drain to empty rather than one event per publish. With a
        // single poll the receiver's outbox could stay full under load,
        // `polling_room` would go false, and the direct request would
        // time out — a race between this loop and the flood rather than
        // anything about broadcast. That made this test flaky under
        // whole-suite contention and passing in isolation.
        while tokio::time::timeout(Duration::from_millis(25), b.next_event())
            .await
            .is_ok()
        {}
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

/// An infrastructure-only peer cannot join the mesh (testing.md 226).
///
/// Proven at the CONNECTION layer, and that is where the proof belongs:
/// `settle_outcome` refuses an inbound connection from a peer whose class
/// is `ConnectivityInfrastructureOnly`, so the socket closes before any
/// protocol negotiates. GossipSub never sees the peer — not because the
/// behaviour refused it, which it cannot, but because the gate did.
/// `stage6_malformed_frames.rs` records the same fact for direct.
///
/// The mesh-level blacklist is the second layer, for the class changing
/// after establishment. This test proves the first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_infrastructure_only_peer_cannot_join_the_mesh() {
    let (infra_id, infra_peer) = who();
    let (node_id, node_peer) = who();

    // The node knows the infra peer for reachability control ONLY.
    let node_trust = TrustSources::new(
        PeerTrustPolicy::new([]).expect("nobody on the data plane"),
        InfrastructureSet::new([infra_peer.clone()]).expect("one infrastructure peer"),
    );
    let mut victim = SwarmRuntime::start(&node_id, SubstrateConfig::default(), node_trust)
        .expect("the node starts");
    victim
        .configure_broadcast(
            BroadcastChannels::from_profile(&profile(&["general"]), 64).expect("valid"),
        )
        .await
        .expect("installs");
    victim
        .join(channel("general"), "sub")
        .await
        .expect("lands")
        .expect("accepted");

    // The infra peer, for its part, treats the node as data-plane and
    // tries to broadcast to it.
    let infra = node(
        &infra_id,
        &[&node_peer],
        &["general"],
        SubstrateConfig::default(),
    )
    .await;
    infra
        .join(channel("general"), "pub")
        .await
        .expect("lands")
        .expect("accepted");

    let address = victim
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("a loopback address"))
        .await
        .expect("listens");
    infra
        .dial(node_peer.clone(), address)
        .await
        .expect("the command lands")
        .expect("the dial is admitted on the infra side");

    // THE NODE NEVER ANNOUNCES A CONNECTION. Either nothing arrives, or
    // the close lands first; both mean no data-plane connection was
    // retained.
    let announced = tokio::time::timeout(SILENCE, async {
        loop {
            match victim.next_event().await {
                Some(SwarmEvent::Connected { .. }) => return true,
                Some(_) => {}
                None => return false,
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(
        !announced,
        "an infrastructure-only peer must not hold a data-plane connection"
    );

    // And whatever it publishes reaches nobody.
    publish_repeatedly(&infra, "pub", 20, b"from infra").await;
    assert!(
        drain_until(&victim, "sub", SILENCE).await.is_empty(),
        "nothing from an infrastructure-only peer may be delivered"
    );

    infra.shutdown().await.expect("infra stops");
    victim.shutdown().await.expect("the node stops");
}

/// An unauthorized original publisher is Ignore: not delivered, not
/// forwarded, and the relay is not penalised.
///
/// Four real peers in a chain, because each of the three claims needs a
/// different observer:
///
/// ```text
/// A (publisher) — R (relay, trusts A, C) — C (trusts R, D; NOT A) — D (trusts C, R, A)
/// ```
///
/// - NOT DELIVERED is observed at C: its session drains empty.
/// - NOT FORWARDED is observed only at D, which is reachable solely
///   through C. D TRUSTS A, deliberately: if C forwarded, D would
///   deliver. Were D not to trust A, its silence would be D ignoring the
///   message itself and would prove nothing about C.
/// - NOT PENALISED is observed by R still reaching C afterwards: a
///   message R publishes itself is delivered, so R was neither closed
///   nor pruned for having relayed something C did not authorize.
///
/// The positive control is what makes D's silence evidence, and D trusts
/// R for the same reason it trusts A: the trust question is about the
/// ORIGINAL publisher, so a control from R reaches D only if D authorizes
/// R. The first version of this test had D trusting C alone, and D
/// ignored the control — the test's own trust rule, applied to its
/// author.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unauthorized_publisher_is_ignored_not_delivered_and_not_relayed_further() {
    let (a_id, a_peer) = who();
    let (r_id, r_peer) = who();
    let (c_id, c_peer) = who();
    let (d_id, d_peer) = who();

    let mut a = node(&a_id, &[&r_peer], &["general"], SubstrateConfig::default()).await;
    let mut r = node(
        &r_id,
        &[&a_peer, &c_peer],
        &["general"],
        SubstrateConfig::default(),
    )
    .await;
    // C does NOT trust A.
    let mut c = node(
        &c_id,
        &[&r_peer, &d_peer],
        &["general"],
        SubstrateConfig::default(),
    )
    .await;
    // D trusts A so that a forwarded message WOULD be delivered — D's
    // silence is then C not forwarding, not D ignoring.
    let mut d = node(
        &d_id,
        &[&c_peer, &r_peer, &a_peer],
        &["general"],
        SubstrateConfig::default(),
    )
    .await;

    connect(&mut a, &mut r, &r_peer).await;
    connect(&mut r, &mut c, &c_peer).await;
    connect(&mut c, &mut d, &d_peer).await;

    for (n, session) in [(&a, "pub"), (&r, "relay"), (&c, "sub"), (&d, "far")] {
        n.join(channel("general"), session)
            .await
            .expect("lands")
            .expect("accepted");
    }

    // POSITIVE CONTROL FIRST: R publishes, and it reaches both C and D.
    // This proves the chain forwards at all, so the silence below is
    // Ignore doing its job and not a mesh that never formed.
    publish_repeatedly(&r, "relay", 30, b"control").await;
    assert_eq!(
        drain_until(&c, "sub", PATIENCE).await.len(),
        1,
        "the control reaches C"
    );
    assert_eq!(
        drain_until(&d, "far", PATIENCE).await.len(),
        1,
        "and is forwarded by C to D"
    );

    // NOW A PUBLISHES. R trusts A and forwards; C does not trust A.
    publish_repeatedly(&a, "pub", 31, b"from an untrusted origin").await;

    assert!(
        drain_until(&c, "sub", SILENCE).await.is_empty(),
        "NOT DELIVERED: C does not authorize A as a publisher"
    );
    assert!(
        drain_until(&d, "far", SILENCE).await.is_empty(),
        "NOT FORWARDED: D is reachable only through C, and C must not relay an Ignore"
    );

    // NOT PENALISED: R can still reach C afterwards. A `Reject` would
    // have counted against R for forwarding it; `Ignore` does not.
    publish_repeatedly(&r, "relay", 32, b"still here").await;
    assert_eq!(
        drain_until(&c, "sub", PATIENCE).await.len(),
        1,
        "R was not penalised for relaying a message C ignored"
    );

    for n in [a, r, c, d] {
        n.shutdown().await.expect("stops");
    }
}

/// Invalid-signature traffic cannot poison the cache against later
/// authentic traffic (`PUBSUB.md`'s SPIKE-002/Phase 2 MUST).
///
/// A peer this node TRUSTS at the connection layer publishes messages
/// claiming to originate from another peer, without a signature. Strict
/// validation must refuse them before they can affect what the genuine
/// publisher's later messages do.
///
/// # What this proves, and the collision it cannot construct
///
/// The MUST asks that invalid source/sequence messages be rejected
/// before they create a lasting duplicate-cache entry. A literal
/// collision — forging the exact `(source, sequence)` the genuine
/// publisher will next use — is NOT constructible from outside the
/// backend: `sequence_number` is assigned internally
/// (`behaviour.rs`'s `last_seq_no.next()`), so no publisher, honest or
/// otherwise, chooses it.
///
/// What is observable is the property the MUST exists to protect: after
/// a stream of forged messages bearing the genuine publisher's identity,
/// that publisher's real message is still delivered. If a forgery had
/// created a valid-message cache entry, this is where it would show.
///
/// The ordering itself is visible in the vendored backend — validation
/// at `behaviour.rs:1782` precedes `duplicate_cache.insert` at `:1827` —
/// but reading it pins nothing across an upgrade, which is why the
/// behavioural half is here.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invalid_signature_traffic_cannot_poison_the_cache_for_authentic_traffic() {
    use futures::StreamExt;

    let (a_id, a_peer) = who();
    let (v_id, v_peer) = who();
    let forger_keys = libp2p::identity::Keypair::generate_ed25519();
    let forger_peer = TransportIdentity::parse(
        libp2p::PeerId::from_public_key(&forger_keys.public()).to_base58(),
    )
    .expect("a canonical peer id");

    // The victim trusts BOTH: the forger is a trusted peer abusing its
    // connection, which is the only party that can reach the mesh at all.
    let mut victim = node(
        &v_id,
        &[&a_peer, &forger_peer],
        &["general"],
        SubstrateConfig::default(),
    )
    .await;
    let mut a = node(&a_id, &[&v_peer], &["general"], SubstrateConfig::default()).await;
    victim
        .join(channel("general"), "sub")
        .await
        .expect("lands")
        .expect("accepted");
    a.join(channel("general"), "pub")
        .await
        .expect("lands")
        .expect("accepted");

    let address = victim
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("a loopback address"))
        .await
        .expect("the victim listens");

    // THE FORGER: a raw backend with no signing and no validation, which
    // is the only way to emit what a conforming node cannot. It claims
    // `a_peer` as the author and signs nothing.
    let topic = libp2p::gossipsub::IdentTopic::new(
        interweave_transport_runtime::topic::topic_key_v1(&channel("general")).wire_string(),
    );
    let forged_body = envelope(99, b"forged").encode();
    let forger_address = address.clone();
    let author = libp2p::PeerId::from_public_key(&a_id.swarm_keypair().public());

    let forger = tokio::spawn(async move {
        let mut swarm = libp2p::SwarmBuilder::with_existing_identity(forger_keys)
            .with_tokio()
            .with_tcp(
                libp2p::tcp::Config::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )
            .expect("the same transport stack the victim uses")
            .with_behaviour(|_| {
                libp2p::gossipsub::Behaviour::<
                    libp2p::gossipsub::IdentityTransform,
                    libp2p::gossipsub::AllowAllSubscriptionFilter,
                >::new(
                    // AUTHOR, not Signed: the message carries `a_peer` as
                    // its source and no signature at all.
                    libp2p::gossipsub::MessageAuthenticity::Author(author),
                    libp2p::gossipsub::ConfigBuilder::default()
                        .validation_mode(libp2p::gossipsub::ValidationMode::None)
                        .build()
                        .expect("a permissive config"),
                )
                .expect("the forging behaviour builds")
            })
            .expect("behaviour")
            .build();

        swarm.behaviour_mut().subscribe(&topic).expect("subscribes");
        swarm.dial(forger_address).expect("dials the victim");

        for _ in 0..40 {
            let _ = swarm
                .behaviour_mut()
                .publish(topic.hash(), forged_body.clone());
            let _ = tokio::time::timeout(Duration::from_millis(50), swarm.select_next_some()).await;
        }
    });

    a.dial(v_peer.clone(), address)
        .await
        .expect("the command lands")
        .expect("the dial is admitted");
    wait_for(&mut a, "a connection to the victim", |e| {
        matches!(e, SwarmEvent::Connected { .. })
    })
    .await;

    // POSITIVE CONTROL: the forger really did reach the victim. Both
    // assertions below hold trivially if it never connected, so without
    // this the test would pass most loudly when it was testing nothing.
    wait_for(
        &mut victim,
        "the forger's connection",
        |e| matches!(e, SwarmEvent::Connected { peer } if *peer == forger_peer),
    )
    .await;

    let _ = forger.await;

    // THE GENUINE PUBLISHER STILL GETS THROUGH. If a forged message had
    // created a lasting valid-message cache entry under the identity it
    // claimed, this is what would fail.
    publish_repeatedly(&a, "pub", 98, b"authentic").await;
    let held = drain_until(&victim, "sub", PATIENCE).await;
    assert!(
        !held.is_empty(),
        "the authentic publisher must still be delivered after forged traffic"
    );

    // And no forgery was ever delivered, whatever else happened.
    assert!(
        held.iter().all(|e| e.payload.bytes() != b"forged"),
        "an unsigned message claiming another peer's identity must never be delivered"
    );

    a.shutdown().await.expect("a stops");
    victim.shutdown().await.expect("the victim stops");
}

/// The last leave on a channel the profile does not desire drops the
/// backend subscription — observed at the OTHER peer.
///
/// Backend subscription state has no local witness worth trusting: this
/// node's own bookkeeping is the thing under test. What a peer sees is
/// the mesh's actual behaviour, so A watches for B's `PeerUnsubscribed`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_last_leave_on_an_undesired_channel_drops_the_backend_subscription() {
    let (a_id, a_peer) = who();
    let (b_id, b_peer) = who();

    // B desires NOTHING, so a join is the only thing holding the topic.
    let mut a = node(&a_id, &[&b_peer], &["general"], SubstrateConfig::default()).await;
    let mut b = node(&b_id, &[&a_peer], &[], SubstrateConfig::default()).await;
    connect(&mut a, &mut b, &b_peer).await;

    b.join(channel("general"), "only")
        .await
        .expect("lands")
        .expect("accepted");
    wait_for(&mut a, "B's subscription", |e| {
        matches!(e, SwarmEvent::PeerSubscribed { peer, channel: c } if *peer == b_peer && *c == channel("general"))
    })
    .await;

    b.leave(channel("general"), "only")
        .await
        .expect("the leave lands");

    wait_for(&mut a, "B's unsubscription", |e| {
        matches!(e, SwarmEvent::PeerUnsubscribed { peer, channel: c } if *peer == b_peer && *c == channel("general"))
    })
    .await;

    a.shutdown().await.expect("a stops");
    b.shutdown().await.expect("b stops");
}

/// Leaving a channel the profile desires keeps the mesh warm.
///
/// The negative twin of the test above, and the reason `desired` exists:
/// a client leaving must not tear down a subscription the operator asked
/// the daemon to hold.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leaving_a_desired_channel_keeps_the_mesh_warm() {
    let (a_id, a_peer) = who();
    let (b_id, b_peer) = who();

    // B DESIRES the channel.
    let mut a = node(&a_id, &[&b_peer], &["general"], SubstrateConfig::default()).await;
    let mut b = node(&b_id, &[&a_peer], &["general"], SubstrateConfig::default()).await;
    connect(&mut a, &mut b, &b_peer).await;

    b.join(channel("general"), "only")
        .await
        .expect("lands")
        .expect("accepted");
    b.leave(channel("general"), "only")
        .await
        .expect("the leave lands");

    // POSITIVE CONTROL and the assertion in one: A must still be able to
    // reach a session on B afterwards, which needs B's subscription to
    // be live. A second session joins after the leave and receives.
    b.join(channel("general"), "later")
        .await
        .expect("lands")
        .expect("accepted");
    a.join(channel("general"), "pub")
        .await
        .expect("lands")
        .expect("accepted");
    publish_repeatedly(&a, "pub", 40, b"still warm").await;
    assert_eq!(
        drain_until(&b, "later", PATIENCE).await.len(),
        1,
        "the desired subscription survived the leave"
    );

    // And A never saw an unsubscription in the interval.
    let seen = tokio::time::timeout(SILENCE, async {
        loop {
            match a.next_event().await {
                Some(SwarmEvent::PeerUnsubscribed { peer, .. }) if peer == b_peer => return true,
                Some(_) => {}
                None => return false,
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(
        !seen,
        "a desired channel must not be unsubscribed by a client's leave"
    );

    a.shutdown().await.expect("a stops");
    b.shutdown().await.expect("b stops");
}

/// Shutdown leaves the mesh before the swarm drops.
///
/// A dropped swarm closes connections, and a closing connection says
/// nothing about subscriptions: the peer keeps this node in its topic set
/// until its own timers age it out. The unsubscription must therefore go
/// out while the connection is still alive, which means A observes it
/// BEFORE the disconnection rather than instead of it.
///
/// The ordering is the assertion. Draining until `Disconnected` and
/// asserting the unsubscription was seen somewhere in that prefix is what
/// makes this a test of "before the swarm drops" rather than of "at some
/// point".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_unsubscribes_before_dropping_the_swarm() {
    let (a_id, a_peer) = who();
    let (b_id, b_peer) = who();

    let mut a = node(&a_id, &[&b_peer], &["general"], SubstrateConfig::default()).await;
    let mut b = node(&b_id, &[&a_peer], &["general"], SubstrateConfig::default()).await;
    connect(&mut a, &mut b, &b_peer).await;

    wait_for(
        &mut a,
        "B's subscription",
        |e| matches!(e, SwarmEvent::PeerSubscribed { peer, .. } if *peer == b_peer),
    )
    .await;

    b.shutdown().await.expect("b stops");

    // Drain A until B's disconnection, remembering whether the
    // unsubscription arrived first.
    let mut unsubscribed_first = false;
    let ordering = tokio::time::timeout(PATIENCE, async {
        loop {
            match a.next_event().await {
                Some(SwarmEvent::PeerUnsubscribed { peer, .. }) if peer == b_peer => {
                    unsubscribed_first = true;
                }
                Some(SwarmEvent::Disconnected { peer, .. }) if peer == b_peer => return true,
                Some(_) => {}
                None => return false,
            }
        }
    })
    .await
    .expect("B's disconnection reached A");
    assert!(ordering, "the event stream ended before B disconnected");
    assert!(
        unsubscribed_first,
        "the unsubscription must go out while the connection is still alive"
    );

    a.shutdown().await.expect("a stops");
}

/// Publishing into an empty mesh is local success, not an error.
///
/// `NoPeersSubscribedToTopic` means nobody is listening yet, which is the
/// ordinary state of a node that just started. Broadcast is fire-and-forget
/// (ADR-0029): a publisher gets no per-message answer about reach, so the
/// only honest local answer is that the message was accepted for sending.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publishing_into_an_empty_mesh_is_local_success() {
    let (a_id, _) = who();
    let a = node(&a_id, &[], &["general"], SubstrateConfig::default()).await;
    a.join(channel("general"), "only")
        .await
        .expect("lands")
        .expect("accepted");

    let answer = a
        .publish(channel("general"), "only", envelope(1, b"into the void"))
        .await
        .expect("the command lands");
    assert!(
        answer.is_ok(),
        "an empty mesh is not a publish failure: {answer:?}"
    );

    a.shutdown().await.expect("a stops");
}

/// A full session queue drops for that session, and the mesh still
/// forwards to everyone else.
///
/// The distinction the whole delivery path rests on: a local queue is a
/// LOCAL fact. A relay whose own consumer is behind must go on carrying
/// traffic for its neighbours, because the alternative is one slow client
/// degrading a channel for every peer downstream of it.
///
/// A—B—C, with C reachable only through B. B's session holds one and drops
/// the rest; C's holds both, which it can only do if B forwarded while
/// dropping.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_session_queue_drops_for_that_session_and_the_mesh_still_forwards() {
    let (a_id, a_peer) = who();
    let (b_id, b_peer) = who();
    let (c_id, c_peer) = who();

    let mut a = node(&a_id, &[&b_peer], &["general"], SubstrateConfig::default()).await;
    // B keeps room for exactly ONE delivery per session.
    let mut b = node_with_queue(
        &b_id,
        &[&a_peer, &c_peer],
        &["general"],
        SubstrateConfig::default(),
        1,
    )
    .await;
    // C trusts A, the ORIGINAL publisher — trust is answered about the
    // author, not the relay that carried the message. C trusting only B
    // would make C ignore everything A sends and this test would read a
    // forwarding failure that was not one.
    let mut c = node(
        &c_id,
        &[&b_peer, &a_peer],
        &["general"],
        SubstrateConfig::default(),
    )
    .await;

    connect(&mut a, &mut b, &b_peer).await;
    connect(&mut c, &mut b, &b_peer).await;

    for r in [&a, &b, &c] {
        r.join(channel("general"), "sub")
            .await
            .expect("lands")
            .expect("accepted");
    }

    publish_repeatedly(&a, "sub", 1, b"first").await;
    publish_repeatedly(&a, "sub", 2, b"second").await;

    let at_c = drain_until(&c, "sub", PATIENCE).await;
    assert_eq!(
        at_c.len(),
        2,
        "the relay must forward both even though its own session could hold only one"
    );
    let at_b = drain_until(&b, "sub", Duration::from_secs(1)).await;
    assert_eq!(at_b.len(), 1, "the bounded session held exactly its bound");

    a.shutdown().await.expect("a stops");
    b.shutdown().await.expect("b stops");
    c.shutdown().await.expect("c stops");
}

/// A signed but malformed envelope is Reject, and does not wedge later
/// valid traffic from the same publisher.
///
/// Objective invalidity — a body that is not a `BroadcastMessageV1` — is
/// Reject under ADR-0029, and later valid traffic from the same publisher
/// is unaffected.
///
/// # What it does NOT prove
///
/// Not that the Reject was REPORTED. Mutation says so: suppressing the
/// report on the Reject arm leaves this test passing, because an
/// unreported message sits in the backend's cache without blocking a
/// later message with a different id. The cost of not reporting is
/// unreclaimed cache, which has no end-to-end signal.
///
/// The report is not unverified in general — suppressing it on the ACCEPT
/// arm fails `an_unauthorized_publisher_is_ignored_not_delivered_and_not_relayed_further`,
/// whose positive control depends on a relay forwarding, and forwarding is
/// what the report releases. Accept is covered; Reject and Ignore are
/// argued from the shared code path, not observed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_signed_but_malformed_envelope_is_reject_and_does_not_wedge_later_valid_traffic() {
    use futures::StreamExt;

    let (v_id, _v_peer) = who();
    let forger_keys = libp2p::identity::Keypair::generate_ed25519();
    let forger_peer = TransportIdentity::parse(
        libp2p::PeerId::from_public_key(&forger_keys.public()).to_base58(),
    )
    .expect("a canonical peer id");

    let mut victim = node(
        &v_id,
        &[&forger_peer],
        &["general"],
        SubstrateConfig::default(),
    )
    .await;
    victim
        .join(channel("general"), "sub")
        .await
        .expect("lands")
        .expect("accepted");
    let address = victim
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("a loopback address"))
        .await
        .expect("the victim listens");

    let topic = libp2p::gossipsub::IdentTopic::new(
        interweave_transport_runtime::topic::topic_key_v1(&channel("general")).wire_string(),
    );
    let valid = envelope(7, b"well formed").encode();

    let publisher = tokio::spawn(async move {
        let mut swarm = libp2p::SwarmBuilder::with_existing_identity(forger_keys)
            .with_tokio()
            .with_tcp(
                libp2p::tcp::Config::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )
            .expect("the same transport stack the victim uses")
            .with_behaviour(|keys| {
                libp2p::gossipsub::Behaviour::<
                    libp2p::gossipsub::IdentityTransform,
                    libp2p::gossipsub::AllowAllSubscriptionFilter,
                >::new(
                    // SIGNED, and by its own key: everything about this
                    // publisher is valid except the bytes it sends, so the
                    // verdict can only come from decoding.
                    libp2p::gossipsub::MessageAuthenticity::Signed(keys.clone()),
                    libp2p::gossipsub::ConfigBuilder::default()
                        .build()
                        .expect("a default config"),
                )
                .expect("the behaviour builds")
            })
            .expect("behaviour")
            .build();

        swarm.behaviour_mut().subscribe(&topic).expect("subscribes");
        swarm.dial(address).expect("dials the victim");

        // Garbage first, repeatedly, then the well-formed envelope.
        for round in 0..40 {
            let body = if round < 30 {
                vec![0xff; 24 + round]
            } else {
                valid.clone()
            };
            let _ = swarm.behaviour_mut().publish(topic.hash(), body);
            let _ = tokio::time::timeout(Duration::from_millis(50), swarm.select_next_some()).await;
        }
    });

    // POSITIVE CONTROL: the publisher really reached the victim.
    wait_for(
        &mut victim,
        "the publisher's connection",
        |e| matches!(e, SwarmEvent::Connected { peer } if *peer == forger_peer),
    )
    .await;
    let _ = publisher.await;

    let held = drain_until(&victim, "sub", PATIENCE).await;
    assert_eq!(
        held.len(),
        1,
        "exactly the well-formed envelope is delivered, and the malformed ones neither \
         reached the session nor wedged it: {held:?}"
    );
    assert_eq!(held[0].payload.bytes(), b"well formed");

    victim.shutdown().await.expect("the victim stops");
}

/// GossipSub originates no dial.
///
/// The behaviour is composed for mesh traffic on connections the root gate
/// already admitted; it must never reach for a peer on its own. A known
/// address the node was never told to dial is the case that would expose
/// it — gossipsub sees the peer in its address book and, if it dialed at
/// all, would dial here.
///
/// This pins the claim in `behaviour.rs` and `Cargo.toml` to a test, per
/// CLAUDE.md §4: an invariant stated in a comment owes one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gossipsub_never_originates_a_dial() {
    let (a_id, a_peer) = who();
    let (b_id, b_peer) = who();

    let mut a = node(&a_id, &[&b_peer], &["general"], SubstrateConfig::default()).await;
    let b = node(&b_id, &[&a_peer], &["general"], SubstrateConfig::default()).await;
    let address = b
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("a loopback address"))
        .await
        .expect("b listens");

    // A knows exactly where B is, and subscribes — but is never told to
    // dial. Only an autonomous behaviour would connect from here.
    a.add_address(b_peer.clone(), address)
        .await
        .expect("the address is recorded");
    a.join(channel("general"), "sub")
        .await
        .expect("lands")
        .expect("accepted");

    let connected = tokio::time::timeout(SILENCE, async {
        loop {
            match a.next_event().await {
                Some(SwarmEvent::Connected { peer }) if peer == b_peer => return true,
                Some(_) => {}
                None => return false,
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(
        !connected,
        "gossipsub must not dial a peer the admission gate was never asked about"
    );

    a.shutdown().await.expect("a stops");
    b.shutdown().await.expect("b stops");
}

/// The largest LEGAL broadcast crosses the wire.
///
/// `max_transmit_size` bounds the encoded GossipSub RPC, not the envelope
/// inside it, so a ceiling sized for the envelope alone silently refuses
/// the biggest message the application is allowed to send. Nothing local
/// catches that: the publisher's own limit check passes, and the failure
/// appears only when the backend declines to frame it.
///
/// Arithmetic cannot prove this — the framing belongs to the backend's
/// encoding, which is exactly why the ceiling is not computed exactly.
/// Only a maximum envelope actually arriving does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_largest_legal_broadcast_crosses_the_wire() {
    let (a_id, a_peer) = who();
    let (b_id, b_peer) = who();

    let mut a = node(&a_id, &[&b_peer], &["general"], SubstrateConfig::default()).await;
    let mut b = node(&b_id, &[&a_peer], &["general"], SubstrateConfig::default()).await;
    connect(&mut a, &mut b, &b_peer).await;
    for r in [&a, &b] {
        r.join(channel("general"), "sub")
            .await
            .expect("lands")
            .expect("accepted");
    }

    // EVERY field at its maximum, not just the payload: the longest legal
    // media type too, since it is part of what the framing must carry.
    let longest_media_type = format!("text/{}", "x".repeat(MAX_MEDIA_TYPE_BYTES - 5));
    let biggest = BroadcastMessageV1 {
        message_id: MessageId::from_bytes([0xab; 16]),
        sent_at_ms: u64::MAX,
        payload: Payload::at_ceiling(
            Some(MediaType::parse(&longest_media_type).expect("a legal media type")),
            vec![0xcd; MAX_PAYLOAD_BYTES],
        )
        .expect("exactly at the ceiling"),
    };
    // Republished while the mesh forms, exactly as `publish_repeatedly`
    // does: a single publish races subscription propagation, and the same
    // envelope id makes the repeats harmless.
    let mut published = None;
    for _ in 0..12 {
        published = Some(
            a.publish(channel("general"), "sub", biggest.clone())
                .await
                .expect("the command lands"),
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let answer = published.expect("at least one publish");
    assert!(
        answer.is_ok(),
        "the largest legal envelope publishes: {answer:?}"
    );

    let held = drain_until(&b, "sub", PATIENCE).await;
    assert_eq!(held.len(), 1, "it reached the other peer");
    assert_eq!(
        held[0].payload.bytes().len(),
        MAX_PAYLOAD_BYTES,
        "and arrived whole"
    );

    a.shutdown().await.expect("a stops");
    b.shutdown().await.expect("b stops");
}

/// A refused reconfiguration changes nothing, including the queue bound.
///
/// The bound used to be assigned before the registry was asked, so a
/// configuration the registry REFUSED still moved it: the caller was told
/// the configuration failed while every later session silently opened at
/// the bound from the rejected request. A partially applied config is
/// worse than a refused one, because nothing reports it.
///
/// The bound is observed rather than read: sessions opened after the
/// refusal must still drop at the ORIGINAL bound of one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_reconfiguration_leaves_the_queue_bound_alone() {
    let (a_id, a_peer) = who();
    let (b_id, b_peer) = who();

    let mut a = node(&a_id, &[&b_peer], &["general"], SubstrateConfig::default()).await;
    // B starts bounded at ONE.
    let mut b = node_with_queue(
        &b_id,
        &[&a_peer],
        &["general"],
        SubstrateConfig::default(),
        1,
    )
    .await;
    connect(&mut a, &mut b, &b_peer).await;

    // Push the registry past MAX_SUBSCRIPTIONS with joins, so that the
    // next desired set cannot be accepted.
    let mut filled = 0u32;
    for i in 0..2_000u32 {
        let c = ChannelId::parse(format!("bulk-{i}")).expect("a legal channel");
        if b.join(c, "filler").await.expect("lands").is_err() {
            break;
        }
        filled += 1;
    }
    assert!(
        filled > 1_000,
        "the registry filled to its ceiling before refusing: {filled}"
    );

    // A reconfiguration that asks for a LARGER bound and must be refused.
    // TWO desired channels, because `join` fills to exactly one below the
    // ceiling: a single desired channel lands on the boundary and is
    // legally accepted.
    let refused = b
        .configure_broadcast(
            BroadcastChannels::from_profile(&profile(&["general", "second"]), 64)
                .expect("a valid profile"),
        )
        .await;
    assert!(
        refused.is_err(),
        "the registry must refuse a set it cannot hold: {refused:?}"
    );

    // THE BOUND DID NOT MOVE. A session opened after the refusal still
    // holds exactly one.
    a.join(channel("general"), "pub")
        .await
        .expect("lands")
        .expect("accepted");
    b.join(channel("general"), "after")
        .await
        .expect("lands")
        .expect("accepted");
    publish_repeatedly(&a, "pub", 1, b"first").await;
    publish_repeatedly(&a, "pub", 2, b"second").await;

    assert_eq!(
        drain_until(&b, "after", PATIENCE).await.len(),
        1,
        "the rejected configuration's bound must not have been applied"
    );

    a.shutdown().await.expect("a stops");
    b.shutdown().await.expect("b stops");
}

/// Reconfiguring away a desired channel leaves the mesh, and one still
/// joined stays.
///
/// Subscribing to the new set is only half of applying it. A channel the
/// PREVIOUS set desired, that no session joins, is held by nobody once the
/// registry has answered — and used to stay subscribed anyway, receiving
/// and relaying traffic the node no longer wanted, one more with every
/// reconfiguration.
///
/// Both halves are asserted, because the dangerous fix is the
/// over-eager one: unsubscribing a channel a live session still joins
/// would silently stop delivering to it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconfiguring_drops_channels_nobody_holds_and_keeps_the_ones_joined() {
    let (a_id, a_peer) = who();
    let (b_id, b_peer) = who();

    let mut a = node(
        &a_id,
        &[&b_peer],
        &["dropped", "kept"],
        SubstrateConfig::default(),
    )
    .await;
    let mut b = node(
        &b_id,
        &[&a_peer],
        &["dropped", "kept"],
        SubstrateConfig::default(),
    )
    .await;
    connect(&mut a, &mut b, &b_peer).await;

    // A session joins ONLY "kept", so "dropped" is held by the profile
    // alone and "kept" is held by both.
    b.join(channel("kept"), "sub")
        .await
        .expect("lands")
        .expect("accepted");
    wait_for(&mut a, "B's subscriptions", |e| {
        matches!(e, SwarmEvent::PeerSubscribed { peer, channel: c } if *peer == b_peer && *c == channel("dropped"))
    })
    .await;

    // Reconfigure to desire NEITHER. "kept" survives on its join;
    // "dropped" is now held by nobody.
    b.configure_broadcast(
        BroadcastChannels::from_profile(&profile(&[]), 64).expect("a valid profile"),
    )
    .await
    .expect("the empty desired set is accepted");

    wait_for(&mut a, "the unsubscription of the dropped channel", |e| {
        if let SwarmEvent::PeerUnsubscribed { peer, channel: c } = e
            && *peer == b_peer
        {
            assert_ne!(
                *c,
                channel("kept"),
                "a channel a live session joins must NOT be unsubscribed"
            );
            return *c == channel("dropped");
        }
        false
    })
    .await;

    // AND "kept" IS STILL LIVE: a publish reaches the session that joined it.
    a.join(channel("kept"), "pub")
        .await
        .expect("lands")
        .expect("accepted");
    publish_repeatedly_on(&a, "kept", "pub", 5, b"still joined").await;
    assert_eq!(
        drain_until(&b, "sub", PATIENCE).await.len(),
        1,
        "a channel a live session joins must survive the reconfiguration"
    );

    a.shutdown().await.expect("a stops");
    b.shutdown().await.expect("b stops");
}

/// A session's final leave gives its queue back; a partial leave does not.
///
/// Nothing bounded the queue map. `SubscriptionRegistry` bounds sessions
/// per channel, but a local client that joins and leaves under a FRESH
/// session id each time left one queue entry behind per id, holding
/// whatever those queues had not drained. That is the memory-exhaustion
/// shape SPIKE-002 measured elsewhere, reached here without ever
/// exceeding a subscription bound.
///
/// # What this test proves, and what proves the rest
///
/// The PARTIAL leave is what it covers: a session that left one channel
/// of two is still owed deliveries on the other. That is the guard
/// against the over-eager fix, and it bites — closing on every leave
/// fails it.
///
/// The closure itself is NOT observable here, and mutation says so:
/// never closing the queue leaves this test passing. Once a session has
/// left its last channel it is no longer a subscriber, so nothing is
/// delivered to it whether its queue exists or not — the leak is invisible
/// from outside precisely because the leaked queue is unreachable. What
/// bounds it is the predicate, unit-tested beside `SubscriptionRegistry`
/// as `a_session_holding_one_of_two_channels_is_still_live` and
/// `one_sessions_leave_does_not_release_another`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_final_leave_closes_the_session_queue_and_a_partial_one_does_not() {
    let (a_id, a_peer) = who();
    let (b_id, b_peer) = who();

    let mut a = node(
        &a_id,
        &[&b_peer],
        &["general", "second"],
        SubstrateConfig::default(),
    )
    .await;
    let mut b = node(
        &b_id,
        &[&a_peer],
        &["general", "second"],
        SubstrateConfig::default(),
    )
    .await;
    connect(&mut a, &mut b, &b_peer).await;

    // The session holds TWO channels and leaves one: still live.
    for c in ["general", "second"] {
        b.join(channel(c), "two")
            .await
            .expect("lands")
            .expect("accepted");
    }
    b.leave(channel("general"), "two")
        .await
        .expect("the leave lands");

    a.join(channel("second"), "pub")
        .await
        .expect("lands")
        .expect("accepted");
    publish_repeatedly_on(&a, "second", "pub", 9, b"still owed").await;
    assert_eq!(
        drain_until(&b, "two", PATIENCE).await.len(),
        1,
        "a session that left one channel of two is still owed the other"
    );

    // Now it leaves the last one. The queue must be gone, which is
    // observable as a delivery that no longer lands in it.
    b.leave(channel("second"), "two")
        .await
        .expect("the leave lands");
    b.join(channel("second"), "other")
        .await
        .expect("lands")
        .expect("accepted");
    publish_repeatedly_on(&a, "second", "pub", 10, b"after the last leave").await;

    assert!(
        drain_until(&b, "two", Duration::from_secs(1))
            .await
            .is_empty(),
        "the closed queue holds nothing and is not reopened by a delivery"
    );
    assert_eq!(
        drain_until(&b, "other", PATIENCE).await.len(),
        1,
        "and the live session still receives"
    );

    a.shutdown().await.expect("a stops");
    b.shutdown().await.expect("b stops");
}

/// One broadcast cannot notify past the outbox bound, however many
/// sessions joined.
///
/// A single message fans out to every joined session, so a capacity
/// decision taken once before the loop lets one message append one
/// notification per session on the strength of one free slot. Direct
/// cannot do this — it notifies at most once per message — which is why
/// the single boolean it uses was not enough here.
///
/// The bound is observed as the node STAYING ALIVE: an outbox past its
/// capacity stops the Swarm being polled for every event class, so a node
/// that overshot would no longer answer anything. A direct exchange after
/// the fan-out is the probe.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_broadcast_to_many_sessions_cannot_overrun_the_outbox() {
    let (a_id, a_peer) = who();
    let (b_id, b_peer) = who();

    let cramped = SubstrateConfig {
        event_capacity: 4,
        ..SubstrateConfig::default()
    };
    let mut a = node(&a_id, &[&b_peer], &["general"], SubstrateConfig::default()).await;
    let mut b = node(&b_id, &[&a_peer], &["general"], cramped).await;
    connect(&mut a, &mut b, &b_peer).await;

    // FAR more joined sessions than the outbox can hold notifications for.
    for i in 0..32u32 {
        b.join(channel("general"), format!("s{i}"))
            .await
            .expect("lands")
            .expect("accepted");
    }
    a.join(channel("general"), "pub")
        .await
        .expect("lands")
        .expect("accepted");
    publish_repeatedly(&a, "pub", 3, b"fan out").await;

    // THE NODE IS STILL SERVING. If the fan-out had appended one
    // notification per session, the outbox would be past its capacity and
    // B would have stopped polling the Swarm for everything.
    let endpoint_frame = interweave_transport_api::DirectMessageV2 {
        message_id: MessageId::from_bytes([77; 16]),
        sent_at_ms: 1_786_600_000_000,
        source_endpoint: endpoint("human"),
        destination_endpoint: Some(endpoint("human")),
        payload: Payload::at_ceiling(
            Some(MediaType::parse("text/plain").expect("legal")),
            b"still serving".to_vec(),
        )
        .expect("within the ceiling"),
    };
    let answered = tokio::time::timeout(PATIENCE, a.send_direct(b_peer.clone(), endpoint_frame))
        .await
        .expect("the exchange settled rather than hanging behind a fan-out")
        .expect("the command lands");
    assert!(
        answered.is_ok(),
        "a fan-out must not push the outbox past its bound: {answered:?}"
    );

    // And the deliveries themselves are all there: what a full outbox
    // costs is the WAKE-UP, never the message.
    let mut delivered = 0;
    for i in 0..32u32 {
        delivered += drain_until(&b, &format!("s{i}"), Duration::from_millis(500))
            .await
            .len();
    }
    assert_eq!(
        delivered, 32,
        "every joined session still holds the message"
    );

    a.shutdown().await.expect("a stops");
    b.shutdown().await.expect("b stops");
}

/// An overload drop is announced, not silent.
///
/// Broadcast is allowed to drop for a session whose consumer is behind —
/// that is the design, and the alternative is one slow client stalling
/// the mesh for everyone. What it must not do is drop INVISIBLY: a gap
/// the consumer cannot distinguish from a message that was never sent is
/// the difference between a slow client and a broken network, and only
/// the node knows which.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_queue_overflow_is_announced() {
    let (a_id, a_peer) = who();
    let (b_id, b_peer) = who();

    let mut a = node(&a_id, &[&b_peer], &["general"], SubstrateConfig::default()).await;
    // Room for exactly one delivery, so the second is refused.
    let mut b = node_with_queue(
        &b_id,
        &[&a_peer],
        &["general"],
        SubstrateConfig::default(),
        1,
    )
    .await;
    connect(&mut a, &mut b, &b_peer).await;

    for r in [&a, &b] {
        r.join(channel("general"), "sub")
            .await
            .expect("lands")
            .expect("accepted");
    }

    publish_repeatedly(&a, "sub", 1, b"first").await;
    publish_repeatedly(&a, "sub", 2, b"second").await;

    wait_for(&mut b, "the drop announcement", |e| {
        matches!(
            e,
            SwarmEvent::BroadcastDropped { channel: c, source_peer, sessions }
                if *c == channel("general") && *source_peer == a_peer && *sessions == 1
        )
    })
    .await;

    a.shutdown().await.expect("a stops");
    b.shutdown().await.expect("b stops");
}

/// Two local clients on one profile see each other's broadcasts.
///
/// `human-client-model-b.md`: "If human and Claude both join
/// `project-alpha`, both receive broadcast events because both explicitly
/// joined, not because they share a PeerId." GossipSub does not loop a
/// publish back to its own node, so without a local fan-out this case —
/// the one Model B is built around — silently delivered nothing.
///
/// The publishing session is deliberately excluded, and that is asserted
/// too: it already has the message, and echoing it back would make every
/// client filter its own traffic.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_local_sessions_on_one_profile_receive_each_others_broadcasts() {
    let (a_id, _) = who();
    let a = node(&a_id, &[], &["general"], SubstrateConfig::default()).await;

    for s in ["human", "claude"] {
        a.join(channel("general"), s)
            .await
            .expect("lands")
            .expect("accepted");
    }

    a.publish(channel("general"), "human", envelope(4, b"from the human"))
        .await
        .expect("the command lands")
        .expect("accepted locally");

    let at_claude = drain_until(&a, "claude", PATIENCE).await;
    assert_eq!(
        at_claude.len(),
        1,
        "the other local session receives it, with no network involved"
    );
    assert_eq!(at_claude[0].payload.bytes(), b"from the human");
    assert_eq!(
        at_claude[0].channel,
        channel("general"),
        "under the channel it was published on"
    );

    assert!(
        drain_until(&a, "human", Duration::from_millis(500))
            .await
            .is_empty(),
        "the publishing session is not handed back its own message"
    );

    a.shutdown().await.expect("a stops");
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
    publish_repeatedly_on(publisher, "general", session, id, body).await;
}

/// The same, on a named channel.
async fn publish_repeatedly_on(
    publisher: &SwarmRuntime,
    channel_name: &str,
    session: &str,
    id: u8,
    body: &[u8],
) {
    for _ in 0..12 {
        let _ = publisher
            .publish(channel(channel_name), session, envelope(id, body))
            .await
            .expect("the command lands");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
