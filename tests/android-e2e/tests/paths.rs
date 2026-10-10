// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Android side and a desktop daemon over a relayed and a direct path
//! (plan §20 gate (c)'s transport cases), the Android side behind the
//! device seam: D, a desktop daemon, reserves on the test relay; R reaches
//! D only through the circuit; and C is given D's own address -- the
//! direct path, and the control in the same run, so the relayed reading
//! is the relay's doing and not the harness's. A direct message crosses
//! each path both ways over the client-api, each side is owed the
//! route-begin notice reading the path it took, and after R's process
//! death and restart D is owed R's return relayed with nothing before it
//! (`reconnected`). R's and C's halves are `interweave-android-e2e-cases`'
//! `paths` case, which runs in the Android side's own process; D's half
//! is here.
//!
//! Two runs. On the host stand-in both R and C are stand-ins. On a device
//! (`#[ignore]`d, run with `--ignored` and the device's environment,
//! `src/adb.rs`) the phone is R in one run and C in the other, and a
//! stand-in is the other side -- one phone cannot be both, and the
//! control stays in each run.
//!
//! What makes the circuit R's only route is the production boundary, as
//! in `tests/desktop-e2e/tests/relayed_path.rs`. D listens on loopback, and
//! ADR-0052's floor refuses a peer-advertised loopback address, so R never
//! learns a direct route to D. The stand-ins listen on the wildcard an
//! `embedded-android` profile requires, so they advertise this host's
//! interface addresses, and D refuses those too: a private-range candidate
//! is admitted only on a host with a private listener of its family, and
//! D has none. DCUtR's boundary applies the same rule before any socket.
//! The operator's door admits each profile's own static relay and entry.
//!
//! What this does NOT prove, by name: on the stand-in, the device -- the
//! Android target build, its process lifecycle, its network callbacks,
//! SELinux-confined app data (the stand-in's limits, `src/lib.rs`); on a
//! device, a radio network -- the phone reaches this host's loopback
//! through adb's reversed ports (`src/adb.rs`); a daemon acting as the
//! relay (the relay is a bare Swarm carrying the production relay-server
//! field, `interweave_test_support::e2e::relay`); anything off loopback --
//! the relay and D listen on 127.0.0.1; any NAT; more than one
//! host; a host whose interfaces carry a public address, where D could
//! learn a direct address for R -- the case assumes this host's interface
//! addresses are private. The `HumanChatV2` cases ride the same seam and
//! topology in `human_chat.rs`.

#![cfg(target_os = "linux")]
#![allow(clippy::expect_used, clippy::panic)]

use interweave_android_e2e_cases::{PathsArgs, Told, cases, keys};
use interweave_android_e2e_tests::adb::AdbDevice;
use interweave_android_e2e_tests::{
    Desktop, Device, HostStandIn, PATIENCE, Relay, desktop_answers, free_port, lease_request,
    relayed_example_of,
};
use interweave_local_client_api::{DataSessionBinding as _, RECONNECTED, ROUTE_ESTABLISHED};
use interweave_transport_api::{PeerPath, TransportIdentity};

/// Where the Android side listens: an `embedded-android` profile listens
/// on a wildcard only, since a listener on one address dies with it.
const WILDCARD: &str = "/ip4/0.0.0.0/tcp/0";

fn loopback_port() -> u16 {
    free_port(std::net::Ipv4Addr::LOCALHOST)
}

