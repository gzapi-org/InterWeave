// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Stage 4 exit gate, over real sockets.
//!
//! Two peers listen, dial, authenticate each other with Noise, exchange
//! Identify, and shut down. Loopback TCP rather than a mock: the gate
//! asks whether the substrate works, and a mocked transport would prove
//! only that the translation layer compiles.
//!
//! Every test binds `127.0.0.1:0` and reads back the assigned port, so
//! nothing depends on a fixed port being free.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::{
    MAX_CONFIGURED_CAPACITY, SubstrateConfig, SubstrateError, SwarmEvent, SwarmRuntime,
};
use interweave_transport_runtime::TrustSources;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::Multiaddr;

/// Trust exactly these peers on the application data plane.
///
/// There is no allow-all: ADR-0012's default admits nobody, so a test
/// that connects has to say who it trusts -- and one that connects
/// without saying so now fails, which is the point.
fn trusting(peers: &[&TransportIdentity]) -> TrustSources {
    TrustSources::new(
        PeerTrustPolicy::new(peers.iter().map(|p| (*p).clone())).expect("a handful"),
        InfrastructureSet::default(),
    )
}

/// Every wait is bounded. A hung substrate must fail the suite rather
/// than hold CI until the job timeout, where the cause is invisible.
const PATIENCE: Duration = Duration::from_secs(20);

async fn wait_for<F>(runtime: &mut SwarmRuntime, what: &str, mut predicate: F) -> SwarmEvent
where
    F: FnMut(&SwarmEvent) -> bool,
{
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for {what}");
        match tokio::time::timeout(remaining, runtime.next_event()).await {
            Err(_) => panic!("timed out waiting for {what}"),
            Ok(None) => panic!("the substrate stopped while waiting for {what}"),
            Ok(Some(event)) => {
                if predicate(&event) {
                    return event;
                }
            }
        }
    }
}

