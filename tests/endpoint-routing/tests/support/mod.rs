// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Helpers shared by the endpoint-routing suites: two connected runtimes
//! on one profile shape, and the leases that make them routable.
//!
//! Lifted out of the Model-B suite when Stage 8 added a second file,
//! rather than copied into it — two copies of `connected_pair` would be
//! two definitions of what "connected" means.
#![allow(dead_code, clippy::expect_used, clippy::panic)]

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
use interweave_transport_runtime::TrustSources;
use interweave_trust_api::EndpointTrustPolicy;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};

pub(crate) fn who() -> (ProfileIdentity, TransportIdentity) {
    let id = ProfileIdentity::generate();
    let peer = id.transport_identity().expect("peer id");
    (id, peer)
}

pub(crate) fn trusting(peers: &[&TransportIdentity]) -> TrustSources {
    TrustSources::new(
        PeerTrustPolicy::new(peers.iter().map(|p| (*p).clone())).expect("a handful"),
        InfrastructureSet::default(),
    )
}

pub(crate) fn endpoint(name: &str) -> EndpointId {
    EndpointId::parse(name).expect("valid endpoint id")
}

/// Claim each named endpoint for a session of the same name.
///
/// What Stage 6 did implicitly at `configure_direct`, done explicitly:
/// a session sends AS the endpoint it holds, so these tests name sessions
/// after endpoints and the lease is the only thing that binds the two.
pub(crate) async fn claim_all(runtime: &SwarmRuntime, names: &[&str]) {
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
pub(crate) fn profile_with(entries: Vec<EndpointConfig>, default: Option<&str>) -> ProfileConfig {
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
pub(crate) fn entry(name: &str) -> EndpointConfig {
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
pub(crate) fn endpoints() -> DirectEndpoints {
    DirectEndpoints::from_profile(
        &profile_with(
            vec![entry("human"), entry("claude"), entry("gpt-5")],
            Some("human"),
        ),
        8,
    )
    .expect("a valid profile")
}

/// A frame from `from`, to `to`, carrying `body` under `id`.
pub(crate) fn frame(from: &str, to: Option<&str>, body: &[u8], id: u8) -> DirectMessageV2 {
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

pub(crate) async fn connected_pair() -> (SwarmRuntime, SwarmRuntime, TransportIdentity) {
    connected_pair_claiming(&["human", "claude", "gpt-5"], &["human", "claude", "gpt-5"]).await
}

/// The same pair, with each side claiming only the endpoints named —
/// so a test can leave one unleased and prove what that means.
pub(crate) async fn connected_pair_claiming(
    sender_claims: &[&str],
    receiver_claims: &[&str],
) -> (SwarmRuntime, SwarmRuntime, TransportIdentity) {
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
    claim_all(&sender, sender_claims).await;

    receiver
        .configure_direct(endpoints())
        .await
        .expect("endpoints install");
    claim_all(&receiver, receiver_claims).await;
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
pub(crate) async fn wait_connected(runtime: &mut SwarmRuntime) {
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
