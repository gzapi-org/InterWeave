// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Stage 5: every outbound dial passes root admission, over real sockets.
//!
//! Loopback TCP and a real Swarm rather than a mock. The clauses under
//! test are about what the *substrate* does with policy, and a mocked
//! transport would prove only that the translation layer compiles —
//! which is exactly how `policy.admit(&request, class, 0)` survived
//! review with a literal zero for the clock.

#![allow(clippy::expect_used, clippy::panic)]

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::{DialRefusal, SubstrateConfig, SwarmRuntime};
use interweave_transport_runtime::{
    ConnectionClass, ConnectionManager, ConnectionPolicy, DialDenial, DialOrigin, DialRequest,
};
use libp2p::Multiaddr;

/// A peer id nothing is listening for, so a dial to it always fails.
const ABSENT: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";

fn identity() -> ProfileIdentity {
    ProfileIdentity::generate()
}

fn config() -> SubstrateConfig {
    SubstrateConfig {
        max_pending_dials: 2,
        ..SubstrateConfig::default()
    }
}

#[tokio::test]
async fn the_pending_dial_ceiling_is_enforced_by_the_substrate() {
    // The ceiling is only real if a ticket is HELD for the life of the
    // dial. Releasing it when the dial began would bound nothing but
    // the rate of the loop, and the count would read zero between
    // iterations however many connections were in flight.
    //
    // Two slots, three dials to an address that will not answer within
    // the test: the third must be refused by policy rather than by the
    // network.
    let runtime = SwarmRuntime::start(&identity(), config()).expect("starts");

    // TEST-NET-1 (RFC 5737). Routed nowhere, so the dial stays pending
    // rather than failing fast the way a closed loopback port does.
    let unreachable: Multiaddr = "/ip4/192.0.2.1/tcp/4001".parse().expect("valid");
    let absent = TransportIdentity::parse(ABSENT).expect("canonical");

    let first = runtime
        .dial(absent.clone(), unreachable.clone())
        .await
        .expect("the command reaches the task");
    assert!(first.is_ok(), "the first slot: {first:?}");
    let second = runtime
        .dial(absent.clone(), unreachable.clone())
        .await
        .expect("the command reaches the task");
    assert!(second.is_ok(), "the second slot: {second:?}");

    let third = runtime
        .dial(absent.clone(), unreachable.clone())
        .await
        .expect("the command reaches the task");
    match third {
        Err(DialRefusal::Policy(DialDenial::TooManyPendingDials)) => {}
        other => panic!("the third dial must be refused by the ceiling, got {other:?}"),
    }

    runtime.shutdown().await.expect("shuts down");
}

// A DRAIN TEST IS DELIBERATELY ABSENT HERE. `shutdown` consumes the
// runtime, so there is no handle left to dial with and nothing an
// end-to-end test could observe. The property -- that a draining
// manager refuses admission, and that a holder of an existing snapshot
// handle sees the refusal without refreshing -- is proved in
// `connection_manager::tests::revoking_authorization_reaches_a_holder_
// that_never_asked`, at the layer where it is actually observable.
//
// Writing one here anyway would have meant a test that dials, shuts
// down, and asserts nothing: the shape that passes forever and means
// nothing, which is what this suite exists to avoid.

