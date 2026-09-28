// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! What the in-process admin port asks of the runtime's OWNER (plan §16
//! (2)): a shutdown request reaches `ComposedRuntime::shutdown_requested`
//! and stops nothing by itself, and `stop` returns the count of events
//! the runtime dropped, which nothing can read after it (#139 review N1).
//! The port's endpoint operations are the conformance suite's, run
//! against every binding.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_local_client_api::{AdminBinding, AdminCapability, AdminPort};
use interweave_profile_config::ProfileConfig;
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{TransportError, TransportIdentity, TransportRuntime};
use interweave_transport_composition::{ComposedRuntime, CompositionOptions};

const PATIENCE: Duration = Duration::from_secs(20);

fn profile(trusted: &[&TransportIdentity], statics: &[String]) -> ProfileConfig {
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
",
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

/// The request reaches the owner with the asking port's id and grace;
/// the runtime keeps running until the owner stops it; the first request
/// stands; a port without `admin.shutdown` is refused; and once the owner
/// has stopped the runtime, a request is `BackendUnavailable`.
#[tokio::test]
async fn an_admin_shutdown_is_a_request_the_owner_receives() {
    let (identity, _) = id();
    let runtime =
        ComposedRuntime::start(&identity, &profile(&[], &[]), CompositionOptions::default())
            .await
            .expect("composes");
    let binding = runtime.sessions();

    let powerless = binding
        .admin([AdminCapability::Endpoints].into())
        .await
        .expect("a port");
    assert_eq!(
        powerless.shutdown(Duration::from_secs(1)).await,
        Err(TransportError::CapabilityDenied)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(200), runtime.shutdown_requested())
            .await
            .is_err(),
        "nothing asked yet"
    );

    let first = binding
        .admin([AdminCapability::Shutdown].into())
        .await
        .expect("a port");
    let second = binding
        .admin([AdminCapability::Shutdown].into())
        .await
        .expect("a port");
    first.shutdown(Duration::from_secs(5)).await.expect("asked");
    second
        .shutdown(Duration::from_secs(1))
        .await
        .expect("asked too");
    let asked = tokio::time::timeout(PATIENCE, runtime.shutdown_requested())
        .await
        .expect("the owner hears it")
        .expect("a request");
    assert_eq!(&asked.port, first.port().port_id(), "the first port's");
    assert_eq!(asked.grace, Duration::from_secs(5), "and its grace stands");
    assert!(
        runtime.health().await.is_ok(),
        "a request stops nothing: the runtime still answers"
    );

    runtime.stop().await.expect("the owner stops it");
    assert_eq!(
        first.shutdown(Duration::from_secs(1)).await,
        Err(TransportError::BackendUnavailable),
        "no owner is left to ask"
    );
}

/// A consumer that never reads, behind a one-slot queue, loses events as
/// a peer connects; `stop` returns at least what was counted before it --
/// the total, not a fresh zero.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_returns_the_events_the_runtime_dropped() {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let listen = CompositionOptions {
        listen: vec![format!("/ip4/{ip}/tcp/0")],
        ..CompositionOptions::default()
    };
    let (b_id, b) = id();
    let (a_id, a) = id();
    let target = ComposedRuntime::start(&b_id, &profile(&[&a], &[]), listen.clone())
        .await
        .expect("b composes");
    let b_addr = format!("{}/p2p/{}", target.listening()[0], b.as_str());
    let unread = CompositionOptions {
        event_capacity: 1,
        ..listen
    };
    let subject = ComposedRuntime::start(&a_id, &profile(&[&b], &[b_addr]), unread)
        .await
        .expect("a composes");

    let deadline = tokio::time::Instant::now() + PATIENCE;
    while subject.events_dropped() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no event dropped within {PATIENCE:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let before = subject.events_dropped();
    let total = subject.stop().await.expect("stops");
    assert!(
        total >= before,
        "stop returned {total}, below the {before} counted"
    );
    target.shutdown().await.expect("clean shutdown");
}
