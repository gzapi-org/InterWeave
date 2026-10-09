// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! A real relayed path between two daemons (plan §18's carried limit;
//! desktop<->desktop groundwork for §20 gate (c)'s relayed row, which is
//! Android<->desktop and not proved here): A reserves on a relay, B's only route to
//! A is the circuit through it, and a direct message crosses the circuit
//! both ways over IPC. C, given A's own address, is the direct control
//! in the same run, so the relayed reading is the relay's doing and not
//! the harness's.
//!
//! What the client-api reports of it is the route-begin notice (A
//! 2026-10-09, `LOCAL-CLIENT.md` item 12): each session is owed
//! `PeerPathChanged` with no `previous` and the path its route began on --
//! relayed between A and B, direct between A and C -- and, after B
//! restarts, A's session is owed B's return the same way (`reconnected`).
//!
//! What makes the circuit the ONLY route is the production boundary, not
//! a test switch: every daemon listens on loopback, ADR-0052's floor
//! refuses a peer-advertised loopback address at the address book's
//! door, and DCUtR's boundary refuses a loopback candidate before any
//! socket -- while the operator's door admits the profile's own static
//! relay and static entry (`common::relayed_example`).
//!
//! What this does not prove: a daemon acting as the relay (on one host
//! its hop gate never opens -- `common::relay`'s module note; the gate
//! itself is `relay_hop_gate.rs`'s), any NAT, or more than one host.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_ipc_client::IpcSession;
use interweave_local_client_api::{
    DataSessionBinding as _, DataSessionPort as _, LocalSessionEvent, RECONNECTED,
    ROUTE_ESTABLISHED, SessionEvent,
};
use interweave_transport_api::{
    ConnectivitySummary, DirectDestination, MediaType, MessageId, Payload, PeerPath,
    TransportIdentity,
};

mod common;

use common::relay::Relay;
use common::{Daemon, Home, PATIENCE, free_port, human, lease_request, relayed_example};

fn payload(text: &str) -> Payload {
    Payload::at_ceiling(
        Some(MediaType::parse("text/plain").expect("a media type")),
        text.as_bytes().to_vec(),
    )
    .expect("a payload")
}

