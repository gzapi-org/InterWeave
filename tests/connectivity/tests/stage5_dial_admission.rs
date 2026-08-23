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
use interweave_transport_runtime::TrustSources;
use interweave_transport_runtime::preauth::PreAuthLimitsBuilder;
use interweave_transport_runtime::{
    ConnectionManager, ConnectionPolicy, DialDenial, DialOrigin, DialRequest,
};
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::Multiaddr;

/// A peer id nothing is listening for, so a dial to it always fails.
const ABSENT: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";

fn identity() -> ProfileIdentity {
    ProfileIdentity::generate()
}

/// A fresh identity and the peer id it will present.
///
/// Both are needed at once now: a runtime is started with the peers it
/// trusts, and the peer id has to exist before the runtime that trusts
/// it does.
fn who() -> (ProfileIdentity, TransportIdentity) {
    let id = identity();
    let peer = id.transport_identity().expect("peer id");
    (id, peer)
}

/// Trust exactly these peers on the application data plane.
///
/// There is no allow-all constructor, by ADR-0012's design: the default
/// admits nobody. Every test below that connects therefore says who it
/// trusts, and one that forgot would fail with `Unauthorized` rather
/// than quietly proving something else.
fn trusting(peers: &[&TransportIdentity]) -> TrustSources {
    TrustSources {
        peers: PeerTrustPolicy::new(peers.iter().map(|p| (*p).clone())).expect("a handful"),
        infrastructure: InfrastructureSet::default(),
    }
}

