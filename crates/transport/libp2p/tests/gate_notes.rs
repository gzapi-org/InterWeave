// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The gate's decisions reach the consumer as address-free events
//! (`observability.md` §Logs, A 2026-10-06), over real sockets:
//!
//! - a dial the remote end refuses is `DialFailed` with the `dial_failed`
//!   class (the socket's kind is not reachable structurally --
//!   `DialFailureClass::of_dial_error`), and the retry it scheduled is
//!   `RetryScheduled` with `CONNECTIVITY.md`'s 30 s;
//! - a dial answered by another identity is `DialFailed` with the
//!   `identity_mismatch` class, and `AddressQuarantined` for 30 min --
//!   and NO retry, which is the control that each event follows its own
//!   decision rather than any failure.
//!
//! What a line or a diagnostics row carries is the class; `detail`,
//! the library's text, can name an address and is not read here.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::{DialFailureClass, SubstrateConfig, SwarmEvent, SwarmRuntime};
use interweave_transport_runtime::{DialOrigin, TrustSources};
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::Multiaddr;

const PATIENCE: Duration = Duration::from_secs(20);

fn trusting(peers: &[&TransportIdentity]) -> TrustSources {
    TrustSources::new(
        PeerTrustPolicy::new(peers.iter().map(|p| (*p).clone())).expect("a handful"),
        InfrastructureSet::default(),
    )
}

/// Every event up to the first that `done` accepts, that one included.
async fn events_until<F>(runtime: &mut SwarmRuntime, what: &str, mut done: F) -> Vec<SwarmEvent>
where
    F: FnMut(&SwarmEvent) -> bool,
{
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut seen = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, runtime.next_event()).await {
            Err(elapsed) => panic!("timed out waiting for {what} ({elapsed}): {seen:?}"),
            Ok(None) => panic!("the runtime stopped while waiting for {what}"),
            Ok(Some(event)) => {
                let last = done(&event);
                seen.push(event);
                if last {
                    return seen;
                }
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_dial_is_classed_and_its_retry_is_reported_with_its_delay() {
    let (id, target) = (
        ProfileIdentity::generate(),
        ProfileIdentity::generate()
            .transport_identity()
            .expect("peer id"),
    );
    let mut subject =
        SwarmRuntime::start(&id, SubstrateConfig::default(), trusting(&[&target])).expect("starts");
    // A port nothing listens on: bound and released.
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("binds")
        .local_addr()
        .expect("an address")
        .port();
    let address: Multiaddr = format!("/ip4/127.0.0.1/tcp/{port}").parse().expect("valid");
    subject
        .dial(target.clone(), address)
        .await
        .expect("delivered")
        .expect("admitted");

    let seen = events_until(&mut subject, "the retry", |e| {
        matches!(e, SwarmEvent::RetryScheduled { .. })
    })
    .await;
    assert!(
        seen.iter().any(|e| matches!(
            e,
            SwarmEvent::DialFailed { peer: Some(p), class: DialFailureClass::DialFailed, .. }
                if *p == target
        )),
        "{seen:?}"
    );
    assert_eq!(
        seen.last(),
        Some(&SwarmEvent::RetryScheduled {
            peer: target,
            origin: DialOrigin::Manual,
            attempt: 1,
            delay_ms: 30_000,
            peer_backoff: true,
        })
    );
    subject.shutdown().await.expect("clean shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_identity_mismatch_is_classed_and_quarantined_and_schedules_no_retry() {
    let subject_id = ProfileIdentity::generate();
    let answering_id = ProfileIdentity::generate();
    let expected = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id");
    let answering_peer = answering_id.transport_identity().expect("peer id");
    let subject_peer = subject_id.transport_identity().expect("peer id");
    let mut subject = SwarmRuntime::start(
        &subject_id,
        SubstrateConfig::default(),
        trusting(&[&expected, &answering_peer]),
    )
    .expect("starts");
    let answering = SwarmRuntime::start(
        &answering_id,
        SubstrateConfig::default(),
        trusting(&[&subject_peer]),
    )
    .expect("starts");
    let address = answering
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("listens");
    // Dialled as `expected`; `answering` authenticates as itself.
    subject
        .dial(expected.clone(), address)
        .await
        .expect("delivered")
        .expect("admitted");

    let seen = events_until(&mut subject, "the quarantine", |e| {
        matches!(e, SwarmEvent::AddressQuarantined { .. })
    })
    .await;
    assert!(
        seen.iter().any(|e| matches!(
            e,
            SwarmEvent::DialFailed {
                class: DialFailureClass::IdentityMismatch,
                ..
            }
        )),
        "{seen:?}"
    );
    assert_eq!(
        seen.last(),
        Some(&SwarmEvent::AddressQuarantined {
            peer: expected,
            for_ms: 30 * 60 * 1_000,
        })
    );
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, SwarmEvent::RetryScheduled { .. })),
        "a quarantine is not a retry: {seen:?}"
    );
    subject.shutdown().await.expect("clean shutdown");
    answering.shutdown().await.expect("clean shutdown");
}
