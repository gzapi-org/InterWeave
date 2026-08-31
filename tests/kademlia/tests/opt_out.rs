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
//! # What this suite does not yet prove
//!
//! **That a disabled peer advertises no DHT protocol on the wire.** The
//! command half below is proven: mutating the opt-out so a disabled
//! profile is configured after all makes it fail. The protocol half is
//! not here, because the version that was written did not fail under
//! that same mutation and would have been a green test asserting
//! nothing.
//!
//! Two things defeated it, both worth recording so the next attempt
//! starts past them. `SwarmEvent::Identified` carries the Identify
//! version string and not the peer's protocol list, so the advertisement
//! cannot be read directly and has to be proven by consequence — by an
//! observer that would route a peer advertising the DHT protocol. And
//! any helper that waits for one event DISCARDS the others, so a test
//! asserting an absence cannot use one: the admission it should catch is
//! exactly what such a helper throws away. Accumulating every event
//! fixed that and the test still passed under mutation, which means the
//! remaining gap is in the scenario rather than the plumbing.
//!
//! Tracked rather than left implicit; the exit gate is not met by this
//! file alone.

#![allow(clippy::expect_used, clippy::panic)]

use std::num::NonZeroUsize;
use std::time::Duration;

use interweave_kademlia_control_api::{KademliaCommand, KademliaMode, QueryClass};
use interweave_profile_identity::ProfileIdentity;
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
            class: QueryClass::Exploration,
            key: [3; 32],
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
            class: QueryClass::Exploration,
            key: [3; 32],
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