/// Trust nobody, which is what a freshly configured profile does.
fn trusting_nobody() -> TrustSources {
    TrustSources::default()
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
    let absent_peer = TransportIdentity::parse(ABSENT).expect("canonical");
    let runtime =
        SwarmRuntime::start(&identity(), config(), trusting(&[&absent_peer])).expect("starts");

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
    // Trusted, so every refusal below is the BACKOFF refusing and not
    // the classification: an unauthorized peer is denied for a reason
    // that would make this test pass without proving anything.
    let _ = manager.set_trust(trusting(&[&peer]), &[]);

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
    let (dialer_id, dialer_peer) = who();
    let listener = SwarmRuntime::start(
        &identity(),
        SubstrateConfig::default(),
        trusting(&[&dialer_peer]),
    )
    .expect("starts");
    let bound = listener
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("binds");
    let listener_peer = listener.local_peer().clone();

    let mut dialer = SwarmRuntime::start(
        &dialer_id,
        SubstrateConfig::default(),
        trusting(&[&listener_peer]),
    )
    .expect("starts");
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
    let (dialer_id, dialer_peer) = who();
    let absent = TransportIdentity::parse(ABSENT).expect("canonical");
    let listener = SwarmRuntime::start(
        &identity(),
        SubstrateConfig::default(),
        trusting(&[&dialer_peer]),
    )
    .expect("starts");
    let bound = listener
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("binds");
    let listener_peer = listener.local_peer().clone();

    let mut dialer = SwarmRuntime::start(
        &dialer_id,
        SubstrateConfig {
            max_connections: 1,
            ..SubstrateConfig::default()
        },
        trusting(&[&listener_peer, &absent]),
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
    let wrong = TransportIdentity::parse(ABSENT).expect("canonical");
    let listener = SwarmRuntime::start(&identity(), SubstrateConfig::default(), trusting_nobody())
        .expect("starts");
    let bound = listener
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("binds");

    // The dialer trusts the peer it believes is there, so the refusal
    // under test is the identity check rather than the gate.
    let dialer = SwarmRuntime::start(&identity(), SubstrateConfig::default(), trusting(&[&wrong]))
        .expect("starts");
    // The listener is real and answering -- with a key that is not this
    // one. That is what makes libp2p report `WrongPeerId` rather than a
    // transport error.
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
    let (dialer_id, dialer_peer) = who();
    let absent = TransportIdentity::parse(ABSENT).expect("canonical");
    let listener = SwarmRuntime::start(
        &identity(),
        SubstrateConfig::default(),
        trusting(&[&dialer_peer]),
    )
    .expect("starts");
    let bound = listener
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("binds");
    let listener_peer = listener.local_peer().clone();

    let mut dialer = SwarmRuntime::start(
        &dialer_id,
        SubstrateConfig {
            max_connections: 1,
            ..SubstrateConfig::default()
        },
        trusting(&[&listener_peer, &absent]),
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

#[tokio::test]
async fn a_second_dial_cannot_slip_under_the_ceiling_while_the_first_is_pending() {
    // The ceiling counted connections only once they ESTABLISHED, so
    // every dial admitted before the first one connected saw a count of
    // zero. One slot admitted as many concurrent dials as the pending
    // budget allowed -- and every one of them opened a socket.
    let absent = TransportIdentity::parse(ABSENT).expect("canonical");
    let runtime = SwarmRuntime::start(
        &identity(),
        SubstrateConfig {
            max_connections: 1,
            max_pending_dials: 8,
            ..SubstrateConfig::default()
        },
        trusting(&[&absent]),
    )
    .expect("starts");

    // TEST-NET-1: routed nowhere, so the first dial is still pending
    // when the second is asked for.
    let unreachable: Multiaddr = "/ip4/192.0.2.1/tcp/4001".parse().expect("valid");

    let first = runtime
        .dial(absent.clone(), unreachable.clone())
        .await
        .expect("the command reaches the task");
    assert!(first.is_ok(), "the only connection slot: {first:?}");

    let second = runtime
        .dial(absent, "/ip4/192.0.2.2/tcp/4001".parse().expect("valid"))
        .await
        .expect("the command reaches the task");
    match second {
        Err(DialRefusal::Policy(DialDenial::ConnectionLimitReached)) => {}
        other => panic!("nothing has connected yet, and that is the point: {other:?}"),
    }

    runtime.shutdown().await.expect("shuts down");
}

#[tokio::test]
async fn an_inbound_connection_over_the_ceiling_is_not_kept() {
    // Inbound arrives with no admission to reserve its slot, so a bound
    // that counted only outbound would bound nothing on the node that
    // accepts -- which is the node an attacker gets to choose.
    let (first_id, first_peer) = who();
    let (second_id, second_peer) = who();
    let listener = SwarmRuntime::start(
        &identity(),
        SubstrateConfig {
            max_connections: 1,
            ..SubstrateConfig::default()
        },
        // BOTH trusted, so what refuses the second connection is the
        // ceiling and not the trust policy.
        trusting(&[&first_peer, &second_peer]),
    )
    .expect("starts");
    let bound = listener
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("binds");
    let listener_peer = listener.local_peer().clone();

    let mut first = SwarmRuntime::start(
        &first_id,
        SubstrateConfig::default(),
        trusting(&[&listener_peer]),
    )
    .expect("starts");
    assert!(
        first
            .dial(listener_peer.clone(), bound.clone())
            .await
            .expect("the command reaches the task")
            .is_ok()
    );
    let _ = wait_connected(&mut first).await;

    // The ceiling is now full on the listener. The second peer connects
    // as far as the handshake -- refusing earlier is a Stage 6 concern,
    // and pre-auth admission is a separate bound -- and must not be
    // KEPT.
    let mut second = SwarmRuntime::start(
        &second_id,
        SubstrateConfig::default(),
        trusting(&[&listener_peer]),
    )
    .expect("starts");
    assert!(
        second
            .dial(listener_peer, bound)
            .await
            .expect("the command reaches the task")
            .is_ok()
    );

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match tokio::time::timeout_at(deadline, second.next_event())
            .await
            .expect("the close arrives within the deadline")
        {
            Some(interweave_transport_libp2p::SwarmEvent::Disconnected { .. }) => break,
            Some(_) => {}
            None => panic!("the substrate stopped before the connection closed"),
        }
    }

    second.shutdown().await.expect("shuts down");
    first.shutdown().await.expect("shuts down");
    listener.shutdown().await.expect("shuts down");
}

/// Wait for the runtime to report a dial failure.
async fn wait_dial_failed(runtime: &mut SwarmRuntime) -> String {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match tokio::time::timeout_at(deadline, runtime.next_event())
            .await
            .expect("a dial failure arrives within the deadline")
        {
            Some(interweave_transport_libp2p::SwarmEvent::DialFailed { detail, .. }) => {
                return detail;
            }
            Some(_) => {}
            None => panic!("the substrate stopped before failing"),
        }
    }
}

#[tokio::test]
async fn a_poisoned_address_does_not_suppress_the_peer_s_good_route() {
    // THE POISONING TEST STAGE 5 REQUIRES, in the plan's own words: for
    // a trusted PeerId with both a known-good address and an
    // attacker-supplied wrong-key address, the wrong address must be
    // quarantined without suppressing the known-good route through
    // peer-wide punitive backoff.
    //
    // The attack it describes is cheap. Anyone who can get one bogus
    // address associated with a trusted peer -- a stale record, a
    // hostile directory answer, a peer that moved -- could otherwise
    // put that peer into peer-scoped backoff on demand and keep it
    // there, cutting a route that was working the whole time.
    //
    // Over real sockets, because the distinction being tested is
    // between what libp2p reports for a wrong key and what it reports
    // for an unreachable address, and a mock would be asserting the
    // shape of the author's own assumption.
    let (dialer_id, dialer_peer) = who();
    let good = SwarmRuntime::start(
        &identity(),
        SubstrateConfig::default(),
        trusting(&[&dialer_peer]),
    )
    .expect("starts");
    let good_address = good
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("binds");
    let trusted = good.local_peer().clone();

    // The impostor: a real listener, answering, with a different key.
    let impostor = SwarmRuntime::start(&identity(), SubstrateConfig::default(), trusting_nobody())
        .expect("starts");
    let poisoned_address = impostor
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("binds");
    assert_ne!(
        impostor.local_peer(),
        &trusted,
        "the impostor is a different identity, which is the whole scenario"
    );

    // The dialer trusts the peer it is looking for, and nothing else:
    // the impostor is refused by the identity check, not by trust.
    let mut dialer = SwarmRuntime::start(
        &dialer_id,
        SubstrateConfig::default(),
        trusting(&[&trusted]),
    )
    .expect("starts");

    // The good route works first, so "known-good" is a fact about this
    // run rather than an assumption.
    assert!(
        dialer
            .dial(trusted.clone(), good_address.clone())
            .await
            .expect("the command reaches the task")
            .is_ok()
    );
    assert_eq!(wait_connected(&mut dialer).await, trusted);

    // Now the poisoned one, dialed as the SAME trusted peer.
    assert!(
        dialer
            .dial(trusted.clone(), poisoned_address.clone())
            .await
            .expect("the command reaches the task")
            .is_ok(),
        "admitted -- the address has no history yet, and finding out is the point"
    );
    let detail = wait_dial_failed(&mut dialer).await;
    assert!(
        detail.contains("Unexpected peer ID"),
        "the impostor must fail the IDENTITY check, not the transport -- a          connection refused or a timeout would quarantine nothing and would          make the assertions below pass for the wrong reason: {detail}"
    );

    // THE CLAUSE. The poisoned address is refused from now on...
    match dialer
        .dial(trusted.clone(), poisoned_address)
        .await
        .expect("the command reaches the task")
    {
        Err(DialRefusal::Policy(DialDenial::AddressQuarantined)) => {}
        other => panic!("the poisoned address must be quarantined, got {other:?}"),
    }

    // ...and the route that has been working all along is untouched.
    let again = dialer
        .dial(trusted, good_address)
        .await
        .expect("the command reaches the task");
    assert!(
        again.is_ok(),
        "one poisoned address must not cost the peer its good route, got {again:?}"
    );

    dialer.shutdown().await.expect("shuts down");
    impostor.shutdown().await.expect("shuts down");
    good.shutdown().await.expect("shuts down");
}

#[tokio::test]
async fn a_source_past_its_pre_auth_rate_is_refused_before_noise() {
    // Stage 5's third exit clause: pre-Noise work is bounded. Until the
    // gate was consulted from `handle_pending_inbound_connection`, the
    // whole `preauth` module was a state machine nothing called -- the
    // exact shape this repository has shipped before and the reason
    // CLAUDE.md says a claim owes a test.
    //
    // Two starts per window from one bucket, then a third from the same
    // bucket. Every peer here is on 127.0.0.1, which is one bucket by
    // design: the accounting is by transport source, not by identity,
    // precisely because an unauthenticated party has no identity yet.
    let limits = PreAuthLimitsBuilder {
        max_attempts_per_window: 2,
        ..PreAuthLimitsBuilder::default()
    }
    .build()
    .expect("two starts a minute is a narrowing of the specified policy");

    // Every dialer is TRUSTED by the listener, so the refusal under
    // test is the rate and not the trust policy -- and so the two that
    // get in stay in, which is the second half of the assertion.
    let dialers: Vec<(ProfileIdentity, TransportIdentity)> = (0..3).map(|_| who()).collect();
    let trusted: Vec<&TransportIdentity> = dialers.iter().map(|(_, p)| p).collect();

    let listener = SwarmRuntime::start(
        &identity(),
        SubstrateConfig {
            preauth: limits,
            ..SubstrateConfig::default()
        },
        trusting(&trusted),
    )
    .expect("starts");
    let bound = listener
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("binds");
    let listener_peer = listener.local_peer().clone();

    // The two the window allows.
    let mut admitted = Vec::new();
    for (id, _) in dialers.iter().take(2) {
        let mut peer =
            SwarmRuntime::start(id, SubstrateConfig::default(), trusting(&[&listener_peer]))
                .expect("starts");
        assert!(
            peer.dial(listener_peer.clone(), bound.clone())
                .await
                .expect("the command reaches the task")
                .is_ok()
        );
        assert_eq!(wait_connected(&mut peer).await, listener_peer);
        admitted.push(peer);
    }

    // The third start from that bucket. The dial is admitted locally --
    // this node's own policy has nothing against it -- and refused by
    // the LISTENER, before a handshake is attempted.
    let mut refused = SwarmRuntime::start(
        &dialers[2].0,
        SubstrateConfig::default(),
        trusting(&[&listener_peer]),
    )
    .expect("starts");
    assert!(
        refused
            .dial(listener_peer, bound)
            .await
            .expect("the command reaches the task")
            .is_ok(),
        "the refusal belongs to the listener, not to the dialer's own gate"
    );
    let detail = wait_dial_failed(&mut refused).await;
    assert!(
        !detail.contains("Unexpected peer ID"),
        "refused for the rate, not for identity: {detail}"
    );

    // AND THE ONES ALREADY IN are untouched: a rate limit refuses new
    // work, it does not tear down what was already accepted. Asserted
    // by watching for a disconnection that must not arrive, because
    // "still connected" is not observable any other way and a loop that
    // checked something else would be a loop that asserts nothing.
    for peer in &mut admitted {
        match tokio::time::timeout(std::time::Duration::from_millis(300), peer.next_event()).await {
            Err(_) => {}
            Ok(Some(interweave_transport_libp2p::SwarmEvent::Disconnected { .. })) => {
                panic!("a rate limit must not close connections it already accepted");
            }
            Ok(_) => {}
        }
    }

    refused.shutdown().await.expect("shuts down");
    for peer in admitted {
        peer.shutdown().await.expect("shuts down");
    }
    listener.shutdown().await.expect("shuts down");
}

#[tokio::test]
async fn a_completed_handshake_gives_its_pre_auth_slot_back() {
    // A ceiling on handshakes IN FLIGHT is a ceiling on concurrency. A
    // slot that is not released when the handshake finishes turns it
    // into a lifetime quota instead: the listener would accept exactly
    // `max_pending_total` connections and then refuse everyone forever,
    // and it would do so silently, because refusing is what the gate is
    // supposed to do.
    let limits = PreAuthLimitsBuilder {
        max_pending_total: 1,
        max_pending_per_source: 1,
        ..PreAuthLimitsBuilder::default()
    }
    .build()
    .expect("one at a time is a narrowing of the specified policy");

    let dialers: Vec<(ProfileIdentity, TransportIdentity)> = (0..3).map(|_| who()).collect();
    let trusted: Vec<&TransportIdentity> = dialers.iter().map(|(_, p)| p).collect();

    let listener = SwarmRuntime::start(
        &identity(),
        SubstrateConfig {
            preauth: limits,
            ..SubstrateConfig::default()
        },
        trusting(&trusted),
    )
    .expect("starts");
    let bound = listener
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("binds");
    let listener_peer = listener.local_peer().clone();

    // One at a time, three times over. Each completes before the next
    // begins, so the ceiling of one is never the thing being tested --
    // the release is.
    let mut peers = Vec::new();
    for (attempt, (id, _)) in dialers.iter().enumerate() {
        let mut peer =
            SwarmRuntime::start(id, SubstrateConfig::default(), trusting(&[&listener_peer]))
                .expect("starts");
        assert!(
            peer.dial(listener_peer.clone(), bound.clone())
                .await
                .expect("the command reaches the task")
                .is_ok()
        );
        assert_eq!(
            wait_connected(&mut peer).await,
            listener_peer,
            "handshake {attempt} must be admitted; a leaked slot refuses every one after the first"
        );
        peers.push(peer);
    }

    for peer in peers {
        peer.shutdown().await.expect("shuts down");
    }
    listener.shutdown().await.expect("shuts down");
}

#[tokio::test]
async fn a_handshake_abandoned_mid_flight_does_not_hold_its_slot() {
    // The cheapest attack this gate exists to bound: open a connection,
    // say nothing, drop it. libp2p never establishes it, so the release
    // that runs on establishment never runs -- and if nothing else
    // releases it, one socket and no bytes costs the listener a slot
    // for the length of the handshake timeout, over and over.
    let limits = PreAuthLimitsBuilder {
        max_pending_total: 1,
        max_pending_per_source: 1,
        ..PreAuthLimitsBuilder::default()
    }
    .build()
    .expect("one at a time is a narrowing of the specified policy");

    let (dialer_id, dialer_peer) = who();
    let listener = SwarmRuntime::start(
        &identity(),
        SubstrateConfig {
            preauth: limits,
            ..SubstrateConfig::default()
        },
        trusting(&[&dialer_peer]),
    )
    .expect("starts");
    let bound = listener
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("binds");
    let listener_peer = listener.local_peer().clone();

    // A raw TCP connection that never speaks. Not a SwarmRuntime,
    // because a runtime would complete the handshake and the abandoned
    // case is the one that matters.
    let socket = socket_addr_of(&bound);
    let abandoned = tokio::net::TcpStream::connect(socket)
        .await
        .expect("the listener is accepting");
    drop(abandoned);

    // The slot must come back. Bounded retry rather than one attempt:
    // the release is driven by an event the listener has to process,
    // and the assertion is that it happens at all -- a slot held until
    // the handshake timeout would not be released within this window.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let mut peer = SwarmRuntime::start(
            &dialer_id,
            SubstrateConfig::default(),
            trusting(&[&listener_peer]),
        )
        .expect("starts");
        let dialed = peer
            .dial(listener_peer.clone(), bound.clone())
            .await
            .expect("the command reaches the task");
        assert!(dialed.is_ok(), "the dialer's own gate has no objection");
        match tokio::time::timeout(std::time::Duration::from_millis(500), peer.next_event()).await {
            Ok(Some(interweave_transport_libp2p::SwarmEvent::Connected { .. })) => {
                peer.shutdown().await.expect("shuts down");
                break;
            }
            _ => {
                peer.shutdown().await.expect("shuts down");
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the abandoned handshake is still holding the only slot"
                );
            }
        }
    }

    listener.shutdown().await.expect("shuts down");
}

