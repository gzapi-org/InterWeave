// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Stage 12's composition root (plan §15), from a profile document to a
//! running TransportRuntime.
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
    let composed = translate(&profile(&[&other], &[seed.clone()], ""), &local, 256)
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
