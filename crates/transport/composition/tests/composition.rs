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

/// A circuit route configured as a static bootstrap peer (ADR-0052 rule
/// 9, A 2026-10-09) is the operator's whole: seeded into the operator set
/// as written, relay and destination both, and accepted by the
/// substrate's own validation of that set.
#[test]
fn a_circuit_route_seed_enters_the_operator_set_whole() {
    let (_, local) = id();
    let (_, relay) = id();
    let (_, other) = id();
    let seed = format!(
        "/ip4/203.0.113.7/tcp/4001/p2p/{}/p2p-circuit/p2p/{}",
        relay.as_str(),
        other.as_str()
    );
    let composed = translate(
        &profile(&[&other], std::slice::from_ref(&seed), ""),
        &local,
        256,
    )
    .expect("a circuit route translates");
    assert_eq!(composed.substrate.operator_addresses, vec![seed]);
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

/// A start that fails AFTER the substrate started -- here its second
/// listen address cannot be bound -- has stopped the substrate when it
/// returns: the port its first listener took is free at once, with no
/// await between. On this single-threaded runtime a substrate merely
/// dropped is only scheduled for abort and still holds the port at that
/// moment, which is what a caller retrying would meet.
#[tokio::test]
async fn a_failed_start_has_released_its_listeners_when_it_returns() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("a free port")
        .local_addr()
        .expect("its address")
        .port();
    let (identity, _) = id();
    let failed = ComposedRuntime::start(
        &identity,
        &profile(&[], &[], ""),
        CompositionOptions {
            // The second is a documentation address no host holds.
            listen: vec![
                format!("/ip4/127.0.0.1/tcp/{port}"),
                "/ip4/192.0.2.1/tcp/0".to_owned(),
            ],
            ..CompositionOptions::default()
        },
    )
    .await;
    assert!(
        matches!(failed, Err(CompositionError::Substrate(_))),
        "the second listen fails the start: {:?}",
        failed.as_ref().err()
    );
    std::net::TcpListener::bind(("127.0.0.1", port))
        .expect("the first listener's port is free when the start returns");
}

