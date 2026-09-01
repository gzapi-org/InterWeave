// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Stage 10's opt-out exit gate: `enabled: false` produces ZERO
//! Kademlia protocol and query activity.
//!
//! **Only observable from outside**, which is why this suite is here
//! rather than beside the driver. "No activity" is not a statement about
//! a function's return value: a runtime that merely declined to record
//! anything locally, while still advertising the DHT protocol and
//! answering walks, would satisfy any in-crate assertion and violate
//! ADR-0034's opt-out. The two observers that can tell are another peer
//! deciding whether to route this one, and a command channel that either
//! answers or does not.
//!
//! Each test carries its own control. "Nothing happened" is equally
//! explained by a test that never ran, so a disabled subject is always
//! measured beside an enabled one that must produce the very event the
//! subject must not — and the control is verified to FIRE, not merely
//! written down.
//!
//! # What this suite proves, and how the second half is observed
//!
//! The protocol half does not infer an advertisement from a translated
//! runtime event — that path is a dead end, because
//! `SwarmEvent::Identified` carries the Identify VERSION string and not
//! the peer's protocol list. It reads the list itself, from a raw
//! `Swarm<identify::Behaviour>` that dials each subject and inspects
//! `identify::Event::Received.info.protocols`. An observer outside the
//! runtime is the only vantage point from which "advertises nothing"
//! is a statement about the wire rather than about our own bookkeeping.
//!
//! # What defeated the first attempt, kept because it is the lesson
//!
//! The protocol half was written once, removed for proving nothing, and
//! written again from a review's design. Two things defeated the first
//! version. `SwarmEvent::Identified` carries the Identify VERSION
//! string, not the protocol list, so the advertisement cannot be read
//! from the runtime's own events and had to be inferred from a
//! consequence — an observer that would have routed the peer. And any
//! helper that waits for one event DISCARDS the others, so a test
//! asserting an ABSENCE cannot use one: the admission it should catch
//! is exactly what such a helper throws away.
//!
//! Accumulating every event fixed the second problem and the test still
//! passed under mutation, which placed the fault in the first: inferring
//! from a consequence was the wrong instrument. Reading `info.protocols`
//! off a raw Identify swarm is direct, and the mutation fails against
//! it.

#![allow(clippy::expect_used, clippy::panic)]

use std::num::NonZeroUsize;
use std::time::Duration;

use interweave_kademlia_control_api::{
    KademliaCommand, KademliaMode, LookupKey, QueryClass, QueryHandle,
};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::runtime::kademlia_driver::KademliaSettings;
use interweave_transport_libp2p::{SubstrateConfig, SwarmEvent, SwarmRuntime};
use interweave_transport_runtime::TrustSources;
use libp2p::Multiaddr;

const PATIENCE: Duration = Duration::from_secs(20);

/// Long enough for anything wrongly queued to surface.
///
/// A quiet window is only as strong as its length, and this one is the
/// same the driver suite uses: the enabled control produces its event
/// well inside it, which is what says the window is long enough rather
/// than merely short enough to pass.
const GRACE: Duration = Duration::from_secs(2);

const NETWORK: &str = "example-private-network";

fn trusting(peers: &[&TransportIdentity]) -> TrustSources {
    TrustSources::new(
        interweave_trust_api::PeerTrustPolicy::new(peers.iter().map(|p| (*p).clone()))
            .expect("a handful"),
        interweave_trust_api::InfrastructureSet::default(),
    )
}

fn enabled(mode: KademliaMode) -> SubstrateConfig {
    SubstrateConfig {
        kademlia: Some(KademliaSettings {
            mode,
            network_id: NETWORK.to_owned(),
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            query_timeout: Duration::from_secs(10),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            disjoint_query_paths: true,
            max_routing_peers: 256,
            max_results_per_query: NonZeroUsize::new(20).expect("nonzero"),
            max_concurrent_queries: NonZeroUsize::new(2).expect("nonzero"),
        }),
        ..SubstrateConfig::default()
    }
}

