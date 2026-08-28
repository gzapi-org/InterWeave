// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Direct ingress rate limiting, between real peers.
//!
//! The last of the plan's nine required network tests. These buckets run
//! *after* Noise and trust admission, so what they bound is a peer that
//! is authorized and misbehaving — the case a trust check cannot reach.
//!
//! # The queue is not allowed to be the one refusing
//!
//! A full endpoint queue and an exhausted bucket both answer `overloaded`
//! on the wire, deliberately: the sender learns it was refused, not which
//! resource it exhausted. That makes them indistinguishable to a test
//! too, so every receiver here is configured with a queue bound far above
//! the burst, and each test drains the queue to confirm it accepted every
//! message that was accepted. A refusal from a queue that was never full
//! is a refusal from the limiter.
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
    DirectEndpoints, SubstrateConfig, SwarmEvent, SwarmRuntime,
};
use interweave_transport_runtime::TrustSources;
use interweave_trust_api::EndpointTrustPolicy;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};

/// The contract default, restated so a change to it fails here rather
/// than silently making these tests measure nothing.
const PER_PEER_BURST: u8 = 32;

/// Comfortably above the burst: the queue must never be the refusing
/// party, or every assertion below would hold for the wrong reason.
const QUEUE_BOUND: usize = 512;

/// How many distinct source endpoint names the flooding peer holds.
///
/// `MAX_ENDPOINTS` is 64 and two of them are `human` and `claude`, so
/// this is what is left. It only has to be comfortably ABOVE the
/// per-peer burst — the claim under test is that distinct source names
/// buy no extra allowance, and 62 names against a burst of 32 says that
/// as well as 640 would.
const INVENTED_SOURCES: u8 = 62;

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