/// Send until the route exists, within `within`, failing with both
/// daemons' logs.
async fn send_until_routed(
    from: &IpcSession,
    to: &TransportIdentity,
    id: u8,
    text: &str,
    daemons: [&Daemon; 2],
    within: Duration,
) {
    let deadline = tokio::time::Instant::now() + within;
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
        assert!(
            tokio::time::Instant::now() < deadline,
            "no route to {}: {sent:?}\nfrom:\n{}\nto:\n{}",
            to.as_str(),
            daemons[0].log(),
            daemons[1].log()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// How soon after B serves its first circuit to A must carry a message:
/// well under the thirty-second peer backoff a circuit dial lost to the
/// relay hop used to cost (`ConnectionManager::record_relay_hop_unreached`),
/// so a daemon that backs A off for losing that race fails here rather
/// than passing on the margin of `PATIENCE`.
const PROMPT: Duration = Duration::from_secs(10);

/// What one session has been told so far, kept across reads.
#[derive(Default)]
struct Told {
    /// Connectivity summaries, newest last.
    states: Vec<ConnectivitySummary>,
    /// Path notices as (peer, previous, current, reason class), in order.
    paths: Vec<(TransportIdentity, Option<PeerPath>, PeerPath, String)>,
    /// Disconnects, by peer, each with how many path notices had been
    /// told before it -- so an order across the two lists can be read.
    gone: Vec<(TransportIdentity, usize)>,
}

impl Told {
    /// Read `session` once, keeping what it said; whether a direct
    /// message `text` from `source` was among it.
    async fn read(&mut self, session: &IpcSession, source: &TransportIdentity, text: &str) -> bool {
        let mut found = false;
        for event in session.events(64).await.expect("events") {
            match event {
                SessionEvent::Direct(m)
                    if &m.source_peer == source && m.payload.bytes() == text.as_bytes() =>
                {
                    found = true;
                }
                SessionEvent::Local(LocalSessionEvent::ServerState {
                    connectivity: Some(c),
                    ..
                }) => self.states.push(c),
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
    async fn take_message(&mut self, session: &IpcSession, source: &TransportIdentity, text: &str) {
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
        session: &IpcSession,
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

/// The newest summary a session reports, after waiting for one where
/// `want` holds -- or the last one seen when it never does, for the
/// assertion to name.
async fn summary_where(
    session: &IpcSession,
    mut newest: Option<ConnectivitySummary>,
    want: impl Fn(&ConnectivitySummary) -> bool,
) -> Option<ConnectivitySummary> {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while !newest.as_ref().is_some_and(&want) && tokio::time::Instant::now() < deadline {
        for event in session.events(64).await.expect("events") {
            if let SessionEvent::Local(LocalSessionEvent::ServerState {
                connectivity: Some(c),
                ..
            }) = event
            {
                newest = Some(c);
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    newest
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_direct_message_crosses_a_real_circuit_both_ways_beside_a_direct_control() {
    let (a, b, c) = (
        Home::new("human-desktop"),
        Home::new("human-desktop"),
        Home::new("human-desktop"),
    );
    let (a_peer, b_peer, c_peer) = (a.write_key(), b.write_key(), c.write_key());
    let relay = Relay::start(&[&a_peer, &b_peer, &c_peer]).await;
    let loopback = std::net::Ipv4Addr::LOCALHOST;
    let a_listen = format!("/ip4/127.0.0.1/tcp/{}", free_port(loopback));
    let direct_to_a = format!("{a_listen}/p2p/{}", a_peer.as_str());
    let circuit_to_a = relay.circuit_to(&a_peer);
    a.write_config(&relayed_example(
        &[&b_peer, &c_peer],
        &a_listen,
        &relay,
        None,
    ));
    b.write_config(&relayed_example(
        &[&a_peer],
        &format!("/ip4/127.0.0.1/tcp/{}", free_port(loopback)),
        &relay,
        Some(&circuit_to_a),
    ));
    c.write_config(&relayed_example(
        &[&a_peer],
        &format!("/ip4/127.0.0.1/tcp/{}", free_port(loopback)),
        &relay,
        Some(&direct_to_a),
    ));
    let mut a_daemon = a.start(&[]);
    a_daemon.serving(&a).await;

    // A's reservation first: until it holds, the relay has nowhere to
    // carry B's circuit, and B's first sends fail as no route.
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while !relay.seen().await.reservations.contains(&pid(&a_peer)) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "A never reserved on the relay:\n{}",
            a_daemon.log()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let mut b_daemon = b.start(&[]);
    b_daemon.serving(&b).await;
    let mut c_daemon = c.start(&[]);
    c_daemon.serving(&c).await;

    let a_session = a.binding().open(lease_request()).await.expect("A leases");
    let b_session = b.binding().open(lease_request()).await.expect("B leases");
    let c_session = c.binding().open(lease_request()).await.expect("C leases");

    let (mut a_told, mut b_told, mut c_told) = (Told::default(), Told::default(), Told::default());

    // B -> A over the circuit, then A -> B back over it.
    send_until_routed(
        &b_session,
        &a_peer,
        1,
        "b to a",
        [&b_daemon, &a_daemon],
        PROMPT,
    )
    .await;
    a_told.take_message(&a_session, &b_peer, "b to a").await;
    send_until_routed(
        &a_session,
        &b_peer,
        2,
        "a to b",
        [&a_daemon, &b_daemon],
        PATIENCE,
    )
    .await;
    b_told.take_message(&b_session, &a_peer, "a to b").await;

    // C -> A over A's own address: the control.
    send_until_routed(
        &c_session,
        &a_peer,
        3,
        "c to a",
        [&c_daemon, &a_daemon],
        PATIENCE,
    )
    .await;
    a_told.take_message(&a_session, &c_peer, "c to a").await;

    // The route indicator's source: each route began on the path the
    // relay's record and the control say it took.
    let begun = (None, PeerPath::Relayed);
    assert_eq!(
        b_told
            .path_notice(&b_session, &a_peer, ROUTE_ESTABLISHED, 0)
            .await
            .1,
        begun,
        "B of A: relayed from the route's begin"
    );
    assert_eq!(
        a_told
            .path_notice(&a_session, &b_peer, ROUTE_ESTABLISHED, 0)
            .await
            .1,
        begun,
        "A of B: relayed from the route's begin"
    );
    let direct = (None, PeerPath::Direct);
    assert_eq!(
        c_told
            .path_notice(&c_session, &a_peer, ROUTE_ESTABLISHED, 0)
            .await
            .1,
        direct,
        "C of A, the control: direct"
    );
    assert_eq!(
        a_told
            .path_notice(&a_session, &c_peer, ROUTE_ESTABLISHED, 0)
            .await
            .1,
        direct,
        "A of C, the control: direct"
    );

    let seen = relay.seen().await;
    assert!(
        seen.circuits.contains(&(pid(&b_peer), pid(&a_peer))),
        "the relay carried B's circuit to A: {seen:?}"
    );
    let touches_c =
        |(s, d): &(libp2p::PeerId, libp2p::PeerId)| *s == pid(&c_peer) || *d == pid(&c_peer);
    assert!(
        !seen.circuits.iter().any(touches_c) && !seen.denied.iter().any(touches_c),
        "no circuit to or from the control, asked or denied: {seen:?}"
    );

    // What the client-api says of each path: B's one peer is relayed,
    // C's is not.
    let b_now = summary_where(&b_session, b_told.states.last().cloned(), |s| {
        s.active_relayed_peer_paths == 1
    })
    .await;
    assert_eq!(
        b_now.map(|s| s.active_relayed_peer_paths),
        Some(1),
        "B reports its path to A as relayed"
    );
    // C's side needs no summary: its notice above says direct, and the
    // relay's record says no circuit touched it. A summary would prove
    // nothing more -- the earliest one, from before C connected, reads
    // zero relayed paths too.

    // B restarts: A's route to B stands, the disconnect is told, and B's
    // return over the circuit is owed with nothing before it.
    let before = relay.seen().await.circuits.len();
    let told_before = a_told.paths.len();
    drop(b_session);
    assert!(b_daemon.terminate().await.success(), "{}", b_daemon.log());
    let mut b_daemon = b.start(&[]);
    b_daemon.serving(&b).await;
    let b_session = b
        .binding()
        .open(lease_request())
        .await
        .expect("B leases again");
    send_until_routed(
        &b_session,
        &a_peer,
        4,
        "b again",
        [&b_daemon, &a_daemon],
        PROMPT,
    )
    .await;
    a_told.take_message(&a_session, &b_peer, "b again").await;
    let (back_at, back) = a_told
        .path_notice(&a_session, &b_peer, RECONNECTED, told_before)
        .await;
    assert_eq!(
        back,
        (None, PeerPath::Relayed),
        "A of B after B's restart: relayed again, nothing before it"
    );
    assert!(
        a_told
            .gone
            .iter()
            .any(|(p, at)| p == &b_peer && (told_before..=back_at).contains(at)),
        "the disconnect was told after the restart began and before the return: {:?}",
        a_told.gone
    );
    assert!(
        relay.seen().await.circuits.len() > before,
        "the return took a new circuit"
    );
    for (name, daemon) in [
        ("A", &mut a_daemon),
        ("B", &mut b_daemon),
        ("C", &mut c_daemon),
    ] {
        assert!(
            daemon.terminate().await.success(),
            "{name}: {}",
            daemon.log()
        );
    }
}

fn pid(peer: &TransportIdentity) -> libp2p::PeerId {
    peer.as_str().parse().expect("a libp2p identity")
}