/// The operator's `enabled: false`: no configured entry at all.
fn disabled() -> SubstrateConfig {
    SubstrateConfig {
        kademlia: None,
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

fn any_kademlia(event: &SwarmEvent) -> bool {
    matches!(event, SwarmEvent::Kademlia { .. })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disabled_profile_answers_no_kademlia_command() {
    // ADR-0034: the opt-out must produce zero QUERY activity, and a
    // command is where query activity would start. A disabled runtime
    // drops it — §13's "no provider task, no routing participation, no
    // query activity" — and says nothing back.
    let subject = ProfileIdentity::generate();
    let mut disabled_node =
        SwarmRuntime::start(&subject, disabled(), TrustSources::default()).expect("starts");
    let _ = listening(&mut disabled_node).await;

    disabled_node
        .kademlia(KademliaCommand::StartQuery {
            // An exploration point, not an identity: §9.3 walks a random
            // point in the key space and the type says so.
            handle: QueryHandle::commanded(1),
            class: QueryClass::Exploration,
            key: LookupKey::KeySpacePoint { point: [3; 32] },
        })
        .await
        .expect("the channel accepts it");
    assert_quiet(&mut disabled_node, "a disabled profile", any_kademlia).await;

    // THE CONTROL. Without it, "no event" is equally explained by a
    // command that never reached the runtime at all — which would make
    // this test pass for a build that dropped every command, enabled or
    // not.
    let peer = ProfileIdentity::generate();
    let mut enabled_node = SwarmRuntime::start(
        &peer,
        enabled(KademliaMode::Client),
        TrustSources::default(),
    )
    .expect("starts");
    let _ = listening(&mut enabled_node).await;
    enabled_node
        .kademlia(KademliaCommand::StartQuery {
            // An exploration point, not an identity: §9.3 walks a random
            // point in the key space and the type says so.
            handle: QueryHandle::commanded(1),
            class: QueryClass::Exploration,
            key: LookupKey::KeySpacePoint { point: [3; 32] },
        })
        .await
        .expect("the channel accepts it");
    wait_for(
        &mut enabled_node,
        "the enabled control to answer the same command",
        any_kademlia,
    )
    .await;

    disabled_node.shutdown().await.expect("stops");
    enabled_node.shutdown().await.expect("stops");
}

/// What a raw Identify observer saw a peer advertise.
///
/// Outside the runtime on purpose. `SwarmEvent::Identified` carries the
/// Identify version string and not the protocol list, so nothing inside
/// can answer "what did this peer advertise" — and a disabled node that
/// still advertised the DHT protocol would satisfy every in-crate
/// assertion while violating ADR-0034's opt-out.
async fn advertised_protocols(
    keys: libp2p::identity::Keypair,
    target: &TransportIdentity,
    address: Multiaddr,
) -> Vec<String> {
    use futures::StreamExt as _;
    let mut observer = libp2p::SwarmBuilder::with_existing_identity(keys)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("the same transport stack the subjects use")
        .with_behaviour(|k| {
            libp2p::identify::Behaviour::new(libp2p::identify::Config::new(
                "/interweave-optout-observer/1".to_owned(),
                k.public(),
            ))
        })
        .expect("behaviour")
        .build();

    let peer: libp2p::PeerId = target.as_str().parse().expect("a libp2p identity");
    observer
        .dial(
            libp2p::swarm::dial_opts::DialOpts::peer_id(peer)
                .addresses(vec![address])
                .build(),
        )
        .expect("dial accepted");

    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "no Identify arrived from the subject");
        match tokio::time::timeout(remaining, observer.select_next_some()).await {
            Ok(libp2p::swarm::SwarmEvent::Behaviour(libp2p::identify::Event::Received {
                info,
                ..
            })) => {
                return info.protocols.iter().map(ToString::to_string).collect();
            }
            Ok(_) => {}
            Err(_) => panic!("no Identify arrived from the subject"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disabled_profile_advertises_no_dht_protocol() {
    // ZERO PROTOCOL ACTIVITY, read off the wire rather than inferred.
    // The observer is a bare Identify swarm, so what it reports is what
    // the subject actually advertised — not what our own translation
    // layer chose to surface.
    let expected = interweave_transport_libp2p::runtime::kademlia_driver::kad_protocol(NETWORK);

    // The observer's identity is known up front, because each subject
    // must TRUST it — otherwise the connection closes before Identify
    // completes and "no DHT protocol" would mean "no conversation".
    let observer_keys = libp2p::identity::Keypair::generate_ed25519();
    let observer_peer = TransportIdentity::parse(observer_keys.public().to_peer_id().to_base58())
        .expect("a canonical identity");

    let on_id = ProfileIdentity::generate();
    let off_id = ProfileIdentity::generate();
    let on_peer = on_id.transport_identity().expect("identity");
    let off_peer = off_id.transport_identity().expect("identity");

    let mut on = SwarmRuntime::start(
        &on_id,
        enabled(KademliaMode::Server),
        trusting(&[&observer_peer]),
    )
    .expect("starts");
    let mut off =
        SwarmRuntime::start(&off_id, disabled(), trusting(&[&observer_peer])).expect("starts");
    let on_addr = listening(&mut on).await;
    let off_addr = listening(&mut off).await;

    // THE CONTROL FIRST. If an enabled server does not advertise the
    // exact protocol, the subject's silence proves nothing about the
    // opt-out and everything about the observer.
    let advertised_on = advertised_protocols(observer_keys.clone(), &on_peer, on_addr).await;
    assert!(
        advertised_on.contains(&expected),
        "the control: an enabled server advertises {expected}, got {advertised_on:?}"
    );

    let advertised_off = advertised_protocols(observer_keys, &off_peer, off_addr).await;
    assert!(
        !advertised_off.contains(&expected),
        "a disabled profile must advertise no DHT protocol, got {advertised_off:?}"
    );

    on.shutdown().await.expect("stops");
    off.shutdown().await.expect("stops");
}
