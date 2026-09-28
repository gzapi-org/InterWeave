// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Stage 12's composition root (plan §15), from a profile document to a
//! running `TransportRuntime`.
//!
//! - translation switches on exactly what the profile's blocks enable;
//! - two composed nodes, one naming the other in its static bootstrap,
//!   connect through discovery -- the candidate learned through the
//!   peer's door and dialled on discovery's account -- and the neutral
//!   surface reports it: `PeerConnected`, `peers()`, `health()`,
//!   `connectivity()`;
//! - the control: the same entry for a peer the profile does not trust
//!   produces no connection.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_profile_config::ProfileConfig;
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{
    Health, PathReadiness, PeerPath, TransportEvent, TransportIdentity, TransportRuntime,
};
use interweave_transport_composition::{
    ComposedRuntime, CompositionError, CompositionOptions, translate,
};

const PATIENCE: Duration = Duration::from_secs(20);

fn profile(trusted: &[&TransportIdentity], statics: &[String], extra: &str) -> ProfileConfig {
    let allowed: Vec<String> = trusted
        .iter()
        .map(|p| format!("\"{}\"", p.as_str()))
        .collect();
    let peers: Vec<String> = statics.iter().map(|s| format!("\"{s}\"")).collect();
    let doc = format!(
        "schema_version: 2
trust:
  policy: static-allowlist
  allowed_peers: [{}]
endpoints:
  entries:
    - id: human
      enabled: true
      advertise: false
discovery:
  providers:
    - type: static-bootstrap
      enabled: true
      priority: 10
      config:
        peers: [{}]
{extra}",
        allowed.join(", "),
        peers.join(", ")
    );
    serde_norway::from_str(&doc).expect("the document parses")
}

fn id() -> (ProfileIdentity, TransportIdentity) {
    let identity = ProfileIdentity::generate();
    let peer = identity.transport_identity().expect("peer id");
    (identity, peer)
}

#[test]
fn translation_switches_on_what_the_blocks_enable_and_nothing_else() {
    let (_, local) = id();
    let (_, other) = id();
    let seed = format!("/dns4/boot.example/tcp/4001/p2p/{}", other.as_str());
    let composed = translate(
        &profile(&[&other], std::slice::from_ref(&seed), ""),
        &local,
        256,
    )
    .expect("a valid profile translates");
    let s = &composed.substrate;
    assert!(s.autonat_client.is_some() && s.relay_client.is_some() && s.dcutr.is_some());
    assert!(s.autonat_server.is_none() && s.relay_server.is_none());
    assert!(s.kademlia.is_none() && s.mdns.is_none());
    assert_eq!(
        s.operator_addresses,
        vec![seed],
        "the static seed is the operator's"
    );
    assert!(!composed.capabilities.durable_delivery);

    // The peer cache's limits come from its entry, not the defaults.
    let cached = translate(
        &profile(
            &[&other],
            &[],
            "    - type: peer-cache\n      enabled: true\n      priority: 20\n      config:\n        ttl: 2d\n        max_entries: 100\n",
        ),
        &local,
        256,
    )
    .expect("translates");
    let (limits, _) = cached.discovery.peer_cache.expect("the cache is planned");
    assert_eq!(
        (limits.ttl_ms(), limits.max_peers()),
        (2 * 24 * 3_600_000, 100)
    );

    let servers = translate(
        &profile(
            &[&other],
            &[],
            "transport:\n  connectivity:\n    relay:\n      server:\n        enabled: true\n    autonat:\n      server:\n        enabled: true\n",
        ),
        &local,
        256,
    )
    .expect("translates");
    assert!(servers.substrate.relay_server.is_some() && servers.substrate.autonat_server.is_some());
}

#[tokio::test]
async fn a_zero_discovery_interval_is_refused_before_anything_starts() {
    let (identity, _) = id();
    let zero = CompositionOptions {
        discovery_interval: Duration::ZERO,
        ..CompositionOptions::default()
    };
    assert!(matches!(
        ComposedRuntime::start(&identity, &profile(&[], &[], ""), zero).await,
        Err(CompositionError::Translation(_))
    ));
    let runtime = ComposedRuntime::start(
        &identity,
        &profile(&[], &[], ""),
        CompositionOptions::default(),
    )
    .await
    .expect("the control: the default interval composes");
    runtime.shutdown().await.expect("clean shutdown");
}

