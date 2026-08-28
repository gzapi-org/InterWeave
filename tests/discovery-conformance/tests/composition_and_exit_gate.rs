// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Stage 9 exit gate: the providers compose, and discovery cannot
//! bypass trust or ConnectionManager.
//!
//! Two halves. Composition is pure and runs in microseconds: three real
//! providers registered with a real `DiscoveryManager`, their events
//! merged, provenance kept. The no-bypass half runs over real sockets,
//! because "cannot bypass" is a claim about what the transport does with
//! a candidate — and a mock would prove only that the mock agrees.
#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_discovery_api::{DiscoveryEvent, DiscoveryProvider, ProviderHealth};
use interweave_discovery_cache::{CacheLimits, PeerCache, PeerCacheDiscovery};
use interweave_discovery_mdns::MdnsDiscovery;
use interweave_discovery_static::{StaticBootstrapDiscovery, StaticEntry};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::runtime::{SubstrateConfig, SwarmEvent, SwarmRuntime};
use interweave_transport_runtime::{DiscoveryManager, TrustSources};
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};

const P1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
const P2: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";

fn peer(s: &str) -> TransportIdentity {
    TransportIdentity::parse(s).expect("valid identity")
}

fn nobody() -> PeerTrustPolicy {
    PeerTrustPolicy::new(Vec::new()).expect("policy")
}

/// Drain a provider into a manager, the way a composed runtime would.
fn pump(
    manager: &mut DiscoveryManager,
    provider: &mut dyn DiscoveryProvider,
    now_ms: u64,
    trust: &PeerTrustPolicy,
) {
    let source = provider.descriptor().name;
    for event in provider.drain_events(now_ms, 64) {
        manager
            .on_event(&source, event, now_ms, trust)
            .expect("a conforming provider's own events are accepted");
    }
}

#[test]
fn the_three_providers_compose_into_one_candidate_set() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut manager = DiscoveryManager::new();

    // The cache knows P1 from a previous run.
    let cache = PeerCache::load(&dir.path().join("peers.json"), CacheLimits::default())
        .expect("empty cache");
    let mut cache_provider = PeerCacheDiscovery::new(cache);
    cache_provider
        .cache_mut()
        .record_success(&peer(P1), "/ip4/10.0.0.1/tcp/4001", 0)
        .expect("within bounds");

    // Configuration names P1 too, at a different address, and P2.
    let mut static_provider = StaticBootstrapDiscovery::new(vec![
        StaticEntry::new(peer(P1), "/dns4/host.example/tcp/4001").expect("within bounds"),
        StaticEntry::new(peer(P2), "/ip4/10.0.0.2/tcp/4001").expect("within bounds"),
    ])
    .expect("within bounds");

    // The LAN sees P1 at a third address.
    let mut mdns_provider = MdnsDiscovery::new();

    for (descriptor, priority) in [
        (cache_provider.descriptor(), 10),
        (static_provider.descriptor(), 30),
        (mdns_provider.descriptor(), 20),
    ] {
        manager.register(descriptor, priority).expect("registers");
    }
    assert_eq!(manager.provider_count(), 3);

    cache_provider.start(0).expect("starts");
    static_provider.start(0).expect("starts");
    mdns_provider.start(0).expect("starts");
    mdns_provider.push_discovered(P1, "/ip4/192.168.1.5/tcp/4001", 0);

    pump(&mut manager, &mut cache_provider, 0, &nobody());
    pump(&mut manager, &mut static_provider, 0, &nobody());
    pump(&mut manager, &mut mdns_provider, 0, &nobody());

    let candidates = manager.candidates(0);
    assert_eq!(candidates.len(), 2, "two peers, not five observations");

    let p1 = candidates
        .iter()
        .find(|c| c.peer_id == peer(P1))
        .expect("P1 is a candidate");
    assert_eq!(
        p1.addresses.len(),
        3,
        "one peer, three addresses, merged across providers"
    );
    assert_eq!(
        p1.sources.len(),
        3,
        "and every provider's provenance is kept, not collapsed"
    );
}

