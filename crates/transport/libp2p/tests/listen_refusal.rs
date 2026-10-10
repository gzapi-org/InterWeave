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

/// A listener whose socket the platform refuses is `ListenDenied`, not
/// a generic transport failure, so the embedded host can tell the person
/// to grant access. A port below 1024 without the privilege is refused
/// with EACCES here, the error kind an ungranted INTERNET permission
/// gives on Android 17 as EPERM. The control is the test above: an
/// address this host does not hold stays `Transport`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_listener_the_platform_refuses_is_denied() {
    let runtime = runtime();
    let privileged: Multiaddr = "/ip4/127.0.0.1/tcp/80".parse().expect("multiaddr");
    match runtime.listen(privileged).await {
        Err(SubstrateError::ListenDenied(detail)) => {
            assert!(
                detail.contains("(os error "),
                "the OS's own words: {detail:?}"
            );
        }
        // Run with the privilege (root, or a lowered
        // `ip_unprivileged_port_start`) the bind succeeds and the case
        // cannot be reached: said, not passed silently.
        Ok(bound) => panic!("bound {bound}: this test needs an unprivileged runner"),
        Err(other) => panic!("a refused socket is ListenDenied: {other:?}"),
    }
    runtime.shutdown().await.expect("stops");
}
