// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Stage 11 step 6: the relay SERVER over real sockets.
//!
//! What is proved here, with the production runtime as the relay and
//! bare relay clients as the requesters:
//!
//! - an authorized infrastructure-only client's inbound is retained
//!   (route 3, widened under `RelayReservation`), offered Identify and
//!   the hop protocol and nothing else, and its reservation is accepted
//!   -- the subject's event and the client's own agree;
//! - the per-peer ceiling is EXACT: a second reservation for the same
//!   PeerId over another connection is denied `ResourceLimitExceeded`,
//!   which the crate's own comparison would have admitted (SPIKE-004
//!   F10: `>` where the ceiling says `>=`);
//! - the global ceiling is exact too: with two allowed, the third
//!   authorized peer is denied;
//! - a peer in no trust set is closed at establishment as before and
//!   never reaches the hop protocol (the negative control);
//! - a circuit request from one authorized client to another reaches
//!   the subject and is answered as a circuit event naming both ends.
//!
//! What is NOT proved on loopback: a USABLE reservation and a circuit
//! that carries bytes. A reservation's addresses are the relay's own
//! external addresses (SPIKE-004 note 10), and this profile advertises
//! only what the AutoNAT verdict verified -- which loopback cannot
//! produce, since `AUTONAT.md` §6 refuses a loopback candidate. So the
//! subject's reservations carry no address, the bare client closes its
//! listener with `NoAddressesInReservation` after the acceptance, and
//! the circuit below is answered at the relay and fails at its far end.
//! SPIKE-004 phase B is where a reservation carries an address.

#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::time::Duration;

use futures::StreamExt as _;
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::runtime::relay_server_driver::RelayServerSettings;
use interweave_transport_libp2p::{RelayServerOutcome, SubstrateConfig, SwarmEvent, SwarmRuntime};
use interweave_transport_runtime::TrustSources;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::multiaddr::Protocol;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent as Libp2pSwarmEvent};
use libp2p::{Multiaddr, PeerId, identify, identity, relay};

const PATIENCE: Duration = Duration::from_secs(20);
const WINDOW: Duration = Duration::from_secs(3);

const OFFERED_TO_A_CLIENT: &[&str] = &[
    "/ipfs/id/1.0.0",
    "/ipfs/id/push/1.0.0",
    "/libp2p/circuit/relay/0.2.0/hop",
];

#[derive(NetworkBehaviour)]
struct ClientBehaviour {
    identify: identify::Behaviour,
    relay: relay::client::Behaviour,
}

/// A bare relay client: the crate's own, with its transport.
fn client(keys: identity::Keypair) -> libp2p::Swarm<ClientBehaviour> {
    libp2p::SwarmBuilder::with_existing_identity(keys)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("the same transport stack the subject uses")
        .with_relay_client(libp2p::noise::Config::new, libp2p::yamux::Config::default)
        .expect("relay client")
        .with_behaviour(|k, relay| ClientBehaviour {
            identify: identify::Behaviour::new(identify::Config::new(
                "/interweave-relay-test-client/1".to_owned(),
                k.public(),
            )),
            relay,
        })
        .expect("behaviour")
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(600)))
        .build()
}

fn identity_of(keys: &identity::Keypair) -> TransportIdentity {
    TransportIdentity::parse(keys.public().to_peer_id().to_base58()).expect("canonical")
}

fn infrastructure_only(infra: &[&TransportIdentity]) -> TrustSources {
    TrustSources::new(
        PeerTrustPolicy::new(std::iter::empty()).expect("an empty allowlist"),
        InfrastructureSet::new(infra.iter().map(|p| (*p).clone())).expect("a small set"),
    )
}

/// What one bare client saw.
#[derive(Default, Debug)]
struct Seen {
    accepted: usize,
    /// The protocol set the subject offered on this client's connection.
    offered: Option<BTreeSet<String>>,
    connections_closed: usize,
    listeners_closed: Vec<String>,
}

fn note(seen: &mut Seen, subject: PeerId, event: Libp2pSwarmEvent<ClientBehaviourEvent>) {
    match event {
        Libp2pSwarmEvent::Behaviour(ClientBehaviourEvent::Relay(
            relay::client::Event::ReservationReqAccepted { .. },
        )) => seen.accepted += 1,
        Libp2pSwarmEvent::Behaviour(ClientBehaviourEvent::Identify(
            identify::Event::Received { peer_id, info, .. },
        )) if peer_id == subject => {
            seen.offered = Some(info.protocols.iter().map(ToString::to_string).collect());
        }
        Libp2pSwarmEvent::ConnectionClosed { peer_id, .. } if peer_id == subject => {
            seen.connections_closed += 1;
        }
        Libp2pSwarmEvent::ListenerClosed { reason, .. } => {
            seen.listeners_closed.push(format!("{reason:?}"));
        }
        _ => {}
    }
}