#[test]
fn a_candidate_survives_one_providers_retraction_when_another_still_vouches() {
    // COMPOSITION.md's central rule, end to end across two real
    // providers: an address dies when no live source supports it, not
    // when the first source lets go.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut manager = DiscoveryManager::new();

    let cache = PeerCache::load(&dir.path().join("peers.json"), CacheLimits::default())
        .expect("empty cache");
    let mut cache_provider = PeerCacheDiscovery::new(cache);
    cache_provider
        .cache_mut()
        .record_success(&peer(P1), "/ip4/10.0.0.1/tcp/4001", 0)
        .expect("within bounds");
    let mut static_provider = StaticBootstrapDiscovery::new(vec![
        StaticEntry::new(peer(P1), "/ip4/10.0.0.1/tcp/4001").expect("within bounds"),
    ])
    .expect("within bounds");

    manager
        .register(cache_provider.descriptor(), 10)
        .expect("registers");
    manager
        .register(static_provider.descriptor(), 30)
        .expect("registers");
    cache_provider.start(0).expect("starts");
    static_provider.start(0).expect("starts");
    pump(&mut manager, &mut cache_provider, 0, &nobody());
    pump(&mut manager, &mut static_provider, 0, &nobody());

    assert_eq!(manager.candidates(0)[0].sources.len(), 2);

    // The operator removes the configured entry. The cache still vouches.
    static_provider
        .set_entries(Vec::new(), 10)
        .expect("within bounds");
    pump(&mut manager, &mut static_provider, 10, &nobody());

    let after = manager.candidates(10);
    assert_eq!(after.len(), 1, "the candidate survives");
    assert_eq!(
        after[0].sources,
        ["peer-cache".to_owned()].into_iter().collect(),
        "with only the source that still supports it"
    );
}

#[test]
fn aggregate_health_survives_one_degraded_provider() {
    let mut manager = DiscoveryManager::new();
    let mut mdns_provider = MdnsDiscovery::new();
    let static_provider = StaticBootstrapDiscovery::new(Vec::new()).expect("empty is valid");
    manager
        .register(mdns_provider.descriptor(), 20)
        .expect("registers");
    manager
        .register(static_provider.descriptor(), 30)
        .expect("registers");

    mdns_provider.start(0).expect("starts");
    // A container without multicast routing: the normal case.
    mdns_provider.report_backend_down(1);
    pump(&mut manager, &mut mdns_provider, 1, &nobody());
    assert_eq!(
        manager.provider_health("mdns"),
        Some(ProviderHealth::Degraded)
    );

    // Static reports healthy; discovery as a whole is working.
    manager
        .on_event(
            "static-bootstrap",
            DiscoveryEvent::HealthChanged {
                source: "static-bootstrap".to_owned(),
                health: ProviderHealth::Healthy,
            },
            1,
            &nobody(),
        )
        .expect("accepted");
    assert_eq!(
        manager.aggregate_health(),
        ProviderHealth::Healthy,
        "one degraded provider does not make the node look broken"
    );
}

#[test]
fn one_provider_cannot_speak_for_another() {
    // Provenance across real providers: the manager refuses an event
    // whose source is not the emitting provider's registered name, so a
    // provider cannot launder a candidate's origin.
    let mut manager = DiscoveryManager::new();
    let static_provider = StaticBootstrapDiscovery::new(Vec::new()).expect("empty is valid");
    let mdns_provider = MdnsDiscovery::new();
    manager
        .register(static_provider.descriptor(), 30)
        .expect("registers");
    manager
        .register(mdns_provider.descriptor(), 20)
        .expect("registers");

    let forged = DiscoveryEvent::CandidateObserved {
        candidate: Box::new(interweave_discovery_api::CandidatePeer {
            peer_id: peer(P1),
            addresses: ["/ip4/10.0.0.1/tcp/4001".to_owned()].into_iter().collect(),
            // mDNS's name, emitted by static-bootstrap.
            source: "mdns".to_owned(),
            observed_at: 0,
            expires_at: None,
            protocol_observations: std::collections::BTreeSet::new(),
        }),
    };
    assert!(
        manager
            .on_event("static-bootstrap", forged, 0, &nobody())
            .is_err(),
        "a provider cannot stamp another's name"
    );
    assert!(manager.candidates(0).is_empty(), "and nothing was recorded");
}

// --- the exit gate, over real sockets ----------------------------------

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

async fn wait_connected(runtime: &mut SwarmRuntime) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match tokio::time::timeout(remaining, runtime.next_event()).await {
            Ok(Some(SwarmEvent::Connected { .. })) => return true,
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return false,
        }
    }
}

