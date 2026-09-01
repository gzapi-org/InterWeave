// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! Stage 10's third exit-gate clause: a small trusted overlay becomes
//! healthy.
//!
//! **This is the only place the two halves of the port meet.**
//! `KademliaDiscovery` and the Swarm-owned driver each implement
//! `kademlia-control-api`, and each is tested against it — the provider
//! against port events it is handed, the driver against port commands it
//! is given. No production code joins them: `apps/` is empty, and the
//! composition root that would construct a provider beside a runtime is
//! Stage 12.
//!
//! What that leaves untested everywhere else is the TRANSLATION. A
//! driver that emits an event the provider mis-reads, or never emits one
//! the provider waits for, satisfies both unit suites: each side agrees
//! with the port's definition, and neither has ever seen the other's
//! behaviour. Health is the property that needs both — routing
//! admissions come from the driver, the target and the freshness window
//! are the provider's — so it is the assertion that fails if the halves
//! disagree.
//!
//! The harness here is a test, not composition. It pumps the runtime's
//! Kademlia events into the provider and the provider's commands back,
//! which is the loop a composition root will own; nothing about that
//! loop is being proposed as production code.
//!
//! # What this proves, and what it does not
//!
//! It proves the DRIVER-TO-PROVIDER direction: routing admissions and
//! query completions cross the port, and a provider fed by a real
//! driver over real sockets reaches `Healthy` on a small trusted
//! overlay. Stop feeding it and health never arrives — that mutation
//! leaves it Degraded with an empty routing table until the deadline.
//!
//! It does NOT prove that the provider's commands are what convergence
//! depends on. Discard every one of them and the overlay still becomes
//! healthy, because the library's own automatic bootstrap walks the
//! star and finds the third node; the provider observes the result
//! either way. The command direction is exercised and asserted to
//! travel, and is not shown to be necessary — nor can it be here, while
//! libp2p bootstraps itself.

// A test asserts by failing; `expect` and `assert!` are the instrument,
// as in the opt-out suite beside this one.
#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::time::Duration;

use interweave_discovery_api::{DiscoveryProvider, ProviderHealth};
use interweave_discovery_kademlia::{KademliaDiscovery, KademliaProviderConfig};
use interweave_kademlia_control_api::{KademliaCommand, KademliaEvent, KademliaMode};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::runtime::kademlia_driver::{KademliaSettings, network_hash};
use interweave_transport_libp2p::{SubstrateConfig, SwarmEvent, SwarmRuntime};
use interweave_transport_runtime::TrustSources;
use libp2p::Multiaddr;

/// Long enough for two loopback nodes to identify, admit each other and
/// answer one walk; short enough that a hang is a failure rather than a
/// wait.
const PATIENCE: Duration = Duration::from_secs(20);

const NETWORK: &str = "example-private-network";

fn trusting(peers: &[&TransportIdentity]) -> TrustSources {
    TrustSources::new(
        interweave_trust_api::PeerTrustPolicy::new(peers.iter().map(|p| (*p).clone()))
            .expect("small"),
        interweave_trust_api::InfrastructureSet::default(),
    )
}

fn config(mode: KademliaMode) -> SubstrateConfig {
    SubstrateConfig {
        kademlia: Some(KademliaSettings {
            mode,
            network_id: NETWORK.to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(10),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 20,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        }),
        ..SubstrateConfig::default()
    }
}

/// The provider config the driver's settings imply.
///
/// `network_hash` is taken from the same function the driver derives its
/// protocol string with, so the two halves cannot silently disagree
/// about which network they are on — a disagreement this test would
/// otherwise report as "never became healthy".
fn provider_config(mode: KademliaMode) -> KademliaProviderConfig {
    KademliaProviderConfig {
        mode,
        wire_major: 1,
        network_hash: network_hash(NETWORK),
        candidate_ttl_ms: 600_000,
        targeted_lookup_cooldown_ms: 60_000,
        // The canonical floor (§13 refuses below 8). `effective_target`
        // is capped by the trusted population, so a two-node overlay
        // targets its one trusted peer — which is the point of that cap
        // and what makes a small overlay reachable at all.
        target_routing_peers: 8,
        max_routing_peers: 20,
        exploration_interval_ms: 30_000,
        exploration_jitter_percent: 10,
        max_concurrent_queries: 2,
        max_queries_per_minute: 60,
        bootstrap_min_interval_ms: 60_000,
        bootstrap_refresh_interval_ms: 900_000,
    }
}

async fn listening(runtime: &mut SwarmRuntime) -> Multiaddr {
    let addr: Multiaddr = "/ip4/127.0.0.1/tcp/0".parse().expect("loopback");
    runtime.listen(addr).await.expect("listen accepted")
}

