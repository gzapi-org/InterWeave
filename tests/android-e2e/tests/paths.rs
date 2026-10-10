// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Android side and a desktop daemon over a relayed and a direct path
//! (plan §20 gate (c)'s transport cases), with the host stand-in as the
//! Android side behind the device seam: D, a desktop daemon, reserves on
//! the test relay; R, a stand-in, reaches D only through the circuit; and
//! C, a second stand-in, is given D's own address -- the direct path, and
//! the control in the same run, so the relayed reading is the relay's
//! doing and not the harness's. A direct message crosses each path both
//! ways over the client-api, each side is owed the route-begin notice
//! reading the path it took, and after R's process death and restart D is
//! owed R's return relayed with nothing before it (`reconnected`).
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
//! What this does NOT prove, by name: the device -- the Android target
//! build, its process lifecycle, its network callbacks, SELinux-confined
//! app data (the stand-in's limits, `src/lib.rs`); a daemon acting as the
//! relay (the relay is a bare Swarm carrying the production relay-server
//! field, `interweave_test_support::e2e::relay`); anything off loopback --
//! the relay and D listen on 127.0.0.1; any NAT; more than one
//! host; a host whose interfaces carry a public address, where D could
//! learn a direct address for R -- the case assumes this host's interface
//! addresses are private. The `HumanChatV2` cases ride the same seam and
//! topology in `human_chat.rs`.

#![cfg(target_os = "linux")]
#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_android_e2e_tests::{
    Desktop, Device, HostStandIn, PATIENCE, Relay, free_port, human, lease_request,
    relayed_example_of,
};
use interweave_local_client_api::{
    DataSessionBinding as _, DataSessionPort, LocalSessionEvent, RECONNECTED, ROUTE_ESTABLISHED,
    SessionEvent,
};
use interweave_transport_api::{
    DirectDestination, MediaType, MessageId, Payload, PeerPath, TransportIdentity,
};

fn payload(text: &str) -> Payload {
    Payload::at_ceiling(
        Some(MediaType::parse("text/plain").expect("a media type")),
        text.as_bytes().to_vec(),
    )
    .expect("a payload")
}

/// Where a stand-in listens: an `embedded-android` profile listens on a
/// wildcard only, since a listener on one address dies with it.
const WILDCARD: &str = "/ip4/0.0.0.0/tcp/0";

fn loopback() -> String {
    format!(
        "/ip4/127.0.0.1/tcp/{}",
        free_port(std::net::Ipv4Addr::LOCALHOST)
    )
}

