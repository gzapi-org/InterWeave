// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! A circuit address on a node that composes no relay transport is a
//! structural failure, not a transient one (#111 DNS review P3-2).
//!
//! Before the DNS transport, bare TCP answered such an address
//! `MultiaddrNotSupported` and the address left the book. The DNS wrap
//! re-shapes that into `Other`, an ordinary failure, so the retry
//! scheduler dialled an operator's circuit address forever on a node
//! without a relay client. `GatedSwarm::dial` now refuses it, and the
//! refusal is structural.

#![allow(clippy::expect_used, clippy::panic)]

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::runtime::relay_driver::RelayClientSettings;
use interweave_transport_libp2p::{DialRefusal, SubstrateConfig, SwarmRuntime};
use interweave_transport_runtime::TrustSources;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::Multiaddr;

fn trusting(peer: &TransportIdentity) -> TrustSources {
    TrustSources::new(
        PeerTrustPolicy::new([peer.clone()]).expect("one peer"),
        InfrastructureSet::default(),
    )
}

/// Add a circuit route for a trusted peer, dial it, then ask what the
/// book still holds.
async fn dial_a_circuit(
    config: SubstrateConfig,
) -> (Result<(), DialRefusal>, Result<(), DialRefusal>) {
    let subject = ProfileIdentity::generate();
    let peer = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id");
    let relay = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id");
    let runtime = SwarmRuntime::start(&subject, config, trusting(&peer)).expect("starts");
    let circuit: Multiaddr = format!(
        "/ip4/127.0.0.1/tcp/1/p2p/{}/p2p-circuit/p2p/{}",
        relay.as_str(),
        peer.as_str()
    )
    .parse()
    .expect("valid");
    assert!(
        runtime
            .add_address(peer.clone(), circuit.clone())
            .await
            .expect("delivered"),
        "the operator's circuit route enters the book"
    );
    let dialled = runtime
        .dial(peer.clone(), circuit)
        .await
        .expect("delivered");
    let then = runtime.dial_peer(peer).await.expect("delivered");
    runtime.shutdown().await.expect("clean shutdown");
    (dialled, then)
}

#[tokio::test]
async fn a_circuit_on_a_node_without_a_relay_client_is_refused_and_forgotten() {
    let (dialled, then) = dial_a_circuit(SubstrateConfig::default()).await;
    assert!(
        matches!(dialled, Err(DialRefusal::Backend(_))),
        "refused at once, as the backend refusing the shape: {dialled:?}"
    );
    assert!(
        matches!(then, Err(DialRefusal::NoKnownAddress)),
        "and scored structural, so the route left the book rather than waiting to be \
         retried: {then:?}"
    );
}

/// THE CONTROL: the same route on a node that DOES compose the relay
/// transport is handed to it, and stays in the book.
#[tokio::test]
async fn a_circuit_on_a_node_with_a_relay_client_is_dialled() {
    let (dialled, then) = dial_a_circuit(SubstrateConfig {
        relay_client: Some(RelayClientSettings::default()),
        ..SubstrateConfig::default()
    })
    .await;
    assert!(
        dialled.is_ok(),
        "handed to the relay transport: {dialled:?}"
    );
    assert!(
        !matches!(then, Err(DialRefusal::NoKnownAddress)),
        "and still known: {then:?}"
    );
}