/// Wait until the hub has given the third node a routing seat.
///
/// The star is only a star once the hub can answer a walk with the
/// other node. Without this the asker's exploration races the hub's own
/// admission and finds an empty list — a flake, not a failure.
async fn other_routed_at_hub(hub: &mut SwarmRuntime, other: &TransportIdentity) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "the hub never routed the third node");
        if let Ok(Some(SwarmEvent::Kademlia {
            event: KademliaEvent::RoutingPeerAdded { peer },
        })) = tokio::time::timeout(remaining, hub.next_event()).await
            && peer == *other
        {
            return;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_small_trusted_overlay_becomes_healthy_through_the_port() {
    // A STAR, not a pair. With one trusted peer the provider is target-
    // satisfied the moment that peer is routed, so it never issues a
    // query and the outbound half of the port — provider commands
    // reaching the driver — goes unexercised. The first version of this
    // test passed with every command discarded, which is what said so.
    //
    // Three nodes: the asker trusts the hub and the other, and is
    // connected only to the hub. Its effective target is therefore two
    // while its routing table holds one, so §9.3 makes it explore, and
    // reaching health requires the walk to find the third node, dial
    // it, and admit it — driver to provider and back, both ways.
    let (hub_id, other_id, asker_id) = (
        ProfileIdentity::generate(),
        ProfileIdentity::generate(),
        ProfileIdentity::generate(),
    );
    let hub_peer = hub_id.transport_identity().expect("identity");
    let other_peer = other_id.transport_identity().expect("identity");
    let asker_peer = asker_id.transport_identity().expect("identity");

    let mut hub = SwarmRuntime::start(
        &hub_id,
        config(KademliaMode::Server),
        trusting(&[&other_peer, &asker_peer]),
    )
    .expect("hub starts");
    let mut other = SwarmRuntime::start(
        &other_id,
        config(KademliaMode::Server),
        trusting(&[&hub_peer, &asker_peer]),
    )
    .expect("other starts");
    let mut asker = SwarmRuntime::start(
        &asker_id,
        config(KademliaMode::Client),
        trusting(&[&hub_peer, &other_peer]),
    )
    .expect("asker starts");

    let hub_addr = listening(&mut hub).await;
    let _ = listening(&mut other).await;
    let _ = listening(&mut asker).await;

    other
        .dial(hub_peer.clone(), hub_addr.clone())
        .await
        .expect("delivered")
        .expect("admitted");
    other_routed_at_hub(&mut hub, &other_peer).await;

    asker
        .dial(hub_peer.clone(), hub_addr)
        .await
        .expect("delivered")
        .expect("admitted");

    // THE PROVIDER, on the asker side only. Its trust set is the
    // transport's, because §9.3's effective target is computed from the
    // trusted population — a provider that trusted nobody would be
    // target-satisfied at zero, healthy for having asked nothing.
    let mut provider =
        KademliaDiscovery::new(provider_config(KademliaMode::Client), asker_peer.clone())
            .expect("valid config");
    provider.start(0).expect("starts");
    provider.set_remote_trusted(BTreeSet::from([hub_peer.clone(), other_peer.clone()]));

    // The pump a composition root will own: events in, commands out.
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut now_ms = 0_u64;
    let mut entropy = 1_u8;
    let mut queries_issued = 0_usize;
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the overlay never became healthy: {:?}, routing={}, queries={queries_issued}",
            provider.health(),
            provider.routing_view().routing_peers
        );
        now_ms += 100;

        // Drive exploration. `tick` refuses until the view warrants a
        // round and the pace allows one, so calling it every pass is
        // how a scheduler drives it, not a way to force a query.
        entropy = entropy.wrapping_add(1);
        let _ = provider.tick(now_ms, [entropy; 32]);
        for command in provider.drain_commands(usize::MAX) {
            if matches!(command, KademliaCommand::StartQuery { .. }) {
                queries_issued += 1;
            }
            asker
                .kademlia(command)
                .await
                .expect("the channel accepts it");
        }
        let _ = provider.drain_events(now_ms, usize::MAX);

        if provider.health() == ProviderHealth::Healthy {
            break;
        }

        // One event, or a short wait if the overlay is quiet — a quiet
        // moment is not a failure, only the deadline is.
        if let Ok(Some(SwarmEvent::Kademlia { event })) =
            tokio::time::timeout(Duration::from_millis(100), asker.next_event()).await
        {
            provider.ingest_driver_event(event, now_ms);
        }
    }

    assert_eq!(
        provider.health(),
        ProviderHealth::Healthy,
        "the overlay converged and a query answered"
    );
    assert_eq!(
        provider.routing_view().routing_peers,
        2,
        "health at the effective target means BOTH trusted peers are routed — \
         the second arrived only because the walk found and dialled it"
    );
    // THE OUTBOUND HALF IS EXERCISED, NOT LOAD-BEARING, and saying so
    // is the honest bound on this test. The provider commands a query
    // and the driver accepts it, but discarding every command still
    // leaves the overlay healthy: the LIBRARY's own automatic bootstrap
    // walks the star, finds the third node and dials it, and the
    // provider observes the result either way. So this asserts that a
    // command was issued and travelled, and claims nothing about health
    // depending on it.
    //
    // What IS load-bearing is the other direction. Dropping the
    // `ingest_driver_event` above leaves the provider at Degraded with
    // an empty routing table until the deadline — which is the property
    // no other suite can reach, because no other suite runs the two
    // halves against each other.
    assert!(
        queries_issued > 0,
        "the provider commanded a query and the driver's channel took it"
    );

    asker.shutdown().await.expect("stops");
    other.shutdown().await.expect("stops");
    hub.shutdown().await.expect("stops");
}
