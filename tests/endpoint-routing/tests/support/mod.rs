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
type Leases = std::collections::BTreeMap<String, interweave_local_client_api::EndpointLease>;

pub(crate) async fn claim_all(runtime: &SwarmRuntime, names: &[&str]) -> Leases {
    let mut leases = Leases::new();
    for name in names {
        let lease = runtime
            .claim_endpoint(*name, endpoint(name), "in-process")
            .await
            .expect("the claim reaches the task")
            .expect("the endpoint is configured and free");
        leases.insert((*name).to_owned(), lease);
    }
    leases
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

pub(crate) async fn connected_pair() -> (SwarmRuntime, SwarmRuntime, TransportIdentity, Leases) {
    connected_pair_claiming(&["human", "claude", "gpt-5"], &["human", "claude", "gpt-5"]).await
}

/// The same pair, with each side claiming only the endpoints named —
/// so a test can leave one unleased and prove what that means.
pub(crate) async fn connected_pair_claiming(
    sender_claims: &[&str],
    receiver_claims: &[&str],
) -> (SwarmRuntime, SwarmRuntime, TransportIdentity, Leases) {
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
    let leases = claim_all(&sender, sender_claims).await;

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

    (sender, receiver, receiver_peer, leases)
}

/// An advertised endpoint: `advertise: true`, otherwise default.
pub(crate) fn advertised(name: &str) -> EndpointConfig {
    EndpointConfig {
        advertise: true,
        ..entry(name)
    }
}

/// An advertised endpoint whose inbound policy admits only `only`.
pub(crate) fn advertised_to(name: &str, only: &TransportIdentity) -> EndpointConfig {
    EndpointConfig {
        inbound: EndpointTrustPolicy::StaticSubset {
            allowed_peers: [only.clone()].into_iter().collect(),
        },
        ..advertised(name)
    }
}

/// A profile that trusts `peers` and configures `entries`, directory on.
///
/// The endpoint subsets can only narrow to peers the PROFILE trusts, so a
/// test that excludes one endpoint from a peer must list that peer here.
pub(crate) fn profile_trusting(
    peers: &[&TransportIdentity],
    entries: Vec<EndpointConfig>,
    default: Option<&str>,
) -> ProfileConfig {
    let mut profile = profile_directory(entries, default, true);
    profile.trust.allowed_peers = peers.iter().map(|p| (*p).clone()).collect();
    profile
}

/// A profile whose directory is enabled or not.
pub(crate) fn profile_directory(
    entries: Vec<EndpointConfig>,
    default: Option<&str>,
    directory_enabled: bool,
) -> ProfileConfig {
    let mut profile = profile_with(entries, default);
    profile.endpoints.directory = DirectoryConfig {
        enabled: directory_enabled,
        max_advertised: interweave_profile_config::MAX_ADVERTISED_CEILING,
        ..DirectoryConfig::default()
    };
    profile
}

/// Two connected runtimes where the RESPONDER (second) is configured from
/// `responder_profile` and both sides trust each other data-plane.
/// `responder_sessions` names the endpoints the responder claims.
///
/// Both are data-plane trusted because an infrastructure-only peer cannot
/// hold an inbound connection at this stage — the socket closes before a
/// request (see `tests/direct-v2` malformed-frames). The responder's own
/// `Unauthorized` refusal arm is therefore not reachable end to end here,
/// which the Met. block records as a stated limit; the reachable guard is
/// the querier-side local refusal, tested directly.
pub(crate) async fn connected_for_directory(
    responder_profile: ProfileConfig,
    responder_sessions: &[(&str, &str)],
) -> (SwarmRuntime, SwarmRuntime, TransportIdentity) {
    let (querier_id, querier_peer) = who();
    let (responder_id, _responder_peer) = who();

    let mut responder = SwarmRuntime::start(
        &responder_id,
        SubstrateConfig::default(),
        trusting(&[&querier_peer]),
    )
    .expect("the responder starts");
    let responder_peer = responder.local_peer().clone();
    let querier = SwarmRuntime::start(
        &querier_id,
        SubstrateConfig::default(),
        trusting(&[&responder_peer]),
    )
    .expect("the querier starts");

    responder
        .configure_direct(
            DirectEndpoints::from_profile(&responder_profile, 8).expect("a valid profile"),
        )
        .await
        .expect("the responder's endpoints install");
    // The querier configures a `human` endpoint of its own, so a test
    // that wants to send after querying has a source to claim. It leaves
    // the directory at defaults; only the responder's is under test.
    querier
        .configure_direct(
            DirectEndpoints::from_profile(&profile_with(vec![entry("human")], Some("human")), 8)
                .expect("a valid querier profile"),
        )
        .await
        .expect("the querier's endpoint installs");
    for (session, name) in responder_sessions {
        responder
            .claim_endpoint(*session, endpoint(name), "in-process")
            .await
            .expect("the claim reaches the task")
            .expect("the endpoint is configured and free");
    }

    let address = responder
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("loopback"))
        .await
        .expect("listens");
    querier
        .dial(responder_peer.clone(), address)
        .await
        .expect("command")
        .expect("admitted");
    wait_connected(&mut responder).await;

    (querier, responder, responder_peer)
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
