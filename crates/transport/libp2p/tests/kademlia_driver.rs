// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Kademlia driver over real sockets.
//!
//! Loopback TCP, real Noise, real Identify, the real root gate: these
//! are the §7/§9 behaviours that only exist end-to-end — a mocked
//! transport would prove the translation layer compiles and nothing
//! else.

#![allow(clippy::expect_used, clippy::panic)]

use std::num::NonZeroUsize;
use std::time::Duration;

use interweave_kademlia_control_api::{KademliaCommand, KademliaEvent, KademliaMode, QueryClass};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::runtime::kademlia_driver::KademliaSettings;
use interweave_transport_libp2p::{SubstrateConfig, SwarmEvent, SwarmRuntime};
use interweave_transport_runtime::TrustSources;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::Multiaddr;

const PATIENCE: Duration = Duration::from_secs(20);

/// A quiet window long enough for anything wrongly queued to surface.
const GRACE: Duration = Duration::from_secs(2);

fn trusting(peers: &[&TransportIdentity]) -> TrustSources {
    TrustSources::new(
        PeerTrustPolicy::new(peers.iter().map(|p| (*p).clone())).expect("a handful"),
        InfrastructureSet::default(),
    )
}

fn kad_settings(network_id: &str, mode: KademliaMode) -> KademliaSettings {
    KademliaSettings {
        mode,
        network_id: network_id.to_owned(),
        kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
        query_timeout: Duration::from_secs(10),
        parallelism: NonZeroUsize::new(3).expect("nonzero"),
        disjoint_query_paths: true,
        max_routing_peers: 256,
        max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
        max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
    }
}

fn config(network_id: &str, mode: KademliaMode) -> SubstrateConfig {
    SubstrateConfig {
        kademlia: Some(kad_settings(network_id, mode)),
        ..SubstrateConfig::default()
    }
}

async fn wait_for<F>(runtime: &mut SwarmRuntime, what: &str, mut predicate: F) -> SwarmEvent
where
    F: FnMut(&SwarmEvent) -> bool,
{
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for {what}");
        match tokio::time::timeout(remaining, runtime.next_event()).await {
            Err(_) => panic!("timed out waiting for {what}"),
            Ok(None) => panic!("the substrate stopped while waiting for {what}"),
            Ok(Some(event)) => {
                if predicate(&event) {
                    return event;
                }
            }
        }
    }
}

/// Drain events for [`GRACE`], panicking if any matches.
async fn assert_quiet<F>(runtime: &mut SwarmRuntime, what: &str, mut forbidden: F)
where
    F: FnMut(&SwarmEvent) -> bool,
{
    let deadline = tokio::time::Instant::now() + GRACE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        if let Ok(Some(event)) = tokio::time::timeout(remaining, runtime.next_event()).await {
            assert!(!forbidden(&event), "{what}: got {event:?}");
        }
    }
}

async fn listening(runtime: &mut SwarmRuntime) -> Multiaddr {
    let addr: Multiaddr = "/ip4/127.0.0.1/tcp/0".parse().expect("loopback");
    runtime.listen(addr).await.expect("listen accepted")
}