#[test]
fn a_denied_autonomous_dial_leaves_retry_state_exactly_as_it_was() {
    // ADR-0011's second exit-gate clause: "denied autonomous-behaviour
    // dial attempts cannot reset backoff."
    //
    // Proved at the manager rather than through the Swarm, and the
    // reason is worth stating rather than hiding: Stage 4's behaviour
    // set is TCP, Noise, Yamux and Identify, none of which dials. There
    // is no behaviour-originated dial in this build to drive, so a test
    // claiming to exercise that path would be exercising the command
    // path with a different origin label and calling it coverage.
    //
    // What IS proved here is the property the clause rests on, for
    // every autonomous origin: a refusal produces no ticket, and
    // recording an outcome requires one. The behaviour path inherits it
    // structurally when it arrives, because `handle_pending_outbound_
    // connection` will have the same ticket to obtain and the same
    // absence of any other way to record.
    let policy = ConnectionPolicy::new(8, 8);
    let mut manager = ConnectionManager::new(policy, 8);
    let peer = TransportIdentity::parse(ABSENT).expect("canonical");

    // Put the peer into backoff through an ordinary failure.
    let ticket = manager
        .handle()
        .load()
        .admit(
            &DialRequest {
                peer: Some(peer.clone()),
                address: "/ip4/192.0.2.1/tcp/4001".to_owned(),
                origin: DialOrigin::ConnectionManager,
            },
            ConnectionClass::DataPlaneTrusted,
            1_000,
        )
        .expect("admitted");
    manager.record_failure(ticket, 1_000);

    let scheduled = manager.scheduled_retries();
    let revision = manager.revision();
    assert_eq!(scheduled, 1, "the peer is now scheduled");

    // EVERY autonomous origin, refused, one after another. If any of
    // them cleared or advanced the schedule, an attacker who can
    // provoke Kademlia lookups or relay attempts against a peer could
    // hold that peer permanently un-retried -- or retried instantly and
    // forever, depending on which direction the state moved.
    for origin in [
        DialOrigin::KademliaQuery,
        DialOrigin::RelayReservation,
        DialOrigin::RelayCircuit,
        DialOrigin::AutonatProbe,
        DialOrigin::DcutrHolePunch,
        DialOrigin::DiscoveryReconnect,
    ] {
        let denied = manager.handle().load().admit(
            &DialRequest {
                peer: Some(peer.clone()),
                address: "/ip4/192.0.2.1/tcp/4001".to_owned(),
                origin,
            },
            ConnectionClass::DataPlaneTrusted,
            2_000,
        );
        assert!(
            denied.is_err(),
            "{origin:?} must be refused while in backoff"
        );
        assert_eq!(
            manager.scheduled_retries(),
            scheduled,
            "{origin:?} changed the retry table"
        );
        assert_eq!(
            manager.revision(),
            revision,
            "{origin:?} caused a republish, so something mutated"
        );
        assert!(
            manager.due_retries(2_000).is_empty(),
            "{origin:?} brought the retry forward"
        );
    }

    // And the schedule still elapses on its own terms afterwards.
    assert_eq!(
        manager.due_retries(u64::MAX),
        vec![peer],
        "the retry survived every refusal"
    );
}

/// Wait for the runtime to report a connection, or give up.
///
/// Bounded rather than `loop`: a test that hangs on a missing event is
/// a test that reports nothing at all in CI.
async fn wait_connected(runtime: &mut SwarmRuntime) -> TransportIdentity {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let next = tokio::time::timeout_at(deadline, runtime.next_event())
            .await
            .expect("a connection arrives within the deadline");
        match next {
            Some(interweave_transport_libp2p::SwarmEvent::Connected { peer }) => return peer,
            Some(_) => {}
            None => panic!("the substrate stopped before connecting"),
        }
    }
}

#[tokio::test]
async fn the_dial_goes_to_the_admitted_address() {
    // THE ADDRESS HALF OF THE BINDING. `DialOpts::get_addresses` is
    // crate-private in libp2p, so the only honest way to observe where
    // a dial went is the connection it made. Build the dial from
    // anything but the ticket's own address and no connection happens.
    let listener = SwarmRuntime::start(&identity(), SubstrateConfig::default()).expect("starts");
    let bound = listener
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("binds");
    let listener_peer = listener.local_peer().clone();

    let mut dialer = SwarmRuntime::start(&identity(), SubstrateConfig::default()).expect("starts");
    let answer = dialer
        .dial(listener_peer.clone(), bound)
        .await
        .expect("the command reaches the task");
    assert!(answer.is_ok(), "admitted and dialed: {answer:?}");

    assert_eq!(
        wait_connected(&mut dialer).await,
        listener_peer,
        "the dial reached the peer the admission named, at the address it named"
    );

    dialer.shutdown().await.expect("shuts down");
    listener.shutdown().await.expect("shuts down");
}

#[tokio::test]
async fn an_established_connection_counts_against_the_connection_ceiling() {
    // `ConnectionPolicy::connections` was never incremented, so
    // `max_connections` bounded nothing at all: the check compared a
    // configured limit against a permanent zero. One slot, one
    // connection, and the next dial must be refused by capacity rather
    // than by the network.
    let listener = SwarmRuntime::start(&identity(), SubstrateConfig::default()).expect("starts");
    let bound = listener
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("binds");
    let listener_peer = listener.local_peer().clone();

    let mut dialer = SwarmRuntime::start(
        &identity(),
        SubstrateConfig {
            max_connections: 1,
            ..SubstrateConfig::default()
        },
    )
    .expect("starts");

    let first = dialer
        .dial(listener_peer.clone(), bound)
        .await
        .expect("the command reaches the task");
    assert!(first.is_ok(), "the only slot: {first:?}");
    let _ = wait_connected(&mut dialer).await;

    // TEST-NET-1 again: this must be refused before a socket is opened,
    // so where it points is irrelevant to the assertion and relevant to
    // the test not depending on the network.
    let absent = TransportIdentity::parse(ABSENT).expect("canonical");
    let second = dialer
        .dial(absent, "/ip4/192.0.2.1/tcp/4001".parse().expect("valid"))
        .await
        .expect("the command reaches the task");
    match second {
        Err(DialRefusal::Policy(DialDenial::ConnectionLimitReached)) => {}
        other => panic!("the established connection must fill the ceiling, got {other:?}"),
    }

    dialer.shutdown().await.expect("shuts down");
    listener.shutdown().await.expect("shuts down");
}

