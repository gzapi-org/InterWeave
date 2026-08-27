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