/// THE EXIT GATE. A discovered candidate for an UNTRUSTED peer cannot
/// produce a connection, while the identical flow for a trusted peer
/// does. Discovery has no privileged entrance: the candidate reaches the
/// transport through the same `add_address` any caller uses, and the dial
/// still passes admission.
#[tokio::test]
async fn a_discovered_candidate_cannot_bypass_trust_or_the_connection_manager() {
    let (listener_id, listener_peer) = who();
    let (dialer_id, dialer_peer) = who();

    // The listener is real and reachable.
    let listener = SwarmRuntime::start(
        &listener_id,
        SubstrateConfig::default(),
        trusting(&[&dialer_peer]),
    )
    .expect("the listener starts");
    let address = listener
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("loopback"))
        .await
        .expect("listens");

    // A node that trusts NOBODY. Discovery is about to hand it a
    // perfectly good candidate for the listener.
    let untrusting =
        SwarmRuntime::start(&dialer_id, SubstrateConfig::default(), trusting(&[])).expect("starts");

    // Compose the candidate exactly as a provider would produce it.
    let mut manager = DiscoveryManager::new();
    let mut provider = StaticBootstrapDiscovery::new(vec![
        StaticEntry::new(listener_peer.clone(), address.to_string()).expect("within bounds"),
    ])
    .expect("within bounds");
    manager
        .register(provider.descriptor(), 30)
        .expect("registers");
    provider.start(0).expect("starts");
    pump(&mut manager, &mut provider, 0, &nobody());

    let candidate = manager
        .candidates(0)
        .into_iter()
        .find(|c| c.peer_id == listener_peer)
        .expect("discovery produced the candidate");
    let discovered: libp2p::Multiaddr = candidate
        .addresses
        .iter()
        .next()
        .expect("an address")
        .parse()
        .expect("the address round-trips");

    // THE ADDRESS BOOK REFUSES IT. `learn_address` is keyed by trust
    // class: an unclassified peer gets no entry, which is what stops an
    // address book from being a map an unauthorized party grows.
    let remembered = untrusting
        .add_address(listener_peer.clone(), discovered.clone())
        .await
        .expect("the command reaches the task");
    assert!(
        !remembered,
        "an untrusted peer's discovered address is not even remembered"
    );

    // AND THE DIAL REFUSES IT. Nothing to dial, because nothing was
    // remembered — discovery did not create a side door.
    let refusal = untrusting
        .dial_peer(listener_peer.clone())
        .await
        .expect("the command reaches the task")
        .expect_err("an untrusted peer is not dialable from a candidate");
    let _ = refusal;

    // POSITIVE CONTROL: the same candidate, the same flow, a node that
    // trusts the listener. If this did not connect, the assertions above
    // would prove only that the test setup was broken.
    let (trusting_id, trusting_peer) = who();
    let mut listener2 = SwarmRuntime::start(
        &listener_id2(),
        SubstrateConfig::default(),
        trusting(&[&trusting_peer]),
    )
    .expect("starts");
    let address2 = listener2
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("loopback"))
        .await
        .expect("listens");
    let listener2_peer = listener2.local_peer().clone();

    let truster = SwarmRuntime::start(
        &trusting_id,
        SubstrateConfig::default(),
        trusting(&[&listener2_peer]),
    )
    .expect("starts");

    let mut provider2 = StaticBootstrapDiscovery::new(vec![
        StaticEntry::new(listener2_peer.clone(), address2.to_string()).expect("within bounds"),
    ])
    .expect("within bounds");
    provider2.start(0).expect("starts");
    let mut manager2 = DiscoveryManager::new();
    manager2
        .register(provider2.descriptor(), 30)
        .expect("registers");
    pump(&mut manager2, &mut provider2, 0, &nobody());
    let candidate2 = manager2
        .candidates(0)
        .into_iter()
        .find(|c| c.peer_id == listener2_peer)
        .expect("discovery produced it");
    let discovered2: libp2p::Multiaddr = candidate2
        .addresses
        .iter()
        .next()
        .expect("an address")
        .parse()
        .expect("parses");

    assert!(
        truster
            .add_address(listener2_peer.clone(), discovered2)
            .await
            .expect("command"),
        "a trusted peer's discovered address IS remembered"
    );
    truster
        .dial_peer(listener2_peer)
        .await
        .expect("command")
        .expect("and a trusted peer is dialable from a discovered candidate");
    assert!(
        wait_connected(&mut listener2).await,
        "the connection really happened — the refusals above are about trust, not plumbing"
    );
    let _ = listener;
}

/// A second identity for the positive control's listener.
fn listener_id2() -> ProfileIdentity {
    ProfileIdentity::generate()
}