/// The `SocketAddr` a bound loopback multiaddr names.
fn socket_addr_of(address: &Multiaddr) -> std::net::SocketAddr {
    use libp2p::multiaddr::Protocol;

    let mut ip = None;
    let mut port = None;
    for component in address {
        match component {
            Protocol::Ip4(v4) => ip = Some(std::net::IpAddr::V4(v4)),
            Protocol::Ip6(v6) => ip = Some(std::net::IpAddr::V6(v6)),
            Protocol::Tcp(p) => port = Some(p),
            _ => {}
        }
    }
    std::net::SocketAddr::new(ip.expect("an ip component"), port.expect("a tcp component"))
}

#[tokio::test]
async fn a_handshake_that_never_speaks_is_dropped_on_the_timeout() {
    // SECURITY.md's handshake timeout, and the reason it is not
    // optional: a party that completes the TCP handshake and then says
    // nothing costs one socket and no bytes. Without a deadline the
    // listener waits indefinitely, and every bound above this one --
    // the pending ceiling, the per-source share -- is spent by
    // connections that will never finish.
    let limits = PreAuthLimitsBuilder {
        handshake_timeout_ms: 1_000,
        ..PreAuthLimitsBuilder::default()
    }
    .build()
    .expect("a second is a narrowing of the specified ten");

    let listener = SwarmRuntime::start(
        &identity(),
        SubstrateConfig {
            preauth: limits,
            ..SubstrateConfig::default()
        },
        trusting_nobody(),
    )
    .expect("starts");
    let bound = listener
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("binds");

    let mut silent = tokio::net::TcpStream::connect(socket_addr_of(&bound))
        .await
        .expect("the listener is accepting");

    // EOF is the listener hanging up. The read is given several times
    // the timeout, so a failure here is "it never hung up" rather than
    // "the machine was slow".
    let mut buffer = [0_u8; 1];
    let read = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        tokio::io::AsyncReadExt::read(&mut silent, &mut buffer),
    )
    .await
    .expect("the listener must hang up on a handshake that says nothing");
    assert_eq!(
        read.expect("a closed socket reads cleanly"),
        0,
        "the connection must be closed, not merely idle"
    );

    listener.shutdown().await.expect("shuts down");
}