/// Drive the subject and every client for `window`, or until `pred`
/// matches a subject event; returns the subject's events seen.
async fn drive<F>(
    subject: &mut SwarmRuntime,
    subject_pid: PeerId,
    clients: &mut [(&mut libp2p::Swarm<ClientBehaviour>, &mut Seen)],
    what: &str,
    window: Duration,
    mut pred: Option<F>,
) -> Vec<SwarmEvent>
where
    F: FnMut(&SwarmEvent) -> bool,
{
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            assert!(pred.is_none(), "timed out waiting for {what}: {events:?}");
            return events;
        }
        let n = clients.len();
        let next_client = async {
            // Poll each client in turn; whichever is ready first.
            let mut futures = Vec::with_capacity(n);
            for (swarm, _) in clients.iter_mut() {
                futures.push(Box::pin(swarm.select_next_some()));
            }
            let (event, index, _) = futures::future::select_all(futures).await;
            (index, event)
        };
        tokio::select! {
            event = subject.next_event() => {
                let event = event.expect("the runtime is alive");
                let hit = pred.as_mut().is_some_and(|p| p(&event));
                events.push(event);
                if hit {
                    return events;
                }
            }
            (index, event) = next_client, if n > 0 => {
                note(clients[index].1, subject_pid, event);
            }
            () = tokio::time::sleep(remaining) => {
                assert!(pred.is_none(), "timed out waiting for {what}: {events:?}");
                return events;
            }
        }
    }
}

fn reserve_on(
    client: &mut libp2p::Swarm<ClientBehaviour>,
    subject_addr: &Multiaddr,
    subject: PeerId,
) {
    client
        .listen_on(
            subject_addr
                .clone()
                .with(Protocol::P2p(subject))
                .with(Protocol::P2pCircuit),
        )
        .expect("a circuit listen is accepted by the transport");
}

