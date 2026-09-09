// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The vendored AutoNAT v2 client's `retest`, which upstream lacks.
//!
//! `third_party/libp2p-autonat/INTERWEAVE.patch` adds one method, and
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
fn an_abandoned_probe_reports_no_outcome() {
    // The second half of the patch. `AddressNotReachable` is the one arm
    // that falls through to `GenerateEvent` rather than returning, so a
    // late failure for a nonce `retest` abandoned would otherwise be
    // reported as the outcome of whichever probe replaced it. There is no
    // public way to inject a handler event, so this drives the property
    // the guard rests on: after `retest`, no candidate holds the old
    // nonce, which is exactly what `reset_status_to` looks for and now
    // reports.
    let mut client = Behaviour::default();
    let addr: Multiaddr = "/ip4/203.0.113.9/tcp/4001".parse().expect("a literal");
    client.on_swarm_event(FromSwarm::NewExternalAddrCandidate(
        NewExternalAddrCandidate { addr: &addr },
    ));
    client.validate_addr(&addr);

    // A second `retest` answers `false` precisely because the first one
    // left nothing holding a nonce -- the same lookup the guard makes.
    assert!(client.retest(&addr));
    assert!(!client.retest(&addr));
}