#[tokio::test]
async fn an_untrusted_peer_is_never_dialed() {
    // The default this substrate had inverted. Every dial passed a
    // hardcoded `ConnectionClass::DataPlaneTrusted`, so the trust
    // policy was consulted by nobody and an empty allowlist -- ADR-0012's
    // configuration that admits NOBODY -- admitted everybody.
    let listener = SwarmRuntime::start(&identity(), SubstrateConfig::default(), trusting_nobody())
        .expect("starts");
    let bound = listener
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("binds");
    let listener_peer = listener.local_peer().clone();

    let mut dialer =
        SwarmRuntime::start(&identity(), SubstrateConfig::default(), trusting_nobody())
            .expect("starts");

    match dialer
        .dial(listener_peer, bound)
        .await
        .expect("the command reaches the task")
    {
        Err(DialRefusal::Policy(DialDenial::Unauthorized)) => {}
        other => panic!("an unclassified peer must not be dialed, got {other:?}"),
    }

    // And nothing was attempted: a refusal that opened a socket first
    // would be an authorization oracle and a resource cost both.
    let quiet =
        tokio::time::timeout(std::time::Duration::from_millis(500), dialer.next_event()).await;
    assert!(quiet.is_err(), "no network activity may follow: {quiet:?}");

    dialer.shutdown().await.expect("shuts down");
    listener.shutdown().await.expect("shuts down");
}

