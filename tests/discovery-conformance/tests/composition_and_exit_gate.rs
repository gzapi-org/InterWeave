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
fn a_long_running_node_keeps_its_configured_and_announcing_candidates() {
    // The liveness property across all three providers, composed: a node
    // that runs for hours must still hold the peers it was configured
    // with, the ones its cache keeps succeeding against, and the ones the
    // LAN keeps announcing. Each provider refreshes the manager
    // differently, and every one of them got this wrong first time.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut manager = DiscoveryManager::new();

    let cache = PeerCache::load(&dir.path().join("peers.json"), CacheLimits::default())
        .expect("empty cache");
    let mut cache_provider = PeerCacheDiscovery::new(cache);
    let mut static_provider = StaticBootstrapDiscovery::new(vec![
        StaticEntry::new(peer(P1), "/dns4/bootstrap.example/tcp/4001").expect("within bounds"),
    ])
    .expect("within bounds");
    let mut mdns_provider = MdnsDiscovery::new();

    for d in [
        cache_provider.descriptor(),
        static_provider.descriptor(),
        mdns_provider.descriptor(),
    ] {
        manager.register(d, 10).expect("registers");
    }
    cache_provider.start(0).expect("starts");
    static_provider.start(0).expect("starts");
    mdns_provider.start(0).expect("starts");

    // TEN DAYS, in ten-minute steps. The window has to outlast the
    // LONGEST lifetime in play or it cannot see the bug it is for: the
    // cache's own TTL is seven days, so a four-hour run proved the static
    // and mDNS halves and silently skipped the cache one. Ten-minute
    // steps keep it fast while staying well inside every refresh window.
    let step = 10 * 60 * 1_000;
    let ten_days = 10 * 24 * 60 * 60 * 1_000;
    let mut t = 0u64;
    while t <= ten_days {
        cache_provider.add_hint(
            interweave_discovery_api::PeerHint::ObservedReachable {
                peer_id: peer(P2),
                address: "/ip4/10.0.0.2/tcp/4001".to_owned(),
                observed_at: t,
            },
            t,
        );
        mdns_provider.push_discovered(
            "12D3KooWQYV9dGMFoRzNStwpXztXaBUjtPqi6aU76ZgUriHhKust",
            "/ip4/192.168.1.5/tcp/4001",
            t,
        );
        pump(&mut manager, &mut cache_provider, t, &nobody());
        pump(&mut manager, &mut static_provider, t, &nobody());
        pump(&mut manager, &mut mdns_provider, t, &nobody());
        manager.sweep(t);

        let held: Vec<TransportIdentity> = manager
            .candidates(t)
            .into_iter()
            .map(|c| c.peer_id)
            .collect();
        assert!(
            held.contains(&peer(P1)),
            "the configured bootstrap peer vanished at t={t}"
        );
        assert!(
            held.contains(&peer(P2)),
            "the cache peer that keeps succeeding vanished at t={t}"
        );
        assert_eq!(held.len(), 3, "and the announcing LAN peer too, at t={t}");
        t += step;
    }
}

#[test]
fn starting_a_provider_makes_discovery_healthy_at_the_manager() {
    // The manager registers a provider as Unavailable and learns health
    // only from a HealthChanged event. A provider that becomes healthy
    // internally and says nothing leaves aggregate discovery health
    // permanently Unavailable — a node doing real discovery while
    // reporting it does none.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut manager = DiscoveryManager::new();
    let cache = PeerCache::load(&dir.path().join("peers.json"), CacheLimits::default())
        .expect("empty cache");
    let mut cache_provider = PeerCacheDiscovery::new(cache);
    let mut static_provider = StaticBootstrapDiscovery::new(Vec::new()).expect("empty is valid");

    manager
        .register(cache_provider.descriptor(), 10)
        .expect("registers");
    manager
        .register(static_provider.descriptor(), 30)
        .expect("registers");
    assert_eq!(
        manager.aggregate_health(),
        ProviderHealth::Unavailable,
        "registered but unstarted"
    );

    cache_provider.start(0).expect("starts");
    static_provider.start(0).expect("starts");
    pump(&mut manager, &mut cache_provider, 0, &nobody());
    pump(&mut manager, &mut static_provider, 0, &nobody());

    assert_eq!(
        manager.provider_health("peer-cache"),
        Some(ProviderHealth::Healthy),
        "a started provider reports itself healthy"
    );
    assert_eq!(
        manager.provider_health("static-bootstrap"),
        Some(ProviderHealth::Healthy),
        "each provider reports its own start, so one silent provider is caught"
    );
    assert_eq!(
        manager.aggregate_health(),
        ProviderHealth::Healthy,
        "and discovery as a whole is working"
    );
}