#[test]
fn an_invalid_profile_composes_nothing() {
    let (_, local) = id();
    let mut invalid = profile(&[], &[], "");
    invalid.schema_version = 3;
    assert!(matches!(
        translate(&invalid, &local, 256),
        Err(CompositionError::InvalidProfile(errors)) if !errors.is_empty()
    ));
}

/// ADR-0034 §7 at the composition path (architect-cto's ruling,
/// 2026-09-27): the entry point validates, so a kademlia entry enabled
/// only by the implied default composes nothing, while the same entry
/// stating `enabled: true` composes.
#[tokio::test]
async fn composition_refuses_an_implied_kademlia_default_and_runs_a_stated_one() {
    let entry = |enabled: &str| {
        format!(
            "    - type: kademlia\n{enabled}      priority: 40\n      config:\n        network_id: interweave-test\n"
        )
    };
    let (identity, _) = id();
    let implied = profile(&[], &[], "");
    let mut implied_doc = implied.clone();
    implied_doc.discovery.providers.extend(
        serde_norway::from_str::<Vec<interweave_profile_config::DiscoveryProviderConfig>>(&entry(
            "",
        ))
        .expect("parses"),
    );
    let refused =
        ComposedRuntime::start(&identity, &implied_doc, CompositionOptions::default()).await;
    assert!(
        matches!(
            &refused,
            Err(CompositionError::InvalidProfile(errors))
                if errors.iter().any(|e| matches!(
                    e,
                    interweave_profile_config::ConfigError::KademliaDefaultEnablementGated
                ))
        ),
        "the implied default composes nothing"
    );

    let mut stated_doc = implied;
    stated_doc.discovery.providers.extend(
        serde_norway::from_str::<Vec<interweave_profile_config::DiscoveryProviderConfig>>(&entry(
            "      enabled: true\n",
        ))
        .expect("parses"),
    );
    let runtime = ComposedRuntime::start(&identity, &stated_doc, CompositionOptions::default())
        .await
        .expect("a stated enabled: true composes");
    let diagnostics = runtime.diagnostics().await.expect("answered");
    assert!(
        diagnostics
            .discovery
            .providers
            .iter()
            .any(|p| p.name == "kademlia"),
        "and the kademlia provider is constructed: {:?}",
        diagnostics.discovery.providers
    );
    runtime.shutdown().await.expect("clean shutdown");
}

async fn wait_connected(runtime: &mut ComposedRuntime, peer: &TransportIdentity) -> PeerPath {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, runtime.next_event()).await {
            Ok(Some(TransportEvent::PeerConnected {
                peer: got, path, ..
            })) if &got == peer => {
                return path;
            }
            Ok(Some(_)) => {}
            Ok(None) => panic!("the runtime stopped"),
            Err(_) => panic!("no PeerConnected within {PATIENCE:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_composed_nodes_connect_through_static_discovery() {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let listen = CompositionOptions {
        listen: vec![format!("/ip4/{ip}/tcp/0")],
        ..CompositionOptions::default()
    };
    let (b_id, b) = id();
    let (a_id, a) = id();
    let target = ComposedRuntime::start(&b_id, &profile(&[&a], &[], ""), listen.clone())
        .await
        .expect("b composes");
    let b_addr = format!("{}/p2p/{}", target.listening()[0], b.as_str());

    let mut subject = ComposedRuntime::start(&a_id, &profile(&[&b], &[b_addr], ""), listen)
        .await
        .expect("a composes");
    assert_eq!(subject.local_identity().peer, a);
    assert_eq!(wait_connected(&mut subject, &b).await, PeerPath::Direct);

    let peers = subject.peers().await.expect("answered");
    assert_eq!(peers.len(), 1, "{peers:?}");
    assert_eq!((&peers[0].peer, peers[0].path), (&b, PeerPath::Direct));
    let health = subject.health().await.expect("answered");
    assert_eq!(health.aggregate, Health::Healthy, "{health:?}");
    let summary = subject.connectivity().await.expect("answered");
    assert_eq!(
        summary.relay_inbound,
        PathReadiness::Unavailable,
        "no relay configured"
    );
    let diagnostics = subject.diagnostics().await.expect("answered");
    assert_eq!(diagnostics.discovery.provider_count, 1);
    assert_eq!(diagnostics.discovery.candidates, 1);
    assert_eq!(diagnostics.substrate.dial_gate.established_connections, 1);

    subject.shutdown().await.expect("clean shutdown");
    target.shutdown().await.expect("clean shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_static_entry_for_an_untrusted_peer_produces_no_connection() {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let listen = CompositionOptions {
        listen: vec![format!("/ip4/{ip}/tcp/0")],
        ..CompositionOptions::default()
    };
    let (b_id, _) = id();
    let (a_id, a) = id();
    let target = ComposedRuntime::start(&b_id, &profile(&[&a], &[], ""), listen.clone())
        .await
        .expect("b composes");
    let b = target.local_identity().peer;
    let b_addr = format!("{}/p2p/{}", target.listening()[0], b.as_str());

    // A trusts nobody; its static bootstrap names B all the same.
    let mut subject = ComposedRuntime::start(&a_id, &profile(&[], &[b_addr], ""), listen)
        .await
        .expect("a composes");
    let outcome = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            match subject.next_event().await {
                Some(TransportEvent::PeerConnected { .. }) => return true,
                Some(_) => {}
                None => return false,
            }
        }
    })
    .await;
    assert!(
        !matches!(outcome, Ok(true)),
        "discovery grants no trust: a candidate for an untrusted peer is not dialled"
    );
    assert!(subject.peers().await.expect("answered").is_empty());

    subject.shutdown().await.expect("clean shutdown");
    target.shutdown().await.expect("clean shutdown");
}