/// Send `text` from `from` to `to` until the route exists, within
/// `PATIENCE`; `log` is shown on failure.
async fn send_until_routed(
    from: &impl DataSessionPort,
    to: &TransportIdentity,
    id: u8,
    text: &str,
    log: impl Fn() -> String,
) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let sent = from
            .send_direct(
                DirectDestination {
                    peer: to.clone(),
                    endpoint: Some(human()),
                },
                MessageId::from_bytes([id; 16]),
                payload(text),
            )
            .await;
        if sent.is_ok() {
            return;
        }
        // The refusal itself is not printed: this helper is generic over
        // every binding, the in-memory fake's among them, whose refusal
        // carries a trusted peer's identity, and a static analysis reads
        // printing it as logging that identity in clear. What failing
        // here means is the message:
        // no route by the deadline. D's log, shown with it, explains D's
        // side only -- a stand-in's refusal is in no log this case keeps.
        assert!(
            tokio::time::Instant::now() < deadline,
            "no route to {} within the deadline\n{}",
            to.as_str(),
            log()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// What one session has been told so far, kept across reads.
#[derive(Default)]
struct Told {
    /// Path notices as (peer, previous, current, reason class), in order.
    paths: Vec<(TransportIdentity, Option<PeerPath>, PeerPath, String)>,
    /// Disconnects, by peer, each with how many path notices preceded it.
    gone: Vec<(TransportIdentity, usize)>,
}

impl Told {
    /// Read `session` once, keeping what it said; whether a direct
    /// message `text` from `source` was among it.
    async fn read(
        &mut self,
        session: &impl DataSessionPort,
        source: &TransportIdentity,
        text: &str,
    ) -> bool {
        let mut found = false;
        for event in session.events(64).await.expect("events") {
            match event {
                SessionEvent::Direct(m)
                    if &m.source_peer == source && m.payload.bytes() == text.as_bytes() =>
                {
                    found = true;
                }
                SessionEvent::Local(LocalSessionEvent::PeerPathChanged {
                    peer,
                    previous,
                    current,
                    reason_class,
                    ..
                }) => self.paths.push((peer, previous, current, reason_class)),
                SessionEvent::Local(LocalSessionEvent::PeerDisconnected { peer, .. }) => {
                    self.gone.push((peer, self.paths.len()));
                }
                _ => {}
            }
        }
        found
    }

    /// Read until `text` from `source` arrives, within `PATIENCE`.
    async fn take_message(
        &mut self,
        session: &impl DataSessionPort,
        source: &TransportIdentity,
        text: &str,
    ) {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        while !self.read(session, source, text).await {
            assert!(
                tokio::time::Instant::now() < deadline,
                "{text:?} from {} never arrived",
                source.as_str()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Read until a path notice about `peer` with reason `class` has been
    /// told at or after position `from`, within `PATIENCE`; its position
    /// and the notice.
    async fn path_notice(
        &mut self,
        session: &impl DataSessionPort,
        peer: &TransportIdentity,
        class: &str,
        from: usize,
    ) -> (usize, (Option<PeerPath>, PeerPath)) {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            if let Some((at, (_, previous, current, _))) = self
                .paths
                .iter()
                .enumerate()
                .skip(from)
                .find(|(_, (p, _, _, c))| p == peer && c == class)
            {
                return (at, (*previous, *current));
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "no {class} notice for {}: {:?}",
                peer.as_str(),
                self.paths
            );
            self.read(session, peer, "").await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_android_side_crosses_a_relayed_and_a_direct_path_to_a_desktop_both_ways() {
    relayed_and_direct(HostStandIn::new(), HostStandIn::new()).await;
}

/// The case over any [`Device`]: R and C unstarted, their `PeerId`s known.
async fn relayed_and_direct<A: Device>(mut r: A, mut c: A) {
    let mut d = Desktop::new();
    let (d_peer, r_peer, c_peer) = (d.peer.clone(), r.peer(), c.peer());
    let relay = Relay::start(&[&d_peer, &r_peer, &c_peer]).await;
    let d_listen = loopback();
    let direct_to_d = format!("{d_listen}/p2p/{}", d_peer.as_str());
    d.start(&relayed_example_of(
        "human-desktop.yaml",
        &[&r_peer, &c_peer],
        &d_listen,
        &relay,
        None,
    ))
    .await;

    // D's reservation first: until it holds, the relay has nowhere to
    // carry R's circuit.
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while !relay.seen().await.reservations.contains(&pid(&d_peer)) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "D never reserved on the relay:\n{}",
            d.log()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let r_config = relayed_example_of(
        "human-android.yaml",
        &[&d_peer],
        WILDCARD,
        &relay,
        Some(&relay.circuit_to(&d_peer)),
    );
    r.start(&r_config);
    c.start(&relayed_example_of(
        "human-android.yaml",
        &[&d_peer],
        WILDCARD,
        &relay,
        Some(&direct_to_d),
    ));

    let d_session = d.binding().open(lease_request()).await.expect("D leases");
    let r_session = r.binding().open(lease_request()).await.expect("R leases");
    let c_session = c.binding().open(lease_request()).await.expect("C leases");
    let (mut d_told, mut r_told, mut c_told) = (Told::default(), Told::default(), Told::default());

    // R -> D over the circuit and back; C -> D over D's address and back.
    send_until_routed(&r_session, &d_peer, 1, "r to d", || d.log()).await;
    d_told.take_message(&d_session, &r_peer, "r to d").await;
    send_until_routed(&d_session, &r_peer, 2, "d to r", || d.log()).await;
    r_told.take_message(&r_session, &d_peer, "d to r").await;
    send_until_routed(&c_session, &d_peer, 3, "c to d", || d.log()).await;
    d_told.take_message(&d_session, &c_peer, "c to d").await;
    send_until_routed(&d_session, &c_peer, 4, "d to c", || d.log()).await;
    c_told.take_message(&c_session, &d_peer, "d to c").await;

    // Each route began on the path the relay's record says it took.
    let relayed = (None, PeerPath::Relayed);
    let direct = (None, PeerPath::Direct);
    for (told, session, peer, want, what) in [
        (&mut r_told, &r_session, &d_peer, relayed, "R of D: relayed"),
        (
            &mut c_told,
            &c_session,
            &d_peer,
            direct,
            "C of D, the control: direct",
        ),
    ] {
        assert_eq!(
            told.path_notice(session, peer, ROUTE_ESTABLISHED, 0)
                .await
                .1,
            want,
            "{what}"
        );
    }
    for (peer, want, what) in [
        (&r_peer, relayed, "D of R: relayed"),
        (&c_peer, direct, "D of C, the control: direct"),
    ] {
        assert_eq!(
            d_told
                .path_notice(&d_session, peer, ROUTE_ESTABLISHED, 0)
                .await
                .1,
            want,
            "{what}"
        );
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
    // return over a new circuit, relayed with nothing before it.
    let before = seen.circuits.len();
    d_told.read(&d_session, &r_peer, "").await;
    let (told_before, gone_before) = (d_told.paths.len(), d_told.gone.len());
    drop(r_session);
    r.kill();
    r.restart();
    let r_session = r
        .binding()
        .open(lease_request())
        .await
        .expect("R leases again");
    send_until_routed(&r_session, &d_peer, 5, "r again", || d.log()).await;
    d_told.take_message(&d_session, &r_peer, "r again").await;
    let (back_at, back) = d_told
        .path_notice(&d_session, &r_peer, RECONNECTED, told_before)
        .await;
    assert_eq!(
        back, relayed,
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
        relay.seen().await.circuits.len() > before,
        "the return took a new circuit"
    );

    drop((d_session, r_session, c_session));
    r.stop();
    c.stop();
    d.stop().await;
}

fn pid(peer: &TransportIdentity) -> libp2p::PeerId {
    peer.as_str().parse().expect("a libp2p identity")
}