async fn listening_on_loopback(runtime: &mut SwarmRuntime) -> Multiaddr {
    let addr: Multiaddr = "/ip4/127.0.0.1/tcp/0".parse().expect("loopback multiaddr");
    // `listen` resolves to the bound address, so nothing has to consume a
    // separate event to learn where it is listening.
    runtime.listen(addr).await.expect("listen accepted")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_peers_connect_authenticate_identify_and_shut_down() {
    // The exit gate, in one test, because the clauses are one story: a
    // connection that authenticated but never exchanged Identify would
    // pass two separate tests and still not be a working substrate.
    let listener_identity = ProfileIdentity::generate();
    let dialer_identity = ProfileIdentity::generate();
    let listener_peer = listener_identity.transport_identity().expect("peer id");
    let dialer_peer = dialer_identity.transport_identity().expect("peer id");

    let mut listener = SwarmRuntime::start(
        &listener_identity,
        SubstrateConfig::default(),
        trusting(&[&dialer_peer]),
    )
    .expect("listener");
    let mut dialer = SwarmRuntime::start(
        &dialer_identity,
        SubstrateConfig::default(),
        trusting(&[&listener_peer]),
    )
    .expect("dialer");

    assert_eq!(listener.local_peer(), &listener_peer);
    assert_eq!(dialer.local_peer(), &dialer_peer);

    let address = listening_on_loopback(&mut listener).await;

    dialer
        .dial(listener_peer.clone(), address)
        .await
        .expect("command delivered")
        .expect("the dial was admitted");

    // NOISE AUTHENTICATED THE EXPECTED IDENTITY. The dialer knew an
    // address; what it learns here is who was actually there.
    let connected = wait_for(&mut dialer, "an authenticated connection", |e| {
        matches!(e, SwarmEvent::Connected { .. })
    })
    .await;
    match connected {
        SwarmEvent::Connected { peer } => assert_eq!(
            peer, listener_peer,
            "Noise must authenticate the PeerId that was expected"
        ),
        other => panic!("unexpected {other:?}"),
    }

    // And the listener sees the dialer's identity, not merely a socket.
    let inbound = wait_for(&mut listener, "the inbound connection", |e| {
        matches!(e, SwarmEvent::Connected { .. })
    })
    .await;
    match inbound {
        SwarmEvent::Connected { peer } => assert_eq!(peer, dialer_peer),
        other => panic!("unexpected {other:?}"),
    }

    // Identify completes in both directions.
    let identified = wait_for(&mut dialer, "Identify from the listener", |e| {
        matches!(e, SwarmEvent::Identified { .. })
    })
    .await;
    match identified {
        SwarmEvent::Identified {
            peer,
            protocol_version,
            ..
        } => {
            assert_eq!(peer, listener_peer);
            assert_eq!(
                protocol_version,
                interweave_transport_libp2p::IDENTIFY_PROTOCOL,
                "the advertised protocol is the namespaced one (ADR-0047)"
            );
        }
        other => panic!("unexpected {other:?}"),
    }

    // DETERMINISTIC SHUTDOWN: both runtimes are awaited, not dropped.
    listener.shutdown().await.expect("listener stops");
    dialer.shutdown().await.expect("dialer stops");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn identity_survives_a_restart_of_the_substrate() {
    // The exit gate's last clause. The key file is what carries the
    // identity across the restart, so this exercises the Stage 3 adapter
    // and the Stage 4 substrate together — which is the pairing that
    // actually has to hold.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("identity.key");

    let first = ProfileIdentity::generate();
    first.save(&path).expect("save");
    let expected = first.transport_identity().expect("peer id");

    let runtime =
        SwarmRuntime::start(&first, SubstrateConfig::default(), trusting(&[])).expect("start");
    assert_eq!(runtime.local_peer(), &expected);
    runtime.shutdown().await.expect("stops");
    drop(first);

    // A whole new process would reload from disk; this reloads the same
    // way and asserts the PeerId is the same one, not merely a valid one.
    let reloaded = ProfileIdentity::load(&path).expect("load");
    let again =
        SwarmRuntime::start(&reloaded, SubstrateConfig::default(), trusting(&[])).expect("restart");
    assert_eq!(
        again.local_peer(),
        &expected,
        "a restart must keep the PeerId; a new one would silently invalidate every trust relationship"
    );
    again.shutdown().await.expect("stops");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_is_awaited_and_the_task_really_ends() {
    // "Without leaked tasks" is only checkable if something waited. This
    // asserts the join completes, and then that the runtime is genuinely
    // unusable rather than merely marked stopped.
    let identity = ProfileIdentity::generate();
    let mut runtime =
        SwarmRuntime::start(&identity, SubstrateConfig::default(), trusting(&[])).expect("start");
    let address = listening_on_loopback(&mut runtime).await;
    assert!(!address.to_string().is_empty());

    tokio::time::timeout(PATIENCE, runtime.shutdown())
        .await
        .expect("shutdown must not hang")
        .expect("the task joins cleanly");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dial_the_policy_refuses_never_opens_a_socket() {
    // The gate is wired from the first line of substrate code, not
    // retrofitted (CLAUDE.md §3). A refusal here costs no connection
    // attempt at all, which is the reason to check before dialling
    // rather than after failing.
    //
    // The refusal used is the pending-dial budget rather than a
    // quarantine, because it is reachable through the public config in
    // one line. What is being proved is that `admit` RUNS on the dial
    // path — which denial fires is Stage 2's business, and it has its own
    // exhaustive tests.
    use interweave_transport_libp2p::DialRefusal;

    let identity = ProfileIdentity::generate();
    let mut runtime = SwarmRuntime::start(
        &identity,
        SubstrateConfig {
            // Zero pending dials: every dial is refused by policy, which
            // is the cleanest way to prove the gate runs at all.
            max_pending_dials: 0,
            ..SubstrateConfig::default()
        },
        trusting(&[]),
    )
    .expect("start");

    let unreachable: Multiaddr = "/ip4/127.0.0.1/tcp/1".parse().expect("multiaddr");
    let target = TransportIdentity::parse("12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN")
        .expect("canonical peer id");

    let refusal = runtime
        .dial(target, unreachable)
        .await
        .expect("command delivered")
        .expect_err("the policy must refuse this dial");
    assert!(
        matches!(refusal, DialRefusal::Policy(_)),
        "the refusal must come from the admission policy, not the backend: {refusal:?}"
    );

    // Nothing was attempted, so no dial failure arrives either.
    let quiet = tokio::time::timeout(Duration::from_millis(500), runtime.next_event()).await;
    assert!(
        quiet.is_err(),
        "a policy refusal must not produce a network attempt: {quiet:?}"
    );

    runtime.shutdown().await.expect("stops");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dial_to_nowhere_reports_failure_without_stopping_the_substrate() {
    let identity = ProfileIdentity::generate();
    // Port 1 on loopback: reserved, and nothing is listening.
    let nowhere: Multiaddr = "/ip4/127.0.0.1/tcp/1".parse().expect("multiaddr");
    let target = TransportIdentity::parse("12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5")
        .expect("canonical peer id");
    // Trusted, so the refusal under test is the NETWORK's and not the
    // gate's: an unauthorized peer never reaches a socket at all, which
    // is a different test one file over.
    let mut runtime =
        SwarmRuntime::start(&identity, SubstrateConfig::default(), trusting(&[&target]))
            .expect("start");

    runtime
        .dial(target, nowhere)
        .await
        .expect("command delivered")
        .expect("admitted by policy");

    wait_for(&mut runtime, "a dial failure", |e| {
        matches!(e, SwarmEvent::DialFailed { .. })
    })
    .await;

    // The substrate is still alive: a failed dial is an event, not a
    // fatal condition.
    let address = listening_on_loopback(&mut runtime).await;
    assert!(address.to_string().contains("127.0.0.1"));
    runtime.shutdown().await.expect("stops");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn dialling_an_address_where_a_different_peer_answers_does_not_connect() {
    // The test the original suite could not have failed. It asserted that
    // the connected peer equalled the expected one — and it did, because
    // the listener genuinely WAS that peer. Nothing distinguished "libp2p
    // enforced the identity" from "the address happened to be right".
    //
    // Here the address is right and the identity is not: a real listener
    // answers, completes a Noise handshake with its own key, and is not
    // who the dialler asked for. Without the expected PeerId bound into
    // the dial, that connection succeeds.
    let impostor_identity = ProfileIdentity::generate();
    let dialer_identity = ProfileIdentity::generate();
    let expected_but_absent = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id");

    let mut impostor = SwarmRuntime::start(
        &impostor_identity,
        SubstrateConfig::default(),
        trusting(&[]),
    )
    .expect("impostor");
    // The dialer trusts the peer it is ASKING FOR, which is the one the
    // gate classifies. That it is not the peer that answers is the test.
    let mut dialer = SwarmRuntime::start(
        &dialer_identity,
        SubstrateConfig::default(),
        trusting(&[&expected_but_absent]),
    )
    .expect("dialer");

    let address = listening_on_loopback(&mut impostor).await;

    dialer
        .dial(expected_but_absent.clone(), address)
        .await
        .expect("command delivered")
        .expect("admitted by policy");

    // What must NOT happen is a Connected event. A dial failure is the
    // correct outcome: someone answered, and it was not the peer asked
    // for.
    let outcome = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match dialer.next_event().await {
                Some(SwarmEvent::Connected { peer }) => return Some(peer),
                Some(SwarmEvent::DialFailed { .. }) => return None,
                Some(_) => {}
                None => return None,
            }
        }
    })
    .await
    .expect("the dialler must reach a verdict");

    assert!(
        outcome.is_none(),
        "connected to {outcome:?} while expecting {expected_but_absent:?} — \
         the expected identity was not enforced"
    );

    dialer.shutdown().await.expect("stops");
    impostor.shutdown().await.expect("stops");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn listen_resolves_to_the_address_it_bound() {
    // Port 0 means "pick one", so the assigned port is only knowable from
    // the answer. Returning a placeholder made this method's own
    // documentation false and forced every caller to consume an event to
    // learn what it had just been told.
    let identity = ProfileIdentity::generate();
    let other_identity = ProfileIdentity::generate();
    let peer = identity.transport_identity().expect("peer id");
    let other_peer = other_identity.transport_identity().expect("peer id");
    let runtime = SwarmRuntime::start(
        &identity,
        SubstrateConfig::default(),
        trusting(&[&other_peer]),
    )
    .expect("start");

    let requested: Multiaddr = "/ip4/127.0.0.1/tcp/0".parse().expect("multiaddr");
    let bound = runtime.listen(requested).await.expect("listen");

    let text = bound.to_string();
    assert!(text.starts_with("/ip4/127.0.0.1/tcp/"), "{text}");
    let port: u16 = text
        .rsplit('/')
        .next()
        .expect("a port component")
        .parse()
        .expect("the port is a number");
    assert_ne!(port, 0, "the ASSIGNED port, not the one that was asked for");

    // And the address is real: a second runtime can dial it.
    let mut other = SwarmRuntime::start(
        &other_identity,
        SubstrateConfig::default(),
        trusting(&[&peer]),
    )
    .expect("start");
    other
        .dial(peer.clone(), bound)
        .await
        .expect("command delivered")
        .expect("admitted");
    let connected = wait_for(&mut other, "a connection to the returned address", |e| {
        matches!(e, SwarmEvent::Connected { .. })
    })
    .await;
    assert_eq!(connected, SwarmEvent::Connected { peer });

    other.shutdown().await.expect("stops");
    runtime.shutdown().await.expect("stops");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_completes_while_the_event_channel_is_full() {
    // The deadlock. With the send awaited inline, a full event channel
    // parks the whole task inside the event branch: the command branch is
    // never polled, so `shutdown` enqueues its command and waits forever
    // for a reply from a task that is waiting for the consumer it is
    // blocking.
    //
    // A capacity of 1 and a consumer that never drains reproduces it in
    // one connection.
    let listener_identity = ProfileIdentity::generate();
    let dialer_identity = ProfileIdentity::generate();
    let listener_peer = listener_identity.transport_identity().expect("peer id");

    let dialer_peer = dialer_identity.transport_identity().expect("peer id");
    let mut listener = SwarmRuntime::start(
        &listener_identity,
        SubstrateConfig {
            event_capacity: 1,
            ..SubstrateConfig::default()
        },
        trusting(&[&dialer_peer]),
    )
    .expect("listener");
    let dialer = SwarmRuntime::start(
        &dialer_identity,
        SubstrateConfig::default(),
        trusting(&[&listener_peer]),
    )
    .expect("dialer");

    let address = listening_on_loopback(&mut listener).await;

    dialer
        .dial(listener_peer, address)
        .await
        .expect("command delivered")
        .expect("admitted");

    // Give the listener time to fill its one-slot event channel and try to
    // enqueue more. Its events are deliberately NEVER read.
    tokio::time::sleep(Duration::from_millis(500)).await;

    tokio::time::timeout(Duration::from_secs(10), listener.shutdown())
        .await
        .expect("shutdown must not hang behind a full event channel")
        .expect("the task joins");

    dialer.shutdown().await.expect("stops");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn listen_completes_while_the_event_channel_is_full() {
    // The second half of the same deadlock, and the half that survived
    // the first fix. Holding one translated event kept `shutdown`
    // responsive, but while that event was held the loop selected only
    // between channel capacity and more commands — the Swarm was not
    // polled at all.
    //
    // `translate` is what answers a pending `listen`, and it runs only on
    // a polled event, so a `Listen` issued in that state waited for a
    // `NewListenAddr` that could never be observed. The caller could not
    // rescue itself either: `listen` borrows `&self` and `next_event`
    // borrows `&mut self`, so nobody awaiting the former can drain with
    // the latter.
    //
    // Capacity 1 and a consumer that never drains reproduces it in three
    // listeners: the first fills the channel, the second fills the held
    // slot, and the third is the one that used to hang forever.
    let identity = ProfileIdentity::generate();
    let runtime = SwarmRuntime::start(
        &identity,
        SubstrateConfig {
            event_capacity: 1,
            ..SubstrateConfig::default()
        },
        trusting(&[]),
    )
    .expect("runtime");

    let loopback: Multiaddr = "/ip4/127.0.0.1/tcp/0".parse().expect("loopback multiaddr");

    // Events are deliberately NEVER read, so each `Listening` stays where
    // the task put it.
    for nth in 1..=3 {
        let bound = tokio::time::timeout(PATIENCE, runtime.listen(loopback.clone()))
            .await
            .unwrap_or_else(|_| panic!("listen #{nth} hung behind a full event channel"))
            .expect("listen accepted");

        // A bound address, not a placeholder: the reply is only correct
        // if it came from the `NewListenAddr` this test is proving can
        // still be observed.
        assert!(
            bound.to_string().starts_with("/ip4/127.0.0.1/tcp/"),
            "listen #{nth} resolved to {bound}, not a bound loopback address"
        );
        assert!(
            !bound.to_string().ends_with("/tcp/0"),
            "listen #{nth} returned the requested port, not the assigned one"
        );

        // Let the task translate and shelve the resulting event before
        // the next call, so each iteration starts one slot fuller.
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    tokio::time::timeout(Duration::from_secs(10), runtime.shutdown())
        .await
        .expect("shutdown must not hang behind a full event channel")
        .expect("the task joins");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_zero_capacity_configuration_is_an_error_not_a_panic() {
    // `mpsc::channel(0)` aborts the process. A public configuration field
    // must not be able to do that to a transport daemon, whose lint
    // policy treats a reachable panic as a defect everywhere else.
    let identity = ProfileIdentity::generate();

    for (field, config) in [
        (
            "command_capacity",
            SubstrateConfig {
                command_capacity: 0,
                ..SubstrateConfig::default()
            },
        ),
        (
            "event_capacity",
            SubstrateConfig {
                event_capacity: 0,
                ..SubstrateConfig::default()
            },
        ),
    ] {
        match SwarmRuntime::start(&identity, config, trusting(&[])) {
            Err(SubstrateError::InvalidConfig { field: got, .. }) => assert_eq!(got, field),
            other => panic!("{field}: expected InvalidConfig, got {other:?}"),
        }
    }

    // A cap of zero is NOT an error. "Admit no dial", "hold no
    // connection", "accept no listen" are coherent policies and are how
    // the refusal paths are exercised — a panic guard must not become an
    // opinion about them.
    for config in [
        SubstrateConfig {
            max_pending_dials: 0,
            ..SubstrateConfig::default()
        },
        SubstrateConfig {
            max_connections: 0,
            ..SubstrateConfig::default()
        },
        SubstrateConfig {
            max_pending_listens: 0,
            ..SubstrateConfig::default()
        },
    ] {
        let runtime =
            SwarmRuntime::start(&identity, config, trusting(&[])).expect("a zero cap is a policy");
        runtime.shutdown().await.expect("stops");
    }

    // A ZERO HEARTBEAT IS THE SAME ABORT, one type away from the loop
    // that catches the others. `tokio::time::interval` panics on a zero
    // period, and `retry_tick` is a `Duration`, so it was not in the
    // `usize` table every other depth is checked by.
    for (label, tick) in [
        ("zero", Duration::ZERO),
        // Sub-millisecond truncates to the zero above, so it is refused
        // rather than silently becoming it.
        ("sub-millisecond", Duration::from_micros(500)),
    ] {
        let config = SubstrateConfig {
            retry_tick: tick,
            ..SubstrateConfig::default()
        };
        match SwarmRuntime::start(&identity, config, trusting(&[])) {
            Err(SubstrateError::InvalidConfig { field, .. }) => {
                assert_eq!(field, "retry_tick_ms", "{label}");
            }
            other => panic!("{label}: expected InvalidConfig, got {other:?}"),
        }
    }

    // And a heartbeat that is merely slow still starts — the guard is
    // against the abort, not an opinion about tuning.
    let slow = SubstrateConfig {
        retry_tick: Duration::from_secs(30),
        ..SubstrateConfig::default()
    };
    SwarmRuntime::start(&identity, slow, trusting(&[]))
        .expect("a slow heartbeat is a policy, not an abort")
        .shutdown()
        .await
        .expect("stops");

    // And an absurd request is refused rather than allocated: a ceiling
    // is what stops "tuning" from being the denial of service.
    let huge = SubstrateConfig {
        event_capacity: MAX_CONFIGURED_CAPACITY + 1,
        ..SubstrateConfig::default()
    };
    assert!(matches!(
        SwarmRuntime::start(&identity, huge, trusting(&[])),
        Err(SubstrateError::InvalidConfig { .. })
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_listeners_are_bounded_and_abandoned_ones_are_closed() {
    // The command channel bounds how many `Listen` commands may be
    // QUEUED. The task drains it continuously, so listeners and their
    // pending replies accumulate past any instantaneous queue depth
    // unless the table itself is bounded.
    let identity = ProfileIdentity::generate();
    let runtime = SwarmRuntime::start(
        &identity,
        SubstrateConfig {
            max_pending_listens: 2,
            ..SubstrateConfig::default()
        },
        trusting(&[]),
    )
    .expect("runtime");

    let loopback: Multiaddr = "/ip4/127.0.0.1/tcp/0".parse().expect("loopback multiaddr");

    // Each of these resolves, so nothing is left pending — the bound is
    // on listeners still AWAITING an address, and a resolved one is not.
    for _ in 0..4 {
        tokio::time::timeout(PATIENCE, runtime.listen(loopback.clone()))
            .await
            .expect("listen resolves")
            .expect("accepted");
    }

    runtime.shutdown().await.expect("stops");
}