/// The peer cache persists what this node reached and a restart comes
/// back to it (`providers/peer-cache.md`; #137 review F3): A reaches B
/// through its static bootstrap, shuts down, and a new A holding only
/// the cache file reconnects to B. The control: the same restart with a
/// fresh cache file reaches nobody.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reached_peer_survives_a_restart_through_the_peer_cache() {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let scratch = tempfile::tempdir().expect("scratch");
    let cache = scratch.path().join("peers.json");
    let options = |file: &std::path::Path| CompositionOptions {
        listen: vec![format!("/ip4/{ip}/tcp/0")],
        peer_cache_file: Some(file.to_path_buf()),
        ..CompositionOptions::default()
    };
    let with_cache = "    - type: peer-cache\n      enabled: true\n      priority: 20\n";
    let (b_id, b) = id();
    let (a_id, a) = id();
    let target = ComposedRuntime::start(
        &b_id,
        &profile(&[&a], &[], ""),
        CompositionOptions {
            listen: vec![format!("/ip4/{ip}/tcp/0")],
            ..CompositionOptions::default()
        },
    )
    .await
    .expect("b composes");
    let b_addr = format!("{}/p2p/{}", target.listening()[0], b.as_str());

    let mut first = ComposedRuntime::start(
        &a_id,
        &profile(&[&b], &[b_addr], with_cache),
        options(&cache),
    )
    .await
    .expect("a composes");
    wait_connected(&mut first, &b).await;
    first.shutdown().await.expect("clean shutdown");
    let written = std::fs::read_to_string(&cache).expect("the cache was written");
    assert!(
        written.contains(b.as_str()),
        "B's record is in the cache: {written}"
    );

    // The restart: no static entry, only the cache.
    let mut restarted =
        ComposedRuntime::start(&a_id, &profile(&[&b], &[], with_cache), options(&cache))
            .await
            .expect("a composes again");
    wait_connected(&mut restarted, &b).await;
    restarted.shutdown().await.expect("clean shutdown");

    // THE CONTROL: the same restart with a cache that holds nothing.
    let fresh = scratch.path().join("fresh.json");
    let mut cold = ComposedRuntime::start(&a_id, &profile(&[&b], &[], with_cache), options(&fresh))
        .await
        .expect("a composes cold");
    let outcome = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            match cold.next_event().await {
                Some(TransportEvent::PeerConnected { .. }) => return true,
                Some(_) => {}
                None => return false,
            }
        }
    })
    .await;
    assert!(!matches!(outcome, Ok(true)), "a cold cache reaches nobody");
    cold.shutdown().await.expect("clean shutdown");
    target.shutdown().await.expect("clean shutdown");
}