#[tokio::test]
async fn an_untrusted_inbound_connection_is_not_retained() {
    // ADR-0011: the same current authorization that governs outbound
    // applies before an inbound data-plane connection is RETAINED.
    // Arriving is not an authorization, and a listener that kept
    // whoever completed a handshake would make the allowlist a rule
    // about dialing rather than about who this profile talks to.
    let (dialer_id, dialer_peer) = who();
    let listener = SwarmRuntime::start(&identity(), SubstrateConfig::default(), trusting_nobody())
        .expect("starts");
    let bound = listener
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("binds");
    let listener_peer = listener.local_peer().clone();
    let _ = dialer_peer;

    // The dialer trusts the listener, so the dial goes out; the
    // listener does not trust the dialer, so the connection must not
    // survive.
    let mut dialer = SwarmRuntime::start(
        &dialer_id,
        SubstrateConfig::default(),
        trusting(&[&listener_peer]),
    )
    .expect("starts");
    assert!(
        dialer
            .dial(listener_peer, bound)
            .await
            .expect("the command reaches the task")
            .is_ok()
    );

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match tokio::time::timeout_at(deadline, dialer.next_event())
            .await
            .expect("the close arrives within the deadline")
        {
            Some(interweave_transport_libp2p::SwarmEvent::Disconnected { .. }) => break,
            Some(_) => {}
            None => panic!("the substrate stopped before the connection closed"),
        }
    }

    dialer.shutdown().await.expect("shuts down");
    listener.shutdown().await.expect("shuts down");
}