#[tokio::test]
async fn an_identity_mismatch_quarantines_the_address_and_not_the_peer() {
    // ADR-0011: an address that authenticates somebody else is not an
    // unreachable route to be retried on backoff -- it is quarantined,
    // and the expected peer's own backoff is left alone so one injected
    // address cannot suppress that peer's real routes.
    //
    // The two branches are distinguishable from outside precisely
    // because they refuse differently: `record_failure` sets PEER
    // backoff, `record_identity_mismatch` sets ADDRESS quarantine. The
    // generic path that discarded the error therefore produces
    // `PeerBackoff` here, and this asserts the other one.
    let listener = SwarmRuntime::start(&identity(), SubstrateConfig::default()).expect("starts");
    let bound = listener
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("binds");

    let dialer = SwarmRuntime::start(&identity(), SubstrateConfig::default()).expect("starts");
    // The listener is real and answering -- with a key that is not this
    // one. That is what makes libp2p report `WrongPeerId` rather than a
    // transport error.
    let wrong = TransportIdentity::parse(ABSENT).expect("canonical");
    let first = dialer
        .dial(wrong.clone(), bound.clone())
        .await
        .expect("the command reaches the task");
    assert!(
        first.is_ok(),
        "admitted, and it is the handshake that fails"
    );

    // The outcome arrives as an event; the policy is updated before it
    // is translated, so seeing the failure means the recording happened.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut dialer = dialer;
    loop {
        match tokio::time::timeout_at(deadline, dialer.next_event())
            .await
            .expect("the failure arrives within the deadline")
        {
            Some(interweave_transport_libp2p::SwarmEvent::DialFailed { .. }) => break,
            Some(_) => {}
            None => panic!("the substrate stopped before failing"),
        }
    }

    let second = dialer
        .dial(wrong, bound)
        .await
        .expect("the command reaches the task");
    match second {
        Err(DialRefusal::Policy(DialDenial::AddressQuarantined)) => {}
        other => panic!("a mismatched identity must quarantine the address, got {other:?}"),
    }

    dialer.shutdown().await.expect("shuts down");
    listener.shutdown().await.expect("shuts down");
}

#[tokio::test]
async fn a_closed_connection_gives_its_slot_back() {
    // A ceiling that only counts up is a LIFETIME QUOTA, not a
    // concurrency bound: a node would dial `max_connections` peers over
    // its whole run and then never dial again, however many of those
    // connections had long since closed.
    let listener = SwarmRuntime::start(&identity(), SubstrateConfig::default()).expect("starts");
    let bound = listener
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("binds");
    let listener_peer = listener.local_peer().clone();

    let mut dialer = SwarmRuntime::start(
        &identity(),
        SubstrateConfig {
            max_connections: 1,
            ..SubstrateConfig::default()
        },
    )
    .expect("starts");

    let first = dialer
        .dial(listener_peer.clone(), bound)
        .await
        .expect("the command reaches the task");
    assert!(first.is_ok(), "the only slot: {first:?}");
    let _ = wait_connected(&mut dialer).await;

    // Take the far end away, so the connection closes for a reason the
    // dialer did not choose -- which is how they close in practice.
    listener.shutdown().await.expect("shuts down");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match tokio::time::timeout_at(deadline, dialer.next_event())
            .await
            .expect("the disconnection arrives within the deadline")
        {
            Some(interweave_transport_libp2p::SwarmEvent::Disconnected { .. }) => break,
            Some(_) => {}
            None => panic!("the substrate stopped before disconnecting"),
        }
    }

    let absent = TransportIdentity::parse(ABSENT).expect("canonical");
    let again = dialer
        .dial(absent, "/ip4/192.0.2.1/tcp/4001".parse().expect("valid"))
        .await
        .expect("the command reaches the task");
    assert!(
        again.is_ok(),
        "the freed slot must be usable again, got {again:?}"
    );

    dialer.shutdown().await.expect("shuts down");
}
