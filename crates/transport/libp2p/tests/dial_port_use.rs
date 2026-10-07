// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! A manual dial binds a fresh local port, not the listen port
//! (`CONNECTIVITY.md` §12, "Which local port a dial uses"), observed at
//! the far socket: the unit test `each_dial_origin_binds_the_port_section_12_says`
//! pins which origin gets which policy, and this test pins that the
//! policy reaches the dial libp2p makes.
//!
//! Loopback to loopback, where libp2p-tcp would otherwise reuse the
//! listen port. With the fresh-port request dropped from the admitted
//! dial, the source port observed is the listen port and this fails.

#![allow(clippy::expect_used)]

use std::time::Duration;

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::{SubstrateConfig, SwarmRuntime};
use interweave_transport_runtime::TrustSources;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::Multiaddr;
use libp2p::multiaddr::Protocol;

const PATIENCE: Duration = Duration::from_secs(20);

fn trusting(peers: &[&TransportIdentity]) -> TrustSources {
    TrustSources::new(
        PeerTrustPolicy::new(peers.iter().map(|p| (*p).clone())).expect("a handful"),
        InfrastructureSet::default(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_manual_dial_binds_a_fresh_local_port_not_the_listen_port() {
    let id = ProfileIdentity::generate();
    let target = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id");
    let subject =
        SwarmRuntime::start(&id, SubstrateConfig::default(), trusting(&[&target])).expect("starts");
    let listening = subject
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("the subject listens");
    let listen_port = listening
        .iter()
        .find_map(|p| match p {
            Protocol::Tcp(port) => Some(port),
            _ => None,
        })
        .expect("a tcp port");

    let far = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("binds");
    let far_port = far.local_addr().expect("an address").port();
    let address: Multiaddr = format!("/ip4/127.0.0.1/tcp/{far_port}")
        .parse()
        .expect("valid");
    subject
        .dial(target, address)
        .await
        .expect("delivered")
        .expect("admitted");

    let (_stream, source) = tokio::time::timeout(PATIENCE, far.accept())
        .await
        .expect("the dial arrives")
        .expect("accepted");
    assert_ne!(
        source.port(),
        listen_port,
        "a manual dial carries a fresh source port"
    );
    subject.shutdown().await.expect("clean shutdown");
}