/// Claim each named endpoint for a session of the same name.
///
/// What Stage 6 did implicitly at `configure_direct`, done explicitly:
/// a session sends AS the endpoint it holds, so these tests name sessions
/// after endpoints and the lease is the only thing that binds the two.
async fn claim_all(runtime: &SwarmRuntime, names: &[&str]) {
    for name in names {
        runtime
            .claim_endpoint(*name, endpoint(name), "in-process")
            .await
            .expect("the claim reaches the task")
            .expect("the endpoint is configured and free");
    }
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

fn endpoints() -> DirectEndpoints {
    DirectEndpoints::from_profile(
        &profile_with(vec![entry("human"), entry("claude")], Some("human")),
        QUEUE_BOUND,
    )
    .expect("a valid profile")
}

/// The SENDER's endpoints, which are not the receiver's.
///
/// A source endpoint must name a lease the sending node holds, so a
/// sender that will flood under sixty-four invented names has to hold
/// sixty-four leases. That is a local control and changes nothing about
/// what is under test: the receiver sees sixty-four distinct
/// peer-asserted source endpoints arriving from one authenticated peer,
/// which is exactly what it would see from an attacker running its own
/// software and holding no leases at all.
fn sender_endpoints() -> DirectEndpoints {
    let mut entries = vec![entry("human"), entry("claude")];
    for id in 0..INVENTED_SOURCES {
        entries.push(entry(&format!("source-{id}")));
    }
    DirectEndpoints::from_profile(&profile_with(entries, Some("human")), QUEUE_BOUND)
        .expect("a valid profile")
}

/// Distinct `id` per call, because a repeated message id is a DUPLICATE
/// and would be accepted without ever reaching the queue — which would
/// break the drain count these tests rely on.
fn frame(source: &str, id: u8) -> DirectMessageV2 {
    DirectMessageV2 {
        message_id: MessageId::from_bytes([id; 16]),
        sent_at_ms: 1,
        source_endpoint: endpoint(source),
        destination_endpoint: Some(endpoint("claude")),
        payload: Payload::at_ceiling(
            Some(MediaType::parse("text/plain").expect("valid media type")),
            b"flood".to_vec(),
        )
        .expect("within the ceiling"),
    }
}

async fn start(id: &ProfileIdentity, trust: TrustSources) -> SwarmRuntime {
    SwarmRuntime::start(id, SubstrateConfig::default(), trust).expect("the runtime starts")
}

/// A receiver plus `senders` peers already connected to it.
async fn fan_in(senders: usize) -> (Vec<SwarmRuntime>, SwarmRuntime, TransportIdentity) {
    let sending: Vec<(ProfileIdentity, TransportIdentity)> = (0..senders).map(|_| who()).collect();
    let (receiver_id, receiver_peer) = who();

    let mut receiver = start(
        &receiver_id,
        trusting(&sending.iter().map(|(_, p)| p).collect::<Vec<_>>()),
    )
    .await;
    receiver
        .configure_direct(endpoints())
        .await
        .expect("endpoints install");
    claim_all(&receiver, &["human", "claude"]).await;
    let address = receiver
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("loopback"))
        .await
        .expect("listens");

    let mut runtimes = Vec::with_capacity(senders);
    for (id, _) in &sending {
        let sender = start(id, trusting(&[&receiver_peer])).await;
        sender
            .configure_direct(sender_endpoints())
            .await
            .expect("the sender's own endpoints install");
        // Sixty-four leases means sixty-four sessions, each named for the
        // endpoint it holds: a session sends AS its lease and nothing else.
        let mut names = vec!["human".to_owned(), "claude".to_owned()];
        names.extend((0..INVENTED_SOURCES).map(|id| format!("source-{id}")));
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        claim_all(&sender, &names).await;
        sender
            .dial(receiver_peer.clone(), address.clone())
            .await
            .expect("command")
            .expect("admitted");
        wait_connected(&mut receiver).await;
        runtimes.push(sender);
    }

    (runtimes, receiver, receiver_peer)
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

/// Send `count` frames, each with its own message id, and report every
/// answer in order. `source` names the source endpoint per index.
async fn flood(
    sender: &SwarmRuntime,
    peer: &TransportIdentity,
    count: u8,
    source: impl Fn(u8) -> String,
) -> Vec<Result<EndpointId, TransportError>> {
    let mut answers = Vec::with_capacity(usize::from(count));
    for id in 0..count {
        answers.push(
            sender
                .send_direct(source(id), peer.clone(), frame(&source(id), id))
                .await
                .expect("the command reaches the task"),
        );
    }
    answers
}

fn accepted(answers: &[Result<EndpointId, TransportError>]) -> usize {
    answers.iter().filter(|a| a.is_ok()).count()
}

/// Every refusal must be `Overloaded` and nothing else — a rate-limited
/// peer learning `no_route` would be told something false about the
/// endpoint, and a different code would be a different defect.
fn assert_only_overloaded(answers: &[Result<EndpointId, TransportError>]) {
    for answer in answers {
        if let Err(error) = answer {
            assert!(
                matches!(error, TransportError::Overloaded),
                "a refusal under flood is Overloaded, got {error:?}"
            );
        }
    }
}

/// The burst is spendable in full, and past it the peer is refused.
///
/// Twice the burst is sent. Refill cannot rescue the second half: at 120
/// tokens per minute, earning back the 32 the burst just spent would take
/// sixteen seconds, and these sends complete in milliseconds.
#[tokio::test]
async fn a_trusted_peer_is_refused_once_its_burst_is_spent() {
    let (senders, receiver, peer) = fan_in(1).await;
    let answers = flood(&senders[0], &peer, PER_PEER_BURST * 2, |_| "human".into()).await;

    let allowed = accepted(&answers);
    assert!(
        allowed >= usize::from(PER_PEER_BURST),
        "the whole burst is spendable, got {allowed}"
    );
    assert!(
        allowed < answers.len(),
        "and past it the peer is refused; every one of {} was accepted",
        answers.len()
    );
    assert_only_overloaded(&answers);

    // THE QUEUE WAS NOT THE ONE REFUSING. It holds every accepted
    // message and never reached its bound, so the refusals came from the
    // limiter.
    let delivered = receiver
        .drain_endpoint(endpoint("claude"))
        .await
        .expect("the receiver answers");
    assert_eq!(
        delivered.len(),
        allowed,
        "the queue took exactly what was accepted"
    );
    assert!(delivered.len() < QUEUE_BOUND, "and was never full");
}

/// Inventing source endpoint names does not multiply the allowance.
///
/// `ingress.rs` says the source EndpointId is deliberately not a bucket
/// dimension: it is peer-asserted, so keying on it would let one peer
/// mint allowance by naming endpoints, and would make endpoint names an
/// unbounded metric label besides. This is that sentence's test — every
/// frame here carries a source endpoint no other frame used.
#[tokio::test]
async fn a_peer_cannot_mint_allowance_by_inventing_source_endpoints() {
    let (senders, receiver, peer) = fan_in(1).await;
    let answers = flood(&senders[0], &peer, PER_PEER_BURST * 2, |id| {
        format!("source-{}", id % INVENTED_SOURCES)
    })
    .await;

    let allowed = accepted(&answers);
    assert!(
        allowed < answers.len(),
        "{INVENTED_SOURCES} distinct source endpoints bought no extra allowance"
    );
    assert!(
        allowed <= usize::from(PER_PEER_BURST) + 1,
        "and bought no MORE than the one bucket's worth, got {allowed}"
    );
    assert_only_overloaded(&answers);

    let delivered = receiver
        .drain_endpoint(endpoint("claude"))
        .await
        .expect("the receiver answers");
    assert_eq!(delivered.len(), allowed, "the queue was not the refuser");
}

/// One peer's flood leaves another peer's allowance untouched.
///
/// The buckets are per-peer before they are global, so a misbehaving peer
/// cannot deny service to a well-behaved one. The global burst is 256 and
/// the flood below spends 64, so nothing here is near the shared bound —
/// if the quiet peer is refused, the keying is wrong.
#[tokio::test]
async fn a_flooding_peer_does_not_spend_a_quiet_peers_allowance() {
    let (senders, receiver, peer) = fan_in(2).await;

    let flooded = flood(&senders[0], &peer, PER_PEER_BURST * 2, |_| "human".into()).await;
    assert!(
        accepted(&flooded) < flooded.len(),
        "the loud peer was refused"
    );

    // The quiet peer has spent nothing and is owed its own full burst.
    let quiet = flood(&senders[1], &peer, PER_PEER_BURST, |_| "human".into()).await;
    assert_eq!(
        accepted(&quiet),
        usize::from(PER_PEER_BURST),
        "the quiet peer keeps its whole allowance"
    );

    let delivered = receiver
        .drain_endpoint(endpoint("claude"))
        .await
        .expect("the receiver answers");
    assert_eq!(
        delivered.len(),
        accepted(&flooded) + accepted(&quiet),
        "both peers' accepted messages are queued, and nothing else"
    );
}

/// The GLOBAL bucket bounds the aggregate, with every peer inside its own
/// allowance.
///
/// The other tests here spend 64 and 96 against a global burst of 256, so
/// none of them can tell whether the shared bucket exists at all — a
/// wiring or configuration regression that disabled it would pass every
/// one of them. This is the case that cannot be explained by per-peer
/// accounting: sixteen peers each spend exactly their own 32-token burst,
/// so no per-peer bucket refuses anything, and the 512 attempts still
/// have to be cut down by the shared one.
///
/// The margin is deliberately wide rather than exact. The bucket refills
/// while the test runs — 1,200 a minute is 20 a second — so an assertion
/// pinned to 256 lands within a token or two of the boundary and decides
/// on timing: the first version of this test asserted `<= 256 + refill`
/// and failed at 267 against a ceiling of 266.6. Sixteen senders put the
/// accepted total near 256 and the attempted total at 512, and no
/// plausible refill closes that gap.
#[tokio::test]
async fn the_global_bucket_bounds_peers_that_are_each_within_their_own() {
    const SENDERS: usize = 16;
    let (senders, receiver, peer) = fan_in(SENDERS).await;

    let mut accepted_total = 0usize;
    let mut refusals = 0usize;
    for sender in &senders {
        let answers = flood(sender, &peer, PER_PEER_BURST, |_| "human".into()).await;
        // EVERY refusal must be the limiter, as the sibling tests
        // require. Counting bare `Err` would let a transport fault or a
        // queue-capacity regression supply the refusals this test reads
        // as proof of the shared bucket — and both assertions below
        // would then pass with the global limiter disabled.
        assert_only_overloaded(&answers);
        accepted_total += accepted(&answers);
        refusals += answers.len() - accepted(&answers);
    }

    let attempted = SENDERS * usize::from(PER_PEER_BURST);
    assert_eq!(attempted, 512, "sixteen peers at their own burst");
    assert!(
        refusals > 0,
        "every peer stayed inside its own allowance, so a refusal can only \
         have come from the shared bucket — none arrived, which means the \
         global bound is not wired"
    );
    assert!(
        accepted_total < attempted * 3 / 4,
        "the shared bucket must cut the aggregate well below {attempted}; \
         {accepted_total} were accepted"
    );

    // And the queue was not the refuser: everything accepted was
    // enqueued, so the shortfall is the limiter's doing and nothing
    // else's.
    let delivered = receiver
        .drain_endpoint(endpoint("claude"))
        .await
        .expect("the receiver answers");
    assert_eq!(
        delivered.len(),
        accepted_total,
        "the queue took exactly what was accepted"
    );
}