/// Every copy of the data-plane policy a composition hands out binds the
/// local peer and lists it nowhere -- discovery's (`peer_trust`) as well
/// as the substrate's (`trust.peers`), the copy the driver later changes
/// -- for a profile whose allowlist names its own identity; the other peer
/// it lists is the control.
#[test]
fn no_copy_of_the_policy_lists_the_local_peer() {
    use interweave_trust_api::{DenyReason, TrustDecision};
    let (_, local) = id();
    let (_, other) = id();
    let composed =
        translate(&profile(&[&local, &other], &[], ""), &local, 256).expect("translates");
    for (copy, policy) in [
        ("discovery's", &composed.peer_trust),
        ("the substrate's", &composed.trust.peers),
    ] {
        assert_eq!(policy.local_peer(), Some(&local), "{copy}");
        assert_eq!(
            policy.allowed_peers().collect::<Vec<_>>(),
            [&other],
            "{copy}"
        );
        assert_eq!(
            policy.decide(&local),
            TrustDecision::Denied(DenyReason::SelfIdentity),
            "{copy}"
        );
    }
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
            Err(elapsed) => panic!("no PeerConnected within {PATIENCE:?} ({elapsed})"),
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

/// The supported flags the cache file holds for `peer`'s capabilities.
fn capability_flags(file: &std::path::Path, peer: &TransportIdentity) -> Vec<bool> {
    fn find(value: &serde_json::Value, peer: &str, out: &mut Vec<bool>) {
        match value {
            serde_json::Value::Object(map) => {
                if map.get("peer_id").and_then(serde_json::Value::as_str) == Some(peer) {
                    for capability in map
                        .get("capabilities")
                        .and_then(serde_json::Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        if let Some(flag) = capability["supported"].as_bool() {
                            out.push(flag);
                        }
                    }
                }
                for child in map.values() {
                    find(child, peer, out);
                }
            }
            serde_json::Value::Array(items) => {
                for child in items {
                    find(child, peer, out);
                }
            }
            _ => {}
        }
    }
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    let json: serde_json::Value = serde_json::from_str(&text).expect("the cache is json");
    let mut out = Vec::new();
    find(&json, peer.as_str(), &mut out);
    out
}

fn kademlia_entry(mode: &str) -> String {
    format!(
        "    - type: kademlia\n      enabled: true\n      priority: 40\n      config:\n        network_id: interweave-test\n        mode: {mode}\n"
    )
}

const WITH_CACHE: &str = "    - type: peer-cache\n      enabled: true\n      priority: 20\n";

/// One run of A against a B in `b_mode`: reach it, give its Identify a
/// moment -- growing with each try, bounded -- stop both, and read what
/// the cache recorded for B; fails unless it ends at `want`.
async fn reach_and_record(
    a_id: &ProfileIdentity,
    b_id: &ProfileIdentity,
    b_listen: &str,
    cache: &std::path::Path,
    b_mode: &str,
    want: bool,
) {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let listen = || CompositionOptions {
        listen: vec![format!("/ip4/{ip}/tcp/0")],
        ..CompositionOptions::default()
    };
    let a = a_id.transport_identity().expect("peer id");
    let b = b_id.transport_identity().expect("peer id");
    for attempt in 1..=5_u64 {
        let target = ComposedRuntime::start(
            b_id,
            &profile(&[&a], &[], &kademlia_entry(b_mode)),
            CompositionOptions {
                listen: vec![b_listen.to_owned()],
                ..CompositionOptions::default()
            },
        )
        .await
        .expect("b composes");
        let b_addr = format!("{}/p2p/{}", target.listening()[0], b.as_str());
        let mut subject = ComposedRuntime::start(
            a_id,
            &profile(
                &[&b],
                &[b_addr],
                &format!("{WITH_CACHE}{}", kademlia_entry("client")),
            ),
            CompositionOptions {
                peer_cache_file: Some(cache.to_path_buf()),
                ..listen()
            },
        )
        .await
        .expect("a composes");
        wait_connected(&mut subject, &b).await;
        tokio::time::sleep(Duration::from_millis(300 * attempt)).await;
        subject.shutdown().await.expect("clean shutdown");
        target.shutdown().await.expect("clean shutdown");
        if capability_flags(cache, &b).last() == Some(&want) {
            return;
        }
    }
    panic!(
        "the cache never recorded {want} for B: {:?}",
        capability_flags(cache, &b)
    );
}

/// What an authenticated Identify said about the Kademlia server
/// protocol reaches the peer cache and survives a restart -- and newer
/// evidence supersedes it (the owner's review of c283e375, P2-3): B
/// serves, A reaches it and records `true`; A restarts with B gone and no
/// new Identify, and the cache still says `true`; B comes back as a
/// client, A reaches it and the record says `false`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_peers_kademlia_service_survives_a_restart_in_the_cache_and_is_superseded() {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let scratch = tempfile::tempdir().expect("scratch");
    let cache = scratch.path().join("peers.json");
    let listen = || CompositionOptions {
        listen: vec![format!("/ip4/{ip}/tcp/0")],
        ..CompositionOptions::default()
    };
    let (b_id, b) = id();
    let (a_id, _) = id();
    // ONE ADDRESS FOR B across every run, so the cache never holds a stale
    // one beside the static entry: this test is about the capability
    // evidence, not about which of two routes is dialled first.
    let b_listen = {
        let probe = std::net::TcpListener::bind((ip, 0)).expect("a free port");
        let port = probe.local_addr().expect("bound").port();
        format!("/ip4/{ip}/tcp/{port}")
    };

    reach_and_record(&a_id, &b_id, &b_listen, &cache, "server", true).await;

    // THE RESTART WITH NO NEW IDENTIFY: B is gone; A starts on the cache
    // alone and stops. The evidence is still there.
    let alone = ComposedRuntime::start(
        &a_id,
        &profile(
            &[&b],
            &[],
            &format!("{WITH_CACHE}{}", kademlia_entry("client")),
        ),
        CompositionOptions {
            peer_cache_file: Some(cache.clone()),
            ..listen()
        },
    )
    .await
    .expect("a composes alone");
    alone.shutdown().await.expect("clean shutdown");
    assert_eq!(
        capability_flags(&cache, &b).last(),
        Some(&true),
        "kept across a restart without a new Identify"
    );

    // NEWER EVIDENCE: B now a client, its Identify without the protocol.
    reach_and_record(&a_id, &b_id, &b_listen, &cache, "client", false).await;
}

