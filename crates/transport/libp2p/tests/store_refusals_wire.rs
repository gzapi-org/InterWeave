// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! ADR-0052 rule 8's store counts, read where an operator reads them:
//! from `SwarmRuntime::store_refusals()`, OUTSIDE the Swarm task.
//!
//! The #111 re-review found every refusal count write-only in a running
//! node -- each lived inside the task, read only by its own file's unit
//! tests -- while the docs said the counts were how a refusal becomes
//! visible at all. Unit tests cannot show that the fix holds, because
//! they read the counts from inside. This drives a real Identify
//! exchange between two runtimes and reads the result through the
//! public handle.
//!
//! The subject listens on this host's private address. The peer
//! listens there too AND on loopback, so its Identify advertises both.
//! The subject's address-book hook must refuse the loopback address
//! (ADR-0052's floor) and admit the private one (rule 3, beside the
//! subject's own private listener). The admission is the control: a
//! refusal count means nothing unless the same hook is seen admitting
//! too.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::store_refusals::store;
use interweave_transport_libp2p::{SubstrateConfig, SwarmRuntime};
use interweave_transport_runtime::TrustSources;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::Multiaddr;

/// Bounded: a hung exchange fails the suite with a reason rather than
/// holding CI until the job timeout.
const PATIENCE: Duration = Duration::from_secs(20);

fn trusting(peer: &TransportIdentity) -> TrustSources {
    TrustSources::new(
        PeerTrustPolicy::new([peer.clone()]).expect("one peer"),
        InfrastructureSet::default(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_peers_advertised_loopback_is_refused_and_the_refusal_is_readable_outside_the_task() {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let subject_id = ProfileIdentity::generate();
    let peer_id = ProfileIdentity::generate();
    let subject_peer = subject_id.transport_identity().expect("peer id");
    let peer_peer = peer_id.transport_identity().expect("peer id");

    let mut subject = SwarmRuntime::start(
        &subject_id,
        SubstrateConfig::default(),
        trusting(&peer_peer),
    )
    .expect("subject starts");
    let mut peer = SwarmRuntime::start(
        &peer_id,
        SubstrateConfig::default(),
        trusting(&subject_peer),
    )
    .expect("peer starts");

    let subject_addr = subject
        .listen(format!("/ip4/{ip}/tcp/0").parse().expect("valid"))
        .await
        .expect("subject listens on the private address");
    let _ = peer
        .listen(format!("/ip4/{ip}/tcp/0").parse().expect("valid"))
        .await
        .expect("peer listens on the private address");
    // THE ADDRESS UNDER TEST: advertised through Identify, refused by
    // the subject's floor.
    let _: Multiaddr = peer
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("peer listens on loopback too");

    peer.dial(subject_peer.clone(), subject_addr)
        .await
        .expect("the command reaches the task")
        .expect("the gate admits it");

    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let book = subject
            .store_refusals()
            .get(store::ADDRESS_BOOK)
            .cloned()
            .unwrap_or_default();
        if book.refused.get("special_use").copied().unwrap_or(0) >= 1 && book.admitted >= 1 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the subject never reported, through SwarmRuntime::store_refusals, both the \
             refusal of the peer's advertised loopback address and the admission of its \
             private one; last seen: {book:?}"
        );
        // Drain both runtimes, so neither stalls on a full outbox while
        // the exchange completes.
        tokio::select! {
            _ = subject.next_event() => {}
            _ = peer.next_event() => {}
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }

    subject.shutdown().await.expect("clean shutdown");
    peer.shutdown().await.expect("clean shutdown");
}
