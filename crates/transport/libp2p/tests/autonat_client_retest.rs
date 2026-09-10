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
fn retesting_one_candidate_leaves_the_others_where_they_were() {
    // A SECOND OBSERVATION, which this was not. Its assertions used to be
    // a strict subset of the test above -- `retest` true then false on one
    // address, which that test already covers -- so it read as a second
    // case and added no coverage. A review said so.
    //
    // What is actually worth pinning is that `retest` is keyed on the
    // address. The manager re-tests one address at a time, and a method
    // that reset the whole candidate map would restart probes nobody
    // asked to restart -- multiplying the dial-backs this patch already
    // costs an authorized server, which is the bound ADR-0051 records.
    //
    // STILL NOT TESTED HERE, and named rather than implied: the patch's
    // second half, where `reset_status_to` declines to report an outcome
    // for a candidate that no longer holds the nonce.
    //
    // IT IS NOT UNTESTABLE, which an earlier version of this comment said.
    // A probe in flight cannot be SYNTHESISED -- `handler` is private and
    // `dial_request` is `pub(crate)`, so `ToBehaviour::PeerHasServerSupport`
    // cannot be injected and the server pick returns `None` without it. But
    // it can be CAUSED: the vendored crate ships `v2::server::Behaviour`,
    // reachable as `libp2p::autonat::v2::server`, so a two-Swarm test over
    // real sockets would negotiate a real dial request and reach a genuine
    // `Pending`. That test is owed by the step that constructs these
    // behaviours; this PR constructs nothing, which is why it is not here.
    // Calling it impossible would have told that step not to try. Review
    // finding on PR #85.
    let mut client = Behaviour::default();
    let one: Multiaddr = "/ip4/203.0.113.9/tcp/4001".parse().expect("a literal");
    let two: Multiaddr = "/ip4/203.0.113.10/tcp/4001".parse().expect("a literal");

    for addr in [&one, &two] {
        client.on_swarm_event(FromSwarm::NewExternalAddrCandidate(
            NewExternalAddrCandidate { addr },
        ));
        client.validate_addr(addr);
    }

    assert!(
        client.retest(&one),
        "the named address returns to the sweep"
    );
    assert!(
        client.retest(&two),
        "and the other is still tested, so it was not reset as a side effect"
    );
    assert!(
        !client.retest(&two),
        "which the second call confirms: it had been left in `Received`"
    );
}
