// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The question `check_dialable_hosts.sh` says it cannot ask.
//!
//! That guard holds the root manifest's libp2p feature array and
//! `profile-config`'s `DIALABLE_HOST_PROTOCOLS` together, and its own
//! help says why it stops short: enabling the `dns` feature makes the
//! transport AVAILABLE, and the Swarm builder must still wrap the base
//! transport in it. A change that turned the feature on, widened the
//! array and forgot the builder would pass it.
//!
//! An earlier version tried to close that by grepping
//! `runtime/mod.rs` for the construction. Five review rounds found
//! seven ways the search was wrong -- four shapes that satisfied it
//! while the builder constructed nothing (a `#[cfg(test)]` item, a
//! rustfmt-split `use`, a block comment quoting real code, a string
//! literal quoting real code) and three real constructions it missed.
//! The search was deleted rather than patched an eighth time, because
//! "does this code call this function" is a question about types and
//! `grep` answers a question about text.
//!
//! This is what replaced it: build the real transport, dial a `/dns4`
//! address, and read the error kind. No lexical shape satisfies it.
//!
//! # Why `.invalid`
//!
//! RFC 6761 reserves `.invalid` as guaranteed never to resolve, so the
//! lookup fails deterministically without reaching any real name or
//! depending on what a network happens to serve. The test asserts what
//! the failure IS, not that the network is absent.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::{SubstrateConfig, SwarmEvent, SwarmRuntime};
use interweave_transport_runtime::TrustSources;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};

/// Every wait is bounded: a hung substrate must fail the suite rather
/// than hold CI until the job timeout, where the cause is invisible.
const PATIENCE: Duration = Duration::from_secs(20);

fn trusting(peer: &TransportIdentity) -> TrustSources {
    TrustSources::new(
        PeerTrustPolicy::new([peer.clone()]).expect("one peer"),
        InfrastructureSet::default(),
    )
}

/// Drive the runtime until a `DialFailed` for `peer` arrives.
async fn dial_failure(runtime: &mut SwarmRuntime, peer: &TransportIdentity) -> String {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "no dial failure arrived");
        match tokio::time::timeout(remaining, runtime.next_event()).await {
            Ok(Some(SwarmEvent::DialFailed { peer: who, detail }))
                if who.as_ref() == Some(peer) =>
            {
                return detail;
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => panic!("the runtime stopped before the dial failed"),
        }
    }
}

#[tokio::test]
async fn a_dns_address_is_dialable_by_the_transport_this_runtime_builds() {
    // THE ASSERTION THE GUARD ASKED FOR. With the base transport
    // unwrapped, libp2p answers `MultiaddrNotSupported` -- no configured
    // transport understands `/dns4` -- and `attempt_is_structural`
    // classifies that as structural, so `record_permanent_failure` drops
    // the address from the book rather than retrying it. With the
    // transport built, the name is RESOLVED and the failure is an
    // ordinary lookup diagnostic instead.
    //
    // So the test is not "does a dial fail" -- it fails either way. It
    // is WHICH failure, which is the only thing that distinguishes a
    // built transport from a feature flag.
    let dialer_id = ProfileIdentity::generate();
    let target_id = ProfileIdentity::generate();
    let target_peer = target_id.transport_identity().expect("peer id");

    let mut runtime = SwarmRuntime::start(
        &dialer_id,
        SubstrateConfig::default(),
        trusting(&target_peer),
    )
    .expect("the runtime starts, which itself requires the DNS transport to construct");

    let address: libp2p::Multiaddr = "/dns4/interweave-dns-transport.invalid/tcp/4001"
        .parse()
        .expect("a well-formed dns4 address");
    runtime
        .dial(target_peer.clone(), address)
        .await
        .expect("the command reaches the swarm task")
        .expect("the gate admits the dial: this test is about the TRANSPORT, not admission");

    let detail = dial_failure(&mut runtime, &target_peer).await;

    // TWO ASSERTIONS, EACH ON A DISPLAY STRING, and in this order.
    //
    // FIRST, THE ONE THAT DISCRIMINATES. With the DNS wrap removed the
    // failure displays "Multiaddr is not supported: /dns4/..." --
    // measured, not assumed. The first version of this test matched the
    // variant NAME, `MultiaddrNotSupported`, which never appears in the
    // display, so it passed with the transport removed.
    assert!(
        !detail.contains("Multiaddr is not supported"),
        "libp2p answered that no configured transport understands a /dns4 address: the \
         base transport is NOT wrapped in the DNS transport -- the half-done change \
         `check_dialable_hosts.sh` says it cannot detect. Got: {detail}"
    );
    // SECOND, THE POSITIVE: a lookup was attempted. Not "DNS error"
    // alone, which is ONE of the resolver's outcomes -- a nameserver's
    // answer -- and a runner with no DNS egress, a firewalled resolver or
    // a slow one produces another. The #111 re-review caught that the
    // first positive would have failed on such a runner while its message
    // said the transport was unwrapped. These are the pinned resolver's
    // own displays (`hickory-net 0.26.3` `src/error.rs`), and a shape not
    // among them fails as UNRECOGNISED, which is the true statement.
    //
    // PLUS ONE THAT IS NOT THE RESOLVER'S. The builder wraps the WHOLE
    // transport, resolution included, in the handshake timeout
    // (`libp2p 0.57.0` `builder/phase/build.rs:28`), ten seconds here,
    // and hickory's resolv.conf defaults -- five seconds, two attempts --
    // are ten too. On a runner whose resolver silently drops packets the
    // outer timeout wins or ties, and libp2p-core displays "Timeout has
    // been reached" (`transport/timeout.rs:197`) -- #111 DNS review P2-1.
    // It still discriminates: an unwrapped transport answers at once,
    // never by timing out. What it also records: the handshake budget
    // caps name resolution in production.
    const RESOLVER_OUTCOMES: [&str; 7] = [
        "DNS error",
        "request timed out",
        "io error",
        "no connections available",
        "protocol error",
        "resource too busy",
        "Timeout has been reached",
    ];
    assert!(
        RESOLVER_OUTCOMES.iter().any(|shape| detail.contains(shape)),
        "the dial failed in a way that is neither the unwrapped transport's nor any \
         outcome the pinned resolver displays. Read the detail and extend \
         RESOLVER_OUTCOMES only if it IS a resolver's answer. Got: {detail}"
    );

    runtime.shutdown().await.expect("clean shutdown");
}