#[tokio::test]
async fn revoking_trust_closes_the_connection_it_revoked() {
    // ADR-0012: removing a peer must evict its active data-plane
    // connectivity. A revocation that only changed what the next dial
    // was told would leave the revoked peer with a live session for as
    // long as it kept talking -- which is exactly the session an
    // operator revokes trust to end.
    let (dialer_id, dialer_peer) = who();
    let listener = SwarmRuntime::start(
        &identity(),
        SubstrateConfig::default(),
        trusting(&[&dialer_peer]),
    )
    .expect("starts");
    let bound = listener
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("binds");
    let listener_peer = listener.local_peer().clone();

    let mut dialer = SwarmRuntime::start(
        &dialer_id,
        SubstrateConfig::default(),
        trusting(&[&listener_peer]),
    )
    .expect("starts");
    assert!(
        dialer
            .dial(listener_peer.clone(), bound)
            .await
            .expect("the command reaches the task")
            .is_ok()
    );
    assert_eq!(
        wait_connected(&mut dialer).await,
        listener_peer,
        "connected first, so the eviction below has something to evict"
    );

    // Give the listener a moment to record the inbound connection, so
    // the revocation has something to find rather than racing it.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let closed = listener
        .set_trust(trusting_nobody())
        .await
        .expect("the command reaches the task");
    assert_eq!(closed, 1, "the revoked peer's connection must be named");

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match tokio::time::timeout_at(deadline, dialer.next_event())
            .await
            .expect("the close arrives within the deadline")
        {
            Some(interweave_transport_libp2p::SwarmEvent::Disconnected { .. }) => break,
            Some(_) => {}
            None => panic!("the substrate stopped before the connection closed"),
        }
    }

    dialer.shutdown().await.expect("shuts down");
    listener.shutdown().await.expect("shuts down");
}

