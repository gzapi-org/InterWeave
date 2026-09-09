// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The vendored AutoNAT v2 client's `retest`, which upstream lacks.
//!
//! `third_party/libp2p-autonat/INTERWEAVE.patch` adds one method and a
//! guard beside it, and
//! this is the test that fails if the patch is dropped or the method
//! stops returning a candidate to the sweep (ADR-0051). It drives the
//! behaviour directly rather than through a Swarm: the transition it
//! proves is the one the public API exposes, and `validate_addr` is
//! upstream's own hook for putting a candidate into `Received`.

use libp2p::Multiaddr;
use libp2p::autonat::v2::client::Behaviour;
use libp2p::swarm::{FromSwarm, NetworkBehaviour, NewExternalAddrCandidate};

#[test]
fn a_tested_candidate_returns_to_the_sweep_and_an_untested_one_is_left_alone() {
    let mut client = Behaviour::default();
    let addr: Multiaddr = "/ip4/203.0.113.9/tcp/4001".parse().expect("a literal");

    assert!(
        !client.retest(&addr),
        "an address the client has never seen"
    );

    client.on_swarm_event(FromSwarm::NewExternalAddrCandidate(
        NewExternalAddrCandidate { addr: &addr },
    ));
    assert!(!client.retest(&addr), "already untested, nothing to change");

    client.validate_addr(&addr);
    assert!(client.retest(&addr), "received -> untested is the patch");
    assert!(
        !client.retest(&addr),
        "and it stays untested until the sweep tests it again"
    );
}

#[test]
fn retest_leaves_no_candidate_holding_the_old_nonce() {
    // The PRECONDITION the patch's second half rests on, and not the
    // half itself: `reset_status_to` declines to report an outcome when
    // no candidate still holds the nonce, and this shows `retest` is what
    // puts it in that state. The decline itself is NOT tested here --
    // nothing outside the crate can inject a handler event or reach
    // `Pending`, so proving it needs the two-Swarm harness the vendored
    // crate ships and this workspace does not run. Named for what it
    // observes; an earlier name claimed the outcome case. Review finding
    // on PR #85.
    let mut client = Behaviour::default();
    let addr: Multiaddr = "/ip4/203.0.113.9/tcp/4001".parse().expect("a literal");
    client.on_swarm_event(FromSwarm::NewExternalAddrCandidate(
        NewExternalAddrCandidate { addr: &addr },
    ));
    client.validate_addr(&addr);

    // `retest` reports `true` once and then `false`, because it tests
    // `status != Untested` -- which is NOT the lookup `reset_status_to`
    // makes (`is_pending_with_nonce || is_received_with_nonce`). The two
    // coincide on this input and diverge on a `Failed` candidate, which
    // holds no nonce yet still answers `true` here. Stated because an
    // earlier comment called them the same lookup.
    assert!(client.retest(&addr));
    assert!(!client.retest(&addr));
}