fn routed(event: &SwarmEvent, who: &TransportIdentity) -> bool {
    matches!(
        event,
        SwarmEvent::Kademlia {
            event: KademliaEvent::RoutingPeerAdded { peer },
        } if peer == who
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_trusted_server_routes_and_a_client_never_does() {
    // §7 end-to-end: connection alone routes nobody; connection PLUS
    // trust PLUS authenticated Identify evidence of the exact server
    // protocol routes (F3, and on the listener the connection is
    // INBOUND — that is F3's whole point). A client-mode peer never
    // advertises the protocol, so it satisfies everything but the
    // evidence conjunct and is never routed (F17, driver side).
    let hub_id = ProfileIdentity::generate();
    let server_id = ProfileIdentity::generate();
    let client_id = ProfileIdentity::generate();
    let hub_peer = hub_id.transport_identity().expect("peer id");
    let server_peer = server_id.transport_identity().expect("peer id");
    let client_peer = client_id.transport_identity().expect("peer id");

    let mut hub = SwarmRuntime::start(
        &hub_id,
        config("kad-driver-e2e", KademliaMode::Server),
        trusting(&[&server_peer, &client_peer]),
    )
    .expect("hub");
    let mut server = SwarmRuntime::start(
        &server_id,
        config("kad-driver-e2e", KademliaMode::Server),
        trusting(&[&hub_peer]),
    )
    .expect("server");
    let mut client = SwarmRuntime::start(
        &client_id,
        config("kad-driver-e2e", KademliaMode::Client),
        trusting(&[&hub_peer]),
    )
    .expect("client");

    let hub_addr = listening(&mut hub).await;
    let _ = listening(&mut server).await;
    let _ = listening(&mut client).await;

    server
        .dial(hub_peer.clone(), hub_addr.clone())
        .await
        .expect("delivered")
        .expect("admitted");
    client
        .dial(hub_peer.clone(), hub_addr)
        .await
        .expect("delivered")
        .expect("admitted");

    // ONE stateful wait on the hub: its event stream is drained
    // destructively, so waiting for these in sequence would discard
    // whichever arrived while waiting for the other.
    let mut server_routed = false;
    let mut client_connected = false;
    wait_for(
        &mut hub,
        "the server routed and the client connected",
        |e| {
            // The hub routes the SERVER: inbound connection, Identify says
            // it serves the exact protocol, trust says keep it (F3).
            server_routed = server_routed || routed(e, &server_peer);
            client_connected = client_connected
                || matches!(e, SwarmEvent::Connected { peer } if *peer == client_peer);
            assert!(
                !routed(e, &client_peer),
                "a client-mode peer must never be routed"
            );
            server_routed && client_connected
        },
    )
    .await;
    // And the server routes the hub back from its own Identify view.
    wait_for(&mut server, "the hub routed at the server", |e| {
        routed(e, &hub_peer)
    })
    .await;
    // The in-wait assertion covered the window up to convergence; this
    // covers the quiet after it.
    assert_quiet(&mut hub, "a client-mode peer must never be routed", |e| {
        routed(e, &client_peer)
    })
    .await;
    assert_quiet(&mut hub, "a client-mode peer must never be routed", |e| {
        routed(e, &client_peer)
    })
    .await;

    hub.shutdown().await.expect("stops");
    server.shutdown().await.expect("stops");
    client.shutdown().await.expect("stops");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_network_ids_never_mix() {
    // §4: the namespace exists so unrelated deployments sharing
    // bootstrap infrastructure cannot mix DHTs. Both sides are server
    // mode, mutually trusted, connected and identified — and each
    // advertises a protocol the other does not speak.
    let a_id = ProfileIdentity::generate();
    let b_id = ProfileIdentity::generate();
    let a_peer = a_id.transport_identity().expect("peer id");
    let b_peer = b_id.transport_identity().expect("peer id");

    let mut a = SwarmRuntime::start(
        &a_id,
        config("network-alpha", KademliaMode::Server),
        trusting(&[&b_peer]),
    )
    .expect("a");
    let mut b = SwarmRuntime::start(
        &b_id,
        config("network-beta", KademliaMode::Server),
        trusting(&[&a_peer]),
    )
    .expect("b");

    let a_addr = listening(&mut a).await;
    let _ = listening(&mut b).await;
    b.dial(a_peer.clone(), a_addr)
        .await
        .expect("delivered")
        .expect("admitted");

    wait_for(
        &mut a,
        "the connection",
        |e| matches!(e, SwarmEvent::Connected { peer } if *peer == b_peer),
    )
    .await;
    assert_quiet(
        &mut a,
        "a foreign network's peer must never be routed",
        |e| routed(e, &b_peer),
    )
    .await;
    assert_quiet(
        &mut b,
        "a foreign network's peer must never be routed",
        |e| routed(e, &a_peer),
    )
    .await;

    a.shutdown().await.expect("stops");
    b.shutdown().await.expect("stops");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revocation_removes_the_routing_seat_immediately() {
    let a_id = ProfileIdentity::generate();
    let b_id = ProfileIdentity::generate();
    let a_peer = a_id.transport_identity().expect("peer id");
    let b_peer = b_id.transport_identity().expect("peer id");

    let mut a = SwarmRuntime::start(
        &a_id,
        config("kad-revocation", KademliaMode::Server),
        trusting(&[&b_peer]),
    )
    .expect("a");
    let mut b = SwarmRuntime::start(
        &b_id,
        config("kad-revocation", KademliaMode::Server),
        trusting(&[&a_peer]),
    )
    .expect("b");

    let a_addr = listening(&mut a).await;
    let _ = listening(&mut b).await;
    b.dial(a_peer.clone(), a_addr)
        .await
        .expect("delivered")
        .expect("admitted");
    wait_for(&mut a, "b routed at a", |e| routed(e, &b_peer)).await;

    // Trust moves away from b: the routing seat goes with it, in the
    // same command, not when some later event notices (§11).
    let closed = a.set_trust(trusting(&[])).await.expect("trust applied");
    assert!(closed >= 1, "the revoked connection is closed too");
    wait_for(&mut a, "the routing seat removed", |e| {
        matches!(
            e,
            SwarmEvent::Kademlia {
                event: KademliaEvent::RoutingPeerRemoved { peer },
            } if *peer == b_peer
        )
    })
    .await;

    a.shutdown().await.expect("stops");
    b.shutdown().await.expect("stops");
}

/// Build the three-node star: hub trusts both leaves; each leaf trusts
/// the hub, and `asker_trusts_other` decides the experiment.
async fn star(
    asker_trusts_other: bool,
) -> (
    (SwarmRuntime, TransportIdentity),
    (SwarmRuntime, TransportIdentity),
    (SwarmRuntime, TransportIdentity),
) {
    let hub_id = ProfileIdentity::generate();
    let other_id = ProfileIdentity::generate();
    let asker_id = ProfileIdentity::generate();
    let hub_peer = hub_id.transport_identity().expect("peer id");
    let other_peer = other_id.transport_identity().expect("peer id");
    let asker_peer = asker_id.transport_identity().expect("peer id");

    let mut hub = SwarmRuntime::start(
        &hub_id,
        config("kad-star", KademliaMode::Server),
        trusting(&[&other_peer, &asker_peer]),
    )
    .expect("hub");
    let mut other = SwarmRuntime::start(
        &other_id,
        config("kad-star", KademliaMode::Server),
        trusting(&[&hub_peer, &asker_peer]),
    )
    .expect("other");
    let asker_trust = if asker_trusts_other {
        trusting(&[&hub_peer, &other_peer])
    } else {
        trusting(&[&hub_peer])
    };
    let mut asker = SwarmRuntime::start(
        &asker_id,
        config("kad-star", KademliaMode::Server),
        asker_trust,
    )
    .expect("asker");

    let hub_addr = listening(&mut hub).await;
    let _ = listening(&mut other).await;
    let _ = listening(&mut asker).await;

    other
        .dial(hub_peer.clone(), hub_addr.clone())
        .await
        .expect("delivered")
        .expect("admitted");
    wait_for(&mut hub, "other routed at hub", |e| routed(e, &other_peer)).await;

    asker
        .dial(hub_peer.clone(), hub_addr)
        .await
        .expect("delivered")
        .expect("admitted");
    wait_for(&mut asker, "hub routed at asker", |e| routed(e, &hub_peer)).await;

    ((hub, hub_peer), (other, other_peer), (asker, asker_peer))
}

async fn explore(asker: &mut SwarmRuntime) {
    asker
        .kademlia(KademliaCommand::StartQuery {
            class: QueryClass::Exploration,
            key: [0x42; 32],
        })
        .await
        .expect("command delivered");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_exploration_converges_the_star_through_admitted_dials() {
    // The walk's own dials are BEHAVIOUR dials, and every one passes
    // the root gate: here the asker trusts the node the hub reveals, so
    // the gate ADMITS the autonomous dial, the contact succeeds, and
    // the stranger arrives both as a query candidate and as an
    // authenticated connection — the small star converges.
    let ((hub, _), (other, other_peer), (mut asker, _)) = star(true).await;
    explore(&mut asker).await;

    let mut discovered = false;
    let mut connected = false;
    wait_for(&mut asker, "the walk to reach the third node", |e| {
        if let SwarmEvent::Kademlia {
            event: KademliaEvent::QueryResults { candidates, .. },
        } = e
        {
            discovered = discovered
                || candidates
                    .as_slice()
                    .iter()
                    .any(|c| c.peer_id == other_peer);
        }
        connected = connected || matches!(e, SwarmEvent::Connected { peer } if *peer == other_peer);
        discovered && connected
    })
    .await;

    hub.shutdown().await.expect("stops");
    other.shutdown().await.expect("stops");
    asker.shutdown().await.expect("stops");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_gate_refuses_the_walks_dial_to_a_stranger() {
    // The refusal half: the asker does NOT trust the node the hub
    // reveals. The iterative query autonomously dials it; the root gate
    // refuses — unauthorized is unauthorized whoever asks (F1) — and
    // refusal is not failure: the query still completes with what the
    // hub answered, and no connection to the stranger ever exists.
    let ((hub, _), (other, other_peer), (mut asker, _)) = star(false).await;
    explore(&mut asker).await;

    wait_for(&mut asker, "the exploration to complete", |e| {
        matches!(
            e,
            SwarmEvent::Kademlia {
                event: KademliaEvent::QueryResults {
                    class: QueryClass::Exploration,
                    ..
                },
            } | SwarmEvent::Kademlia {
                event: KademliaEvent::QueryFailed {
                    class: QueryClass::Exploration,
                    ..
                },
            }
        )
    })
    .await;
    assert_quiet(
        &mut asker,
        "no connection to the untrusted stranger",
        |e| matches!(e, SwarmEvent::Connected { peer } if *peer == other_peer),
    )
    .await;

    hub.shutdown().await.expect("stops");
    other.shutdown().await.expect("stops");
    asker.shutdown().await.expect("stops");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_draining_runtime_refuses_new_queries_and_settles_them() {
    // Root drain reaches the driver: outstanding work settles, nothing
    // new starts, and the refusal is SETTLED on the port rather than
    // silently swallowed during the grace period.
    let a_id = ProfileIdentity::generate();
    let mut a = SwarmRuntime::start(
        &a_id,
        config("kad-drain", KademliaMode::Server),
        trusting(&[]),
    )
    .expect("a");
    let _ = listening(&mut a).await;
    a.drain().await.expect("draining");
    a.kademlia(KademliaCommand::StartQuery {
        class: QueryClass::Exploration,
        key: [7; 32],
    })
    .await
    .expect("command delivered");
    wait_for(&mut a, "the drained refusal to settle", |e| {
        matches!(
            e,
            SwarmEvent::Kademlia {
                event: KademliaEvent::QueryFailed {
                    class: QueryClass::Exploration,
                    reason: interweave_kademlia_control_api::QueryFailure::ShuttingDown,
                },
            }
        )
    })
    .await;
    a.shutdown().await.expect("stops");
}