#[tokio::test]
async fn a_peer_can_be_dialed_by_identity_alone() {
    // The address book, and the reason it has to exist before the
    // scheduler does: a reconnect knows which peer it wants and has to
    // find out for itself where that peer is.
    let (dialer_id, dialer_peer) = who();
    let listener = SwarmRuntime::start(
        &identity(),
        SubstrateConfig::default(),
        trusting(&[&dialer_peer]),
    )
    .expect("starts");
    let bound = listener
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("binds");
    let listener_peer = listener.local_peer().clone();

    let mut dialer = SwarmRuntime::start(
        &dialer_id,
        SubstrateConfig::default(),
        trusting(&[&listener_peer]),
    )
    .expect("starts");

    // Nothing is known yet, and the refusal says so rather than
    // pretending the policy objected.
    match dialer
        .dial_peer(listener_peer.clone())
        .await
        .expect("the command reaches the task")
    {
        Err(DialRefusal::NoKnownAddress) => {}
        other => panic!("an empty book must say so, got {other:?}"),
    }

    assert!(
        dialer
            .add_address(listener_peer.clone(), bound)
            .await
            .expect("the command reaches the task"),
        "a classified peer gets a book entry"
    );
    assert!(
        dialer
            .dial_peer(listener_peer.clone())
            .await
            .expect("the command reaches the task")
            .is_ok()
    );
    assert_eq!(wait_connected(&mut dialer).await, listener_peer);

    dialer.shutdown().await.expect("shuts down");
    listener.shutdown().await.expect("shuts down");
}

#[tokio::test]
async fn an_untrusted_peer_gets_no_address_book_entry() {
    // The book's key set is bounded by the trust allowlist, because
    // Identify lets any peer that connects describe itself. Asserted at
    // the substrate because that is where the Identify messages arrive.
    let dialer = SwarmRuntime::start(&identity(), SubstrateConfig::default(), trusting_nobody())
        .expect("starts");
    let stranger = TransportIdentity::parse(ABSENT).expect("canonical");
    assert!(
        !dialer
            .add_address(stranger, "/ip4/192.0.2.1/tcp/4001".parse().expect("valid"))
            .await
            .expect("the command reaches the task"),
        "an unclassified peer is not somewhere this profile stores addresses"
    );
    dialer.shutdown().await.expect("shuts down");
}

#[tokio::test(start_paused = true)]
async fn a_failed_peer_is_retried_without_anyone_asking() {
    // `due_retries` computed a moment and nothing waited for it, so a
    // peer that failed was scheduled and then never dialed again: the
    // backoff was a number in a table.
    //
    // Paused time, because the first retry is thirty seconds out by
    // CONNECTIVITY.md and a test that waited would be a test nobody
    // runs. The clock is tokio's, so advancing it advances the
    // substrate's own notion of now -- and the address below fails
    // SYNCHRONOUSLY, with no socket, so there is no real I/O for the
    // virtual clock to outrun.
    let absent = TransportIdentity::parse(ABSENT).expect("canonical");
    let mut runtime = SwarmRuntime::start(
        &identity(),
        SubstrateConfig::default(),
        trusting(&[&absent]),
    )
    .expect("starts");

    // UDP, on a Swarm whose only transport is TCP. libp2p refuses it
    // without opening anything, which is what keeps this test free of
    // wall-clock time.
    let unsupported: Multiaddr = "/ip4/127.0.0.1/udp/1".parse().expect("valid");
    assert!(
        runtime
            .add_address(absent.clone(), unsupported.clone())
            .await
            .expect("the command reaches the task")
    );
    assert!(
        runtime
            .dial(absent.clone(), unsupported)
            .await
            .expect("the command reaches the task")
            .is_ok(),
        "admitted -- libp2p discovers there is no transport for it afterwards"
    );

    // NOBODY ASKS AGAIN. This test issues exactly one dial, so a
    // SECOND failure for that peer can only have come from the
    // scheduler.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(600);
    let mut failures = 0_u8;
    while failures < 2 {
        match tokio::time::timeout_at(deadline, runtime.next_event())
            .await
            .expect("a scheduled retry must arrive without another command")
        {
            Some(interweave_transport_libp2p::SwarmEvent::DialFailed { peer, .. }) => {
                assert_eq!(peer.as_ref(), Some(&absent), "a failure for another peer");
                failures = failures.saturating_add(1);
            }
            Some(_) => {}
            None => panic!("the substrate stopped before retrying"),
        }
    }

    runtime.shutdown().await.expect("shuts down");
}