/// The TCP port of `address` (`/ip4/127.0.0.1/tcp/<port>/...`).
fn tcp_port(address: &str) -> u16 {
    address
        .split('/')
        .skip_while(|p| *p != "tcp")
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("no TCP port in {address}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_android_side_crosses_a_relayed_and_a_direct_path_to_a_desktop_both_ways() {
    relayed_and_direct(HostStandIn::new(), HostStandIn::new()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a device with the app's androidTest build: ANDROID_SERIAL and INTERWEAVE_ANDROID_INSTRUMENTATION (src/adb.rs); one phone, so --test-threads 1"]
async fn on_a_device_the_phone_crosses_the_relayed_path_with_a_stand_in_as_the_control() {
    relayed_and_direct(AdbDevice::connect(), HostStandIn::new()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a device with the app's androidTest build: ANDROID_SERIAL and INTERWEAVE_ANDROID_INSTRUMENTATION (src/adb.rs); one phone, so --test-threads 1"]
async fn on_a_device_the_phone_crosses_the_direct_path_beside_a_relayed_stand_in() {
    relayed_and_direct(HostStandIn::new(), AdbDevice::connect()).await;
}

/// The Android side's half of exchange `serial` toward `d_peer`, which
/// must be told its route began `path`.
fn args(d_peer: &TransportIdentity, path: PeerPath, serial: u8) -> serde_json::Value {
    PathsArgs {
        desktop: d_peer.clone(),
        path,
        serial,
        deadline: PATIENCE,
    }
    .to_json()
}

/// The case over any two [`Device`]s: R and C unstarted, their `PeerId`s
/// known. The exchanges run one at a time, so each read of D's session
/// sees one Android side's traffic only.
async fn relayed_and_direct<R: Device, C: Device>(mut r: R, mut c: C) {
    let mut d = Desktop::new();
    let (d_peer, r_peer, c_peer) = (d.peer.clone(), r.peer(), c.peer());
    let relay = Relay::start(&[&d_peer, &r_peer, &c_peer]).await;
    let d_port = loopback_port();
    let d_listen = format!("/ip4/127.0.0.1/tcp/{d_port}");
    let direct_to_d = format!("{d_listen}/p2p/{}", d_peer.as_str());
    d.start(&relayed_example_of(
        "human-desktop.yaml",
        &[&r_peer, &c_peer],
        &d_listen,
        &relay,
        None,
    ))
    .await;
    // What each Android side dials is this host's loopback: the relay
    // for both, D's own address for C.
    let relay_port = tcp_port(&relay.address);
    r.reach(relay_port);
    c.reach(relay_port);
    c.reach(d_port);

    // D's reservation first: until it holds, the relay has nowhere to
    // carry R's circuit.
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while !relay.seen().await.reservations.contains(&pid(&d_peer)) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "D never reserved on the relay:\n{}",
            d.log()
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    r.start(&relayed_example_of(
        "human-android.yaml",
        &[&d_peer],
        WILDCARD,
        &relay,
        Some(&relay.circuit_to(&d_peer)),
    ));
    c.start(&relayed_example_of(
        "human-android.yaml",
        &[&d_peer],
        WILDCARD,
        &relay,
        Some(&direct_to_d),
    ));

    let d_session = d.binding().open(lease_request()).await.expect("D leases");
    let mut d_told = Told::default();

    // R -> D over the circuit and back; C -> D over D's address and back.
    // Each Android side asserts, in its own process, the path its route
    // to D began on.
    let log = |side: &dyn Device| format!("{}\n{}", d.log(), side.log());
    let run = r.run_case(cases::PATHS, &args(&d_peer, PeerPath::Relayed, 1));
    desktop_answers(&d_session, &mut d_told, &r_peer, 1, || log(&r)).await;
    // The baseline for R's death is taken HERE, while R is known alive:
    // D's answer was just accepted on R's route, and R's case has not
    // returned. On a device R's process ends with its case
    // (`src/adb.rs`), so a baseline taken any later could already hold
    // that death, and the check below would look for a second one.
    let (told_before, gone_before) = (d_told.paths.len(), d_told.gone.len());
    let circuits_before = relay.seen().await.circuits.len();
    let out = run.passed(|| log(&r));
    assert_eq!(
        out[keys::PATH],
        PeerPath::Relayed.label(),
        "R of D: relayed"
    );
    let run = c.run_case(cases::PATHS, &args(&d_peer, PeerPath::Direct, 2));
    desktop_answers(&d_session, &mut d_told, &c_peer, 2, || log(&c)).await;
    let out = run.passed(|| log(&c));
    assert_eq!(
        out[keys::PATH],
        PeerPath::Direct.label(),
        "C of D, the control: direct"
    );

    // D was told each route began on the path the relay's record says.
    for (peer, want, what) in [
        (&r_peer, (None, PeerPath::Relayed), "D of R: relayed"),
        (
            &c_peer,
            (None, PeerPath::Direct),
            "D of C, the control: direct",
        ),
    ] {
        let (_, notice) = d_told
            .path_notice(&d_session, peer, ROUTE_ESTABLISHED, 0, PATIENCE)
            .await
            .unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(notice, want, "{what}");
    }
    let seen = relay.seen().await;
    assert!(
        seen.circuits.contains(&(pid(&r_peer), pid(&d_peer))),
        "the relay carried R's circuit to D: {seen:?}"
    );
    let touches_c =
        |(s, t): &(libp2p::PeerId, libp2p::PeerId)| *s == pid(&c_peer) || *t == pid(&c_peer);
    assert!(
        !seen.circuits.iter().any(touches_c) && !seen.denied.iter().any(touches_c),
        "no circuit to or from the control, asked or denied: {seen:?}"
    );

    // R's process dies and returns: D is told the disconnect, then R's
    // return over a new circuit, relayed with nothing before it. On the
    // stand-in the death is `kill`; on a device it is the end of R's
    // first case's process, and `kill` makes sure of it.
    // R's circuits to D at the death: the return must add one, and an
    // extra circuit opened before it cannot stand in for it.
    let r_to_d = |seen: &interweave_test_support::e2e::relay::RelaySeen| {
        seen.circuits
            .iter()
            .filter(|c| **c == (pid(&r_peer), pid(&d_peer)))
            .count()
    };
    let r_circuits_at_death = r_to_d(&relay.seen().await);
    r.kill();
    r.restart();
    let run = r.run_case(cases::PATHS, &args(&d_peer, PeerPath::Relayed, 3));
    desktop_answers(&d_session, &mut d_told, &r_peer, 3, || log(&r)).await;
    assert_eq!(
        run.passed(|| log(&r))[keys::PATH],
        PeerPath::Relayed.label(),
        "R of D after R's restart: relayed again"
    );
    let (back_at, back) = d_told
        .path_notice(&d_session, &r_peer, RECONNECTED, told_before, PATIENCE)
        .await
        .unwrap_or_else(|e| panic!("D of R after R's restart: {e}"));
    assert_eq!(
        back,
        (None, PeerPath::Relayed),
        "D of R after R's restart: relayed again, nothing before it"
    );
    assert!(
        d_told
            .gone
            .iter()
            .skip(gone_before)
            .any(|(p, at)| p == &r_peer && *at <= back_at),
        "the disconnect was told after the death and before the return: {:?}",
        d_told.gone
    );
    assert!(
        relay.seen().await.circuits.len() > circuits_before
            && r_to_d(&relay.seen().await) > r_circuits_at_death,
        "the return took a new circuit of R's to D"
    );

    drop(d_session);
    r.stop();
    c.stop();
    d.stop().await;
}

fn pid(peer: &TransportIdentity) -> libp2p::PeerId {
    peer.as_str().parse().expect("a libp2p identity")
}