#[test]
fn a_quarantined_cache_reports_degraded_at_start() {
    // The initial transition carries the REAL answer, which is the state
    // a consumer most needs to hear at start rather than never.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("peers.json");
    std::fs::write(&path, b"{ not json").expect("writes");
    let cache = PeerCache::load(&path, CacheLimits::default()).expect("quarantines");
    let mut provider = PeerCacheDiscovery::new(cache);
    let mut manager = DiscoveryManager::new();
    manager
        .register(provider.descriptor(), 10)
        .expect("registers");
    provider.start(0).expect("starts");
    pump(&mut manager, &mut provider, 0, &nobody());
    assert_eq!(
        manager.provider_health("peer-cache"),
        Some(ProviderHealth::Degraded)
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
/// does: whatever door an address comes in by, the dial still passes
/// admission, and admission is where trust is decided.
///
/// A TEST TOPOLOGY, NOT A PATTERN. The candidate is handed to
/// `add_address`, and since ADR-0052 rule 9 that is the OPERATOR'S door:
/// it records the address in the operator set, admitted at every store
/// door whatever its class. Discovery output fed through it is laundered
/// from the peer's door into the operator's, which rule 9 names this
/// test as NOT licensing -- Stage 12's composer owes a peer-door learn
/// command instead (plan §15). What this test proves is the trust
/// half, which does not depend on the door. An earlier version said
/// discovery "has no privileged entrance" here, true before the operator
/// set existed (#111 re-review report 3 P2-3).
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
        .address_list()
        .first()
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
        .address_list()
        .first()
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

/// One mDNS batch never names more peers than the mDNS provider holds at
/// once (CLAUDE.md §7).
///
/// The two constants measure different things -- the DRIVER's bounds one
/// event, the PROVIDER's its whole state across events -- so the relation
/// that matters is `<=`, not the equality an earlier version asserted
/// and called a shared shape (#111 mDNS review F7): a batch larger than
/// the provider's state would be work spent on peers it cannot keep. The
/// ADDRESS bounds are deliberately not related: the driver takes up to
/// `discovery_api::MAX_ADDRESSES` per peer in a batch and the provider
/// keeps `MAX_ADDRESSES_PER_PEER`, dropping the rest under its own bound.
/// This is the one place both crates are visible, so it is asserted here
/// -- at COMPILE time, so this test binary does not build if it breaks.
#[test]
fn a_drivers_batch_names_no_more_peers_than_the_provider_holds() {
    const {
        assert!(
            interweave_transport_libp2p::runtime::mdns_driver::MAX_PEERS_PER_BATCH
                <= interweave_discovery_mdns::MAX_PEERS,
        );
    }
}

/// ADR-0053 rule 2: the vendored crate's record store has the provider's
/// SHAPE -- the same peer bound and the same per-peer address bound -- so,
/// while the crate holds no record the driver's learn-site boundary
/// refuses, it holds none the provider would refuse and evicts none the
/// provider still keeps. A record that boundary refuses (ADR-0052) is
/// held by the crate and never reaches the provider, so it takes a crate
/// slot and no provider one, and with such peers held the crate can
/// evict or refuse an admitted record the provider had room for; until
/// ADR-0053 rule 10's refresh is built, so can a live peer the provider
/// forgot. What this test pins is the equality of the bounds, not those
/// caveats. EQUALITY on both, unlike the batch bound above,
/// and on both because equal in COUNT alone (256 x 8 records of any shape)
/// was the defect: a flood of single-address peers filled the provider at
/// 256 while the crate went on to 2048. Compile-time, so a drift is a
/// build failure.
#[test]
fn the_crates_record_store_has_the_providers_shape() {
    const {
        assert!(
            interweave_transport_libp2p::runtime::mdns_driver::MAX_DISCOVERED_PEERS
                == interweave_discovery_mdns::MAX_PEERS
        );
        assert!(
            interweave_transport_libp2p::runtime::mdns_driver::MAX_ADDRESSES_PER_DISCOVERED_PEER
                == interweave_discovery_mdns::MAX_ADDRESSES_PER_PEER
        );
    }
}

/// ADR-0053 rule 3: the crate clamps an announcer's TTL to the provider's
/// observation TTL, the longest the provider keeps a record whatever the
/// announcer said. A longer clamp serves nothing but a flood; a shorter
/// one would expire what the provider still holds.
#[test]
fn the_crates_ttl_clamp_is_the_providers_observation_ttl() {
    assert_eq!(
        interweave_transport_libp2p::runtime::mdns_driver::MAX_RECORD_TTL,
        std::time::Duration::from_millis(interweave_discovery_mdns::OBSERVATION_TTL_MS),
    );
}
