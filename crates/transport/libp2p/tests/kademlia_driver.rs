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

use interweave_kademlia_control_api::{
    KademliaCommand, KademliaEvent, KademliaMode, QueryClass, QueryHandle,
};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::runtime::kademlia_driver::KademliaSettings;
use interweave_transport_libp2p::{SubstrateConfig, SwarmEvent, SwarmRuntime};
use interweave_transport_runtime::{DialDenial, DialOrigin, TrustSources};
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

async fn listening(runtime: &mut SwarmRuntime, ip: std::net::Ipv4Addr) -> Multiaddr {
    let addr: Multiaddr = format!("/ip4/{ip}/tcp/0")
        .parse()
        .expect("a private address");
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
    let ip = interweave_test_support::net::require_private_interface_v4();
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

    let hub_addr = listening(&mut hub, ip).await;
    let _ = listening(&mut server, ip).await;
    let _ = listening(&mut client, ip).await;

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
                || matches!(e, SwarmEvent::Connected { peer, .. } if *peer == client_peer);
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
    let ip = interweave_test_support::net::require_private_interface_v4();
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

    let a_addr = listening(&mut a, ip).await;
    let _ = listening(&mut b, ip).await;
    b.dial(a_peer.clone(), a_addr)
        .await
        .expect("delivered")
        .expect("admitted");

    wait_for(
        &mut a,
        "the connection",
        |e| matches!(e, SwarmEvent::Connected { peer, .. } if *peer == b_peer),
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
    let ip = interweave_test_support::net::require_private_interface_v4();
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

    let a_addr = listening(&mut a, ip).await;
    let _ = listening(&mut b, ip).await;
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
    ip: std::net::Ipv4Addr,
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

    let hub_addr = listening(&mut hub, ip).await;
    let _ = listening(&mut other, ip).await;
    let _ = listening(&mut asker, ip).await;

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
            handle: QueryHandle::commanded(1),
            class: QueryClass::Exploration,
            key: interweave_kademlia_control_api::LookupKey::KeySpacePoint { point: [0x42; 32] },
        })
        .await
        .expect("command delivered");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_exploration_converges_the_star_through_admitted_dials() {
    let ip = interweave_test_support::net::require_private_interface_v4();
    // The walk's own dials are BEHAVIOUR dials, and every one passes
    // the root gate: here the asker trusts the node the hub reveals, so
    // the gate ADMITS the autonomous dial, the contact succeeds, and
    // the stranger arrives both as a query candidate and as an
    // authenticated connection — the small star converges.
    let ((hub, _), (other, other_peer), (mut asker, _)) = star(true, ip).await;
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
        connected =
            connected || matches!(e, SwarmEvent::Connected { peer, .. } if *peer == other_peer);
        discovered && connected
    })
    .await;

    // THE WIRING, read where an operator reads it (#111 review P2-4).
    // The walk's dial to the third node was extended with the address
    // the hub revealed, so it crossed the root funnel; and the query
    // result naming it crossed the query-candidate hook. Both counts are
    // read through the runtime's handles, which is what fails if the
    // runtime stops sharing either with its task.
    let funnel = asker.root_funnel_counters();
    assert!(
        funnel.passed >= 1,
        "the walk's extended dial crossed the root funnel: {funnel:?}"
    );
    let candidates = asker
        .store_refusals()
        .get(interweave_transport_libp2p::store_refusals::store::QUERY_CANDIDATES)
        .cloned()
        .unwrap_or_default();
    assert!(
        candidates.admitted >= 1,
        "the result's address crossed the query-candidate hook: {candidates:?}"
    );

    hub.shutdown().await.expect("stops");
    other.shutdown().await.expect("stops");
    asker.shutdown().await.expect("stops");
}