/// A failing cache write reaches the discovery report: the directory made
/// read-only after start, the route A confirms to B cannot be written,
/// and the peer-cache provider reports Degraded rather than Healthy (the
/// owner's review of c283e375, P2-2). Its recovery is the provider's own
/// test; this pins the composition flushing THROUGH the provider.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cache_that_cannot_write_is_reported_degraded() {
    use std::os::unix::fs::PermissionsExt as _;
    let ip = interweave_test_support::net::require_private_interface_v4();
    let listen = || CompositionOptions {
        listen: vec![format!("/ip4/{ip}/tcp/0")],
        ..CompositionOptions::default()
    };
    let scratch = tempfile::tempdir().expect("scratch");
    let dir = scratch.path().join("cache");
    std::fs::create_dir(&dir).expect("a cache directory");
    let (b_id, b) = id();
    let (a_id, a) = id();
    let target = ComposedRuntime::start(&b_id, &profile(&[&a], &[], ""), listen())
        .await
        .expect("b composes");
    let b_addr = format!("{}/p2p/{}", target.listening()[0], b.as_str());
    let mut subject = ComposedRuntime::start(
        &a_id,
        &profile(&[&b], &[b_addr], WITH_CACHE),
        CompositionOptions {
            peer_cache_file: Some(dir.join("peers.json")),
            ..listen()
        },
    )
    .await
    .expect("a composes");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).expect("read-only");
    wait_connected(&mut subject, &b).await;
    let cache_health = |d: &interweave_transport_composition::Diagnostics| {
        d.discovery
            .providers
            .iter()
            .find(|p| p.name == "peer-cache")
            .and_then(|p| p.health)
    };
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let diagnostics = subject.diagnostics().await.expect("answered");
        if cache_health(&diagnostics) == Some(interweave_discovery_api::ProviderHealth::Degraded) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "never degraded: {:?}",
            cache_health(&diagnostics)
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("writable");
    subject.shutdown().await.expect("clean shutdown");
    target.shutdown().await.expect("clean shutdown");
}

/// The per-peer gate rows (`CONNECTIVITY.md` §19): one per allowlisted
/// peer and none for anyone else; a connected peer reads connected, and
/// once it is gone the row says what holds it -- the backoff the failed
/// redial set, and the failure's class. An allowlisted peer never seen
/// is the control: a row of nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_peer_rows_say_what_holds_a_peer_that_went_away() {
    use interweave_transport_composition::LastOutcome;
    let ip = interweave_test_support::net::require_private_interface_v4();
    let listen = CompositionOptions {
        listen: vec![format!("/ip4/{ip}/tcp/0")],
        ..CompositionOptions::default()
    };
    let (b_id, b) = id();
    let (a_id, a) = id();
    let (_never_id, never) = id();
    let target = ComposedRuntime::start(&b_id, &profile(&[&a], &[], ""), listen.clone())
        .await
        .expect("b composes");
    let b_addr = format!("{}/p2p/{}", target.listening()[0], b.as_str());
    let mut subject = ComposedRuntime::start(&a_id, &profile(&[&b, &never], &[b_addr], ""), listen)
        .await
        .expect("a composes");
    wait_connected(&mut subject, &b).await;

    let rows = subject.diagnostics().await.expect("answered").peers;
    assert_eq!(rows.len(), 2, "one row per allowlisted peer: {rows:?}");
    let row = |rows: &[interweave_transport_composition::PeerGateRow], p: &TransportIdentity| {
        rows.iter().find(|r| &r.peer == p).cloned().expect("a row")
    };
    let held = row(&rows, &b);
    assert!(held.connected, "{held:?}");
    assert_eq!(held.last_outcome, Some(LastOutcome::Connected));
    let unseen = row(&rows, &never);
    assert_eq!(
        (
            unseen.connected,
            unseen.backoff_until_ms,
            unseen.quarantined_until_ms,
            unseen.last_outcome
        ),
        (false, None, None, None),
        "nothing holds a peer never dialled"
    );

    target.shutdown().await.expect("clean shutdown");
    // The reconnect round redials B, the dial is refused, and the row
    // follows within the round's cadence.
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let gone = loop {
        let rows = subject.diagnostics().await.expect("answered").peers;
        let now = row(&rows, &b);
        if !now.connected && now.backoff_until_ms.is_some() && now.last_outcome.is_some() {
            break now;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the row never showed the hold: {now:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert!(
        matches!(
            gone.last_outcome,
            Some(LastOutcome::DialFailed | LastOutcome::Denied)
        ),
        "{gone:?}"
    );
    assert_eq!(gone.quarantined_until_ms, None);

    subject.shutdown().await.expect("clean shutdown");
}
