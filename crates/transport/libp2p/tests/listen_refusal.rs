// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! A listener that cannot bind says why, over the real TCP transport.
//!
//! `libp2p-tcp` makes, binds and listens on the socket inside
//! `listen_on`, and a refusal there arrives as `TransportError::Other`,
//! which displays as nothing: the listen reply was an empty string, and
//! an Android device run reported `transport: ` with no cause
//! (2026-10-10). An address this host does not hold is refused by
//! `bind` (`EADDRNOTAVAIL`) the same way, synchronously, so it stands
//! for every such refusal.

#![allow(clippy::expect_used, clippy::panic)]

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_libp2p::{SubstrateConfig, SubstrateError, SwarmRuntime};
use interweave_transport_runtime::TrustSources;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::Multiaddr;

fn runtime() -> SwarmRuntime {
    SwarmRuntime::start(
        &ProfileIdentity::generate(),
        SubstrateConfig::default(),
        TrustSources::new(
            PeerTrustPolicy::new([]).expect("empty"),
            InfrastructureSet::default(),
        ),
    )
    .expect("starts")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_listener_that_cannot_bind_says_why() {
    let runtime = runtime();
    // TEST-NET-1 (RFC 5737): never assigned to a host interface.
    let unheld: Multiaddr = "/ip4/192.0.2.1/tcp/0".parse().expect("multiaddr");
    let refused = runtime.listen(unheld).await;
    let Err(SubstrateError::Transport(detail)) = refused else {
        panic!("a bind to an address this host does not hold is refused: {refused:?}");
    };
    // The socket's own error, as `std::io::Error` displays an OS one.
    assert_eq!(
        detail.matches("(os error ").count(),
        1,
        "the refusal carries the socket's own error, once: {detail:?}"
    );

    // THE CONTROL: the same runtime binds an address it does hold, so the
    // refusal above is the address's, not a runtime that cannot listen.
    let held: Multiaddr = "/ip4/127.0.0.1/tcp/0".parse().expect("multiaddr");
    runtime.listen(held).await.expect("loopback binds");
    runtime.shutdown().await.expect("stops");
}