/// ROUTING_STASH, and the operator set the runtime seeds from
/// configuration, both read through the runtime (#111 review P2-4). A
/// trusted peer is offered two names: the one the profile configured
/// is admitted, the other refused as a peer's. Each half is the other's
/// control -- a hook that refuses everything, or admits everything, or
/// judges against a set nobody seeded, fails one of them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_offer_is_judged_against_the_runtimes_operator_set_and_counted() {
    let subject = ProfileIdentity::generate();
    let peer = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id");
    let seed = "/dns4/boot.example/tcp/4001";
    let runtime = SwarmRuntime::start(
        &subject,
        SubstrateConfig {
            operator_addresses: vec![seed.to_owned()],
            ..config("wiring", KademliaMode::Server)
        },
        trusting(&[&peer]),
    )
    .expect("starts");

    runtime
        .kademlia(KademliaCommand::OfferRoutingPeer {
            addresses: interweave_kademlia_control_api::OfferedAddresses::parse_all([
                seed,
                "/dns4/a-peers-choice.invalid/tcp/4001",
            ])
            .expect("bounded"),
            peer,
        })
        .await
        .expect("command delivered");

    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let stash = runtime
            .store_refusals()
            .get(interweave_transport_libp2p::store_refusals::store::ROUTING_STASH)
            .cloned()
            .unwrap_or_default();
        if stash.admitted == 1 && stash.refused.get("not_literal").copied() == Some(1) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the offer was never judged against the runtime's own set and counted: {stash:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    runtime.shutdown().await.expect("stops");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_gate_refuses_the_walks_dial_to_a_stranger() {
    let ip = interweave_test_support::net::require_private_interface_v4();
    // The refusal half: the asker does NOT trust the node the hub
    // reveals. The iterative query autonomously dials it; the root gate
    // refuses — unauthorized is unauthorized whoever asks (F1) — and
    // refusal is not failure: the query still completes with what the
    // hub answered, and no connection to the stranger ever exists.
    let ((hub, _), (other, other_peer), (mut asker, _)) = star(false, ip).await;
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
        |e| matches!(e, SwarmEvent::Connected { peer, .. } if *peer == other_peer),
    )
    .await;

    // AND THE REFUSAL IS READABLE, which is the part that did not exist
    // before Stage 11 step 1.
    //
    // Everything asserted above is an ABSENCE: no connection, no
    // routing seat. That is exactly the problem — libp2p handles a
    // behaviour-emitted dial as `if let Ok(()) = self.dial(opts)` and
    // discards the denial, so there is no `Dialing`, no
    // `OutgoingConnectionError`, and nothing on this event stream. An
    // operator watching a node that never reaches anyone saw silence
    // and could not tell a refusal from an unreachable network.
    //
    // The gate records its own refusals and the runtime keeps the
    // handle, so the same event that produced the silence above is now
    // a readable fact with the origin that asked and the reason it was
    // told no.
    let refusals = asker.dial_refusals();
    assert!(
        refusals.total() > 0,
        "the gate's refusal of the walk's dial is recorded, not merely absent"
    );
    let recent = refusals.recent();
    assert!(
        recent.iter().any(|r| {
            r.origin == Some(DialOrigin::KademliaQuery)
                && r.denial == Some(DialDenial::Unauthorized)
        }),
        "and it names WHO asked and WHY it was refused: {recent:?}"
    );
    assert_eq!(
        refusals
            .counts()
            .get(&(
                Some(DialOrigin::KademliaQuery),
                Some(DialDenial::Unauthorized)
            ))
            .copied()
            .unwrap_or(0),
        refusals.total(),
        "every refusal in this run is that one kind, so nothing else is being counted"
    );

    hub.shutdown().await.expect("stops");
    other.shutdown().await.expect("stops");
    asker.shutdown().await.expect("stops");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_draining_runtime_refuses_new_queries_and_settles_them() {
    // LOOPBACK, deliberately: this runtime learns nothing from a peer, so
    // ADR-0052's floor has nothing to refuse here, and demanding a private
    // interface would fail the test on a host without one for no reason
    // (#111 review P3-8).
    let ip = std::net::Ipv4Addr::LOCALHOST;
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
    let _ = listening(&mut a, ip).await;
    a.drain().await.expect("draining");
    a.kademlia(KademliaCommand::StartQuery {
        handle: QueryHandle::commanded(1),
        class: QueryClass::Exploration,
        key: interweave_kademlia_control_api::LookupKey::KeySpacePoint { point: [7; 32] },
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
                    ..
                },
            }
        )
    })
    .await;
    a.shutdown().await.expect("stops");
}