#[tokio::test(start_paused = true)]
async fn a_revoked_peer_is_not_retried() {
    // The scheduler is a dial origin like any other, so a revocation
    // has to stop it too. A retry that bypassed admission -- because it
    // was "already scheduled" -- would keep dialing a peer the operator
    // has withdrawn trust from, on a timer, indefinitely.
    //
    // The refusal is also REPORTED: nobody is holding a reply channel
    // for a scheduled dial, so without an event an operator watching a
    // peer that never reconnects would have nothing to look at.
    let absent = TransportIdentity::parse(ABSENT).expect("canonical");
    let mut runtime = SwarmRuntime::start(
        &identity(),
        SubstrateConfig::default(),
        trusting(&[&absent]),
    )
    .expect("starts");

    let unsupported: Multiaddr = "/ip4/127.0.0.1/udp/1".parse().expect("valid");
    assert!(
        runtime
            .add_address(absent.clone(), unsupported.clone())
            .await
            .expect("the command reaches the task")
    );
    assert!(
        runtime
            .dial(absent.clone(), unsupported)
            .await
            .expect("the command reaches the task")
            .is_ok()
    );
    let _ = wait_dial_failed(&mut runtime).await;

    // Trust withdrawn while the retry is pending.
    let _ = runtime
        .set_trust(trusting_nobody())
        .await
        .expect("the command reaches the task");

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(600);
    loop {
        match tokio::time::timeout_at(deadline, runtime.next_event())
            .await
            .expect("the scheduler must report why it gave up")
        {
            Some(interweave_transport_libp2p::SwarmEvent::DialFailed { peer, detail }) => {
                assert!(
                    detail.contains("scheduled retry") && detail.contains("Unauthorized"),
                    "a revoked peer must be refused by the gate, not dialed: {detail}"
                );
                assert_eq!(peer.as_ref(), Some(&absent));
                break;
            }
            Some(_) => {}
            None => panic!("the substrate stopped before the retry came due"),
        }
    }

    runtime.shutdown().await.expect("shuts down");
}

#[tokio::test]
async fn a_handshake_killed_by_the_timeout_gives_its_slot_back() {
    // The reclaim path for the case a deadline sweep would have
    // covered, and the reason no sweep is needed: the transport timeout
    // ends the connection, the listener reports the failure, and the
    // slot is released on that report.
    //
    // Without it, one silent connection per timeout would cost the
    // listener a slot permanently -- and with a ceiling of one, the
    // second peer here would never get in.
    let limits = PreAuthLimitsBuilder {
        max_pending_total: 1,
        max_pending_per_source: 1,
        handshake_timeout_ms: 1_000,
        ..PreAuthLimitsBuilder::default()
    }
    .build()
    .expect("a second is a narrowing of the specified ten");

    let (dialer_id, dialer_peer) = who();
    let listener = SwarmRuntime::start(
        &identity(),
        SubstrateConfig {
            preauth: limits,
            ..SubstrateConfig::default()
        },
        trusting(&[&dialer_peer]),
    )
    .expect("starts");
    let bound = listener
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("binds");
    let listener_peer = listener.local_peer().clone();

    // A connection that completes TCP and then says nothing. It holds
    // the only slot until the timeout takes it.
    let silent = tokio::net::TcpStream::connect(socket_addr_of(&bound))
        .await
        .expect("the listener is accepting");

    // A real peer, arriving after the timeout has had its chance.
    tokio::time::sleep(std::time::Duration::from_millis(2_000)).await;
    let mut dialer = SwarmRuntime::start(
        &dialer_id,
        SubstrateConfig::default(),
        trusting(&[&listener_peer]),
    )
    .expect("starts");
    assert!(
        dialer
            .dial(listener_peer.clone(), bound)
            .await
            .expect("the command reaches the task")
            .is_ok()
    );
    assert_eq!(
        wait_connected(&mut dialer).await,
        listener_peer,
        "the timed-out handshake must not still be holding the only slot"
    );

    drop(silent);
    dialer.shutdown().await.expect("shuts down");
    listener.shutdown().await.expect("shuts down");
}
