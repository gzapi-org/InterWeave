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

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{
    DirectMessageV2, EndpointId, MediaType, MessageId, Payload, TransportError, TransportIdentity,
};
use interweave_transport_libp2p::runtime::{
    DirectEndpoints, SubstrateConfig, SwarmEvent, SwarmRuntime,
};
use interweave_transport_runtime::{Generation, TrustSources};
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

fn endpoints() -> DirectEndpoints {
    DirectEndpoints {
        endpoints: vec![endpoint("human"), endpoint("claude")],
        default: Some(endpoint("human")),
        queue_bound: QUEUE_BOUND,
        epoch: Generation::parse("ingress_________").expect("valid generation"),
    }
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
    let mut endpoints = vec![endpoint("human"), endpoint("claude")];
    for id in 0..INVENTED_SOURCES {
        endpoints.push(endpoint(&format!("source-{id}")));
    }
    DirectEndpoints {
        endpoints,
        default: Some(endpoint("human")),
        queue_bound: QUEUE_BOUND,
        epoch: Generation::parse("ingress_________").expect("valid generation"),
    }
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
                .send_direct(peer.clone(), frame(&source(id), id))
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