fn served(events: &[SwarmEvent], peer: &TransportIdentity) -> Vec<RelayServerOutcome> {
    events
        .iter()
        .filter_map(|e| match e {
            SwarmEvent::RelayServed {
                peer: p, outcome, ..
            } if p == peer => Some(outcome.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn the_relay_server_serves_authorized_peers_within_exact_ceilings_and_nobody_else() {
    // THE SUBJECT: the production runtime as a relay, two reservations
    // in all and one per peer, three peers authorized infrastructure-only.
    let keys_a = identity::Keypair::generate_ed25519();
    let keys_b = identity::Keypair::generate_ed25519();
    let keys_c = identity::Keypair::generate_ed25519();
    let keys_d = identity::Keypair::generate_ed25519();
    let (peer_a, peer_b, peer_c, peer_d) = (
        identity_of(&keys_a),
        identity_of(&keys_b),
        identity_of(&keys_c),
        identity_of(&keys_d),
    );
    let subject_id = ProfileIdentity::generate();
    let subject_peer = subject_id.transport_identity().expect("peer id");
    let subject_pid: PeerId = subject_peer.as_str().parse().expect("a libp2p identity");
    let config = SubstrateConfig {
        relay_server: Some(RelayServerSettings {
            max_reservations: 2,
            max_reservations_per_peer: 1,
            ..RelayServerSettings::default()
        }),
        ..SubstrateConfig::default()
    };
    let mut subject = SwarmRuntime::start(
        &subject_id,
        config,
        infrastructure_only(&[&peer_a, &peer_b, &peer_c]),
    )
    .expect("the runtime starts");
    let subject_addr = subject
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("a listen address"))
        .await
        .expect("the subject listens");

    // A RESERVES. Nobody dialled the subject for it: the client's
    // behaviour dials the relay for its reservation, the subject sees an
    // inbound from an infrastructure-only peer and RETAINS it, offers
    // the hop protocol, and accepts.
    let mut a = client(keys_a.clone());
    let mut seen_a = Seen::default();
    reserve_on(&mut a, &subject_addr, subject_pid);
    let events = drive(
        &mut subject,
        subject_pid,
        &mut [(&mut a, &mut seen_a)],
        "A's reservation to be accepted",
        PATIENCE,
        Some(|e: &SwarmEvent| {
            matches!(e, SwarmEvent::RelayServed { peer, outcome: RelayServerOutcome::ReservationAccepted, .. } if *peer == peer_a)
        }),
    )
    .await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SwarmEvent::Connected { peer, .. } if *peer == peer_a)),
        "A's inbound was retained and announced: {events:?}"
    );
    let mut all = events;
    // The client's own acceptance, and what it was offered on the
    // retained connection.
    all.extend(
        drive::<fn(&SwarmEvent) -> bool>(
            &mut subject,
            subject_pid,
            &mut [(&mut a, &mut seen_a)],
            "settling",
            WINDOW,
            None,
        )
        .await,
    );
    // The client's side of the same exchange on loopback: the relay
    // accepted and reported NO address (the module note), which the
    // pinned client turns into a closed listener naming the reason --
    // the reservation stands on the relay, and the client could not use
    // it. Two ends of one exchange, and the loopback limit stated where
    // it bites.
    assert!(
        seen_a
            .listeners_closed
            .iter()
            .any(|r| r.contains("NoAddressesInReservation")),
        "A's client saw the acceptance carry no address: {seen_a:?}"
    );
    let expected: BTreeSet<String> = OFFERED_TO_A_CLIENT
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    assert_eq!(
        seen_a.offered.as_ref(),
        Some(&expected),
        "the retained infrastructure-only inbound is offered Identify and the hop protocol and nothing else"
    );
    assert_eq!(seen_a.connections_closed, 0, "and the connection stays");

    // THE PER-PEER CEILING IS EXACT. The same PeerId on a second
    // connection asks again: denied. The crate's own comparison admits
    // one more than told, so the runtime hands it one below.
    let pid_a = keys_a.public().to_peer_id();
    let mut a2 = client(keys_a);
    let mut seen_a2 = Seen::default();
    reserve_on(&mut a2, &subject_addr, subject_pid);
    let events = drive(
        &mut subject,
        subject_pid,
        &mut [(&mut a, &mut seen_a), (&mut a2, &mut seen_a2)],
        "A's second reservation to be denied",
        PATIENCE,
        Some(|e: &SwarmEvent| {
            matches!(e, SwarmEvent::RelayServed { peer, outcome: RelayServerOutcome::ReservationDenied { .. }, .. } if *peer == peer_a)
        }),
    )
    .await;
    let denied: Vec<_> = served(&events, &peer_a);
    assert!(
        denied.iter().any(|o| matches!(o, RelayServerOutcome::ReservationDenied { status } if status.contains("ResourceLimitExceeded"))),
        "denied for the ceiling, by name: {denied:?}"
    );
    all.extend(events);
    let _ = drive::<fn(&SwarmEvent) -> bool>(
        &mut subject,
        subject_pid,
        &mut [(&mut a, &mut seen_a), (&mut a2, &mut seen_a2)],
        "settling",
        WINDOW,
        None,
    )
    .await;
    assert!(
        seen_a2
            .listeners_closed
            .iter()
            .any(|r| r.contains("ResourceLimitExceeded")),
        "and A's second client was told so: {seen_a2:?}"
    );
    assert_eq!(
        seen_a2.connections_closed, 0,
        "its connection is retained all the same"
    );

    // B RESERVES: the second of two.
    let mut b = client(keys_b);
    let mut seen_b = Seen::default();
    reserve_on(&mut b, &subject_addr, subject_pid);
    let events = drive(
        &mut subject,
        subject_pid,
        &mut [(&mut a, &mut seen_a), (&mut a2, &mut seen_a2), (&mut b, &mut seen_b)],
        "B's reservation to be accepted",
        PATIENCE,
        Some(|e: &SwarmEvent| {
            matches!(e, SwarmEvent::RelayServed { peer, outcome: RelayServerOutcome::ReservationAccepted, .. } if *peer == peer_b)
        }),
    )
    .await;
    all.extend(events);

    // THE GLOBAL CEILING IS EXACT: the third authorized peer is denied.
    let mut c = client(keys_c);
    let mut seen_c = Seen::default();
    reserve_on(&mut c, &subject_addr, subject_pid);
    let events = drive(
        &mut subject,
        subject_pid,
        &mut [(&mut a, &mut seen_a), (&mut a2, &mut seen_a2), (&mut b, &mut seen_b), (&mut c, &mut seen_c)],
        "C's reservation to be denied",
        PATIENCE,
        Some(|e: &SwarmEvent| {
            matches!(e, SwarmEvent::RelayServed { peer, outcome: RelayServerOutcome::ReservationDenied { .. }, .. } if *peer == peer_c)
        }),
    )
    .await;
    assert!(
        served(&events, &peer_c).iter().any(|o| matches!(o, RelayServerOutcome::ReservationDenied { status } if status.contains("ResourceLimitExceeded"))),
        "C denied for the global ceiling: {events:?}"
    );
    all.extend(events);

    // THE NEGATIVE CONTROL: a peer in no trust set. Its inbound is
    // established and closed as it always was; it is never offered the
    // hop protocol and the subject serves it nothing.
    let mut d = client(keys_d);
    let mut seen_d = Seen::default();
    reserve_on(&mut d, &subject_addr, subject_pid);
    let events = drive::<fn(&SwarmEvent) -> bool>(
        &mut subject,
        subject_pid,
        &mut [
            (&mut a, &mut seen_a),
            (&mut a2, &mut seen_a2),
            (&mut b, &mut seen_b),
            (&mut c, &mut seen_c),
            (&mut d, &mut seen_d),
        ],
        "settling",
        WINDOW,
        None,
    )
    .await;
    assert!(
        served(&events, &peer_d).is_empty(),
        "the stranger is served nothing: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SwarmEvent::Connected { peer, .. } if *peer == peer_d)),
        "and never announced: {events:?}"
    );
    assert!(
        seen_d.connections_closed >= 1,
        "D saw its connection closed: {seen_d:?}"
    );
    assert_eq!(seen_d.accepted, 0);
    assert!(
        seen_d
            .offered
            .as_ref()
            .is_none_or(|o| !o.iter().any(|p| p.contains("relay"))),
        "D was never offered the hop protocol: {:?}",
        seen_d.offered
    );
    all.extend(events);

    // A CIRCUIT REQUEST reaches the subject: B asks for A through it.
    // Answered at the relay as a circuit event naming both ends; on
    // loopback it cannot carry bytes (the module note).
    let circuit: Multiaddr = subject_addr
        .clone()
        .with(Protocol::P2p(subject_pid))
        .with(Protocol::P2pCircuit)
        .with(Protocol::P2p(pid_a));
    b.dial(circuit)
        .expect("a circuit dial is accepted by the transport");
    let events = drive(
        &mut subject,
        subject_pid,
        &mut [(&mut a, &mut seen_a), (&mut a2, &mut seen_a2), (&mut b, &mut seen_b), (&mut c, &mut seen_c), (&mut d, &mut seen_d)],
        "the circuit request to be answered",
        PATIENCE,
        Some(|e: &SwarmEvent| {
            matches!(e, SwarmEvent::RelayServed { peer, destination: Some(dst), outcome, .. }
                if *peer == peer_b && *dst == peer_a
                && matches!(outcome, RelayServerOutcome::CircuitAccepted | RelayServerOutcome::CircuitDenied { .. } | RelayServerOutcome::CircuitExchangeFailed { .. } | RelayServerOutcome::CircuitClosed { .. }))
        }),
    )
    .await;
    let circuit_events = served(&events, &peer_b);
    assert!(
        circuit_events.iter().any(|o| !matches!(
            o,
            RelayServerOutcome::ReservationAccepted
                | RelayServerOutcome::ReservationRenewed
                | RelayServerOutcome::ReservationDenied { .. }
        )),
        "the subject answered the circuit request with a circuit event: {circuit_events:?}"
    );
    all.extend(events);
    // Every client the subject served kept its connection; the
    // stranger lost its own.
    assert_eq!(
        seen_a.connections_closed + seen_b.connections_closed + seen_c.connections_closed,
        0
    );
    assert!(
        seen_c
            .listeners_closed
            .iter()
            .any(|r| r.contains("ResourceLimitExceeded")),
        "C's client was told the global ceiling: {seen_c:?}"
    );
    // And no event of the subject's ever named the stranger.
    assert!(
        !all.iter()
            .any(|e| matches!(e, SwarmEvent::RelayServed { peer, .. } if *peer == peer_d)),
        "{all:?}"
    );

    subject.shutdown().await.expect("shutdown");
}
