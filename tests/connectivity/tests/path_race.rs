// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Stage 11 step 9: direct first, the relay after a head-start, over
//! real sockets (`transport/libp2p/CONNECTIVITY.md` section 12).
//!
//! A dialer holds two routes to a target in its address book, a direct
//! address and a circuit through a bare relay that has an external
//! address, and asks `DialPeer`. What is proved:
//!
//! - with the direct route live, the direct connection lands and the
//!   circuit is never dialled: the relay sees no circuit request within
//!   the window, and a second `DialPeer` while the direct connection
//!   stands dials nothing at all (section 12's "reuse healthy direct
//!   connection": no new connection, so no new Identify);
//! - with the direct route black-holed -- a listener that accepts the
//!   TCP connection and never speaks, so the dial hangs in its
//!   handshake -- the circuit is dialled once the head-start has run
//!   out and not before: the relayed connection lands no earlier than
//!   `direct_head_start` after the ask, and the relay saw the request.
//!
//! What is NOT proved here: cancelling the losing attempt (the pinned
//! Swarm has no way to abandon a dial; the black-holed direct dial
//! fails at the handshake timeout after the test ends), and any NAT.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use futures::StreamExt as _;
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::runtime::relay_driver::{RelayClientSettings, StaticRelay};
use interweave_transport_libp2p::{
    PeerPath, RelayReservationOutcome, SubstrateConfig, SwarmEvent, SwarmRuntime,
};
use interweave_transport_runtime::TrustSources;
use interweave_transport_runtime::relay::ReservationConfig;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent as Libp2pSwarmEvent};
use libp2p::{Multiaddr, PeerId, identify, identity, relay};

const PATIENCE: Duration = Duration::from_secs(20);

/// How long a negative is watched for before it counts.
const WINDOW: Duration = Duration::from_secs(3);

/// The head-start under test: the default, so the number a profile
/// gets is the number measured.
const HEAD_START: Duration = Duration::from_millis(750);

#[derive(NetworkBehaviour)]
struct RelayBehaviour {
    identify: identify::Behaviour,
    relay: relay::Behaviour,
}

/// A bare relay server: the crate's own, defaults, no policy -- the
/// two runtimes are what is under test.
fn relay_server(keys: identity::Keypair) -> libp2p::Swarm<RelayBehaviour> {
    libp2p::SwarmBuilder::with_existing_identity(keys)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("the same transport stack the subjects use")
        .with_behaviour(|k| RelayBehaviour {
            identify: identify::Behaviour::new(identify::Config::new(
                "/interweave-relay-test-server/1".to_owned(),
                k.public(),
            )),
            relay: relay::Behaviour::new(k.public().to_peer_id(), relay::Config::default()),
        })
        .expect("behaviour")
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(600)))
        .build()
}

async fn bound(swarm: &mut libp2p::Swarm<RelayBehaviour>) -> Multiaddr {
    swarm
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().expect("a listen address"))
        .expect("listens");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, swarm.select_next_some()).await {
            Ok(Libp2pSwarmEvent::NewListenAddr { address, .. }) => return address,
            Ok(_) => {}
            Err(_) => panic!("the listener never bound"),
        }
    }
}

/// What the bare relay saw, kept across a test so a negative can be
/// read against it.
#[derive(Default)]
struct Seen {
    /// Connections the relay saw established, by peer.
    established: Vec<PeerId>,
    /// Circuits the relay accepted, as (source, destination).
    circuits: Vec<(PeerId, PeerId)>,
    /// Circuit requests the relay denied, as (source, destination).
    denied: Vec<(PeerId, PeerId)>,
}

fn note_relay(seen: &mut Seen, event: Libp2pSwarmEvent<RelayBehaviourEvent>) {
    match event {
        Libp2pSwarmEvent::ConnectionEstablished { peer_id, .. } => seen.established.push(peer_id),
        Libp2pSwarmEvent::Behaviour(RelayBehaviourEvent::Relay(
            relay::Event::CircuitReqAccepted {
                src_peer_id,
                dst_peer_id,
                ..
            },
        )) => seen.circuits.push((src_peer_id, dst_peer_id)),
        Libp2pSwarmEvent::Behaviour(RelayBehaviourEvent::Relay(
            relay::Event::CircuitReqDenied {
                src_peer_id,
                dst_peer_id,
                ..
            },
        )) => seen.denied.push((src_peer_id, dst_peer_id)),
        _ => {}
    }
}

fn identity_of(keys: &identity::Keypair) -> TransportIdentity {
    TransportIdentity::parse(keys.public().to_peer_id().to_base58()).expect("canonical")
}

fn pid(peer: &TransportIdentity) -> PeerId {
    peer.as_str().parse().expect("a libp2p identity")
}

fn trust(data_plane: &[&TransportIdentity], infra: &[&TransportIdentity]) -> TrustSources {
    TrustSources::new(
        PeerTrustPolicy::new(data_plane.iter().map(|p| (*p).clone())).expect("a small allowlist"),
        InfrastructureSet::new(infra.iter().map(|p| (*p).clone())).expect("a small set"),
    )
}

/// The relay client with no static relay: the circuit transport and
/// nothing to reserve on. What a dialer needs to reach a circuit.
fn client_without_relays() -> RelayClientSettings {
    RelayClientSettings {
        static_relays: Vec::new(),
        use_authorized_identify_relays: false,
        reservations: ReservationConfig::default(),
    }
}

fn client_reserving_on(relay: &TransportIdentity, address: &Multiaddr) -> RelayClientSettings {
    RelayClientSettings {
        static_relays: vec![StaticRelay {
            peer: relay.clone(),
            address: format!("{address}/p2p/{}", relay.as_str()),
        }],
        ..client_without_relays()
    }
}

/// Which runtime an event came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Target,
    Dialer,
}

/// The two runtimes and the relay between them.
struct Wire<'a> {
    target: &'a mut SwarmRuntime,
    dialer: &'a mut SwarmRuntime,
    relay: &'a mut libp2p::Swarm<RelayBehaviour>,
    seen: &'a mut Seen,
}

/// The circuit address `target` holds on `relay` at `relay_addr`, as
/// its reservation advertises it.
fn circuit_of(
    relay_addr: &Multiaddr,
    relay: &TransportIdentity,
    target: &TransportIdentity,
) -> Multiaddr {
    format!(
        "{relay_addr}/p2p/{}/p2p-circuit/p2p/{}",
        relay.as_str(),
        target.as_str()
    )
    .parse()
    .expect("a circuit address")
}

/// The relay, listening and advertising what it bound; and the target
/// reserved on it, its reservation accepted, with its `human` endpoint
/// configured.
struct Reserved {
    relay: libp2p::Swarm<RelayBehaviour>,
    relay_peer: TransportIdentity,
    target: SwarmRuntime,
    target_peer: TransportIdentity,
    circuit: Multiaddr,
}

async fn reserved(
    target_config: impl FnOnce(RelayClientSettings) -> SubstrateConfig,
    trust: impl FnOnce(&TransportIdentity) -> TrustSources,
) -> Reserved {
    let relay_keys = identity::Keypair::generate_ed25519();
    let relay_peer = identity_of(&relay_keys);
    let mut relay = relay_server(relay_keys);
    let relay_addr = bound(&mut relay).await;
    relay.add_external_address(relay_addr.clone());

    let target_id = ProfileIdentity::generate();
    let target_peer = target_id.transport_identity().expect("peer id");
    let mut target = SwarmRuntime::start(
        &target_id,
        target_config(client_reserving_on(&relay_peer, &relay_addr)),
        trust(&relay_peer),
    )
    .expect("the target starts");
    let circuit = circuit_of(&relay_addr, &relay_peer, &target_peer);

    // The reservation, driving the relay meanwhile.
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "the reservation was never accepted");
        tokio::select! {
            event = target.next_event() => {
                if let Some(SwarmEvent::RelayReservationChanged {
                    outcome: RelayReservationOutcome::Accepted,
                    addresses,
                    ..
                }) = event
                {
                    assert_eq!(addresses, vec![circuit.to_string()]);
                    break;
                }
            }
            _ = relay.select_next_some() => {}
            () = tokio::time::sleep(remaining) => panic!("the reservation was never accepted"),
        }
    }
    Reserved {
        relay,
        relay_peer,
        target,
        target_peer,
        circuit,
    }
}

/// Every `Connected` for `peer` on `side` with when it arrived.
fn first_connected_at(
    events: &[(Side, SwarmEvent, tokio::time::Instant)],
    side: Side,
    peer: &TransportIdentity,
    path: PeerPath,
) -> Option<tokio::time::Instant> {
    events.iter().find_map(|(s, e, at)| match e {
        SwarmEvent::Connected { peer: p, path: q } if *s == side && p == peer && *q == path => {
            Some(*at)
        }
        _ => None,
    })
}

/// Drive until `pred` or the window ends, stamping each event.
async fn drive_stamped<F>(
    wire: &mut Wire<'_>,
    window: Duration,
    mut pred: Option<F>,
) -> Vec<(Side, SwarmEvent, tokio::time::Instant)>
where
    F: FnMut(Side, &SwarmEvent) -> bool,
{
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            assert!(pred.is_none(), "timed out: {events:?}");
            return events;
        }
        tokio::select! {
            event = wire.target.next_event() => {
                let event = event.expect("the target is alive");
                let hit = pred.as_mut().is_some_and(|p| p(Side::Target, &event));
                events.push((Side::Target, event, tokio::time::Instant::now()));
                if hit { return events; }
            }
            event = wire.dialer.next_event() => {
                let event = event.expect("the dialer is alive");
                let hit = pred.as_mut().is_some_and(|p| p(Side::Dialer, &event));
                events.push((Side::Dialer, event, tokio::time::Instant::now()));
                if hit { return events; }
            }
            event = wire.relay.select_next_some() => note_relay(wire.seen, event),
            () = tokio::time::sleep(remaining) => {
                assert!(pred.is_none(), "timed out: {events:?}");
                return events;
            }
        }
    }
}

#[tokio::test]
async fn a_live_direct_route_wins_and_the_circuit_is_never_dialled() {
    let dialer_id = ProfileIdentity::generate();
    let dialer_peer = dialer_id.transport_identity().expect("peer id");
    let Reserved {
        mut relay,
        relay_peer,
        mut target,
        target_peer,
        circuit,
    } = reserved(
        |client| SubstrateConfig {
            relay_client: Some(client),
            ..SubstrateConfig::default()
        },
        |relay_peer| trust(&[&dialer_peer], &[relay_peer]),
    )
    .await;
    let direct = target
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("a loopback address"))
        .await
        .expect("the target listens");
    let mut seen = Seen::default();
    let mut dialer = SwarmRuntime::start(
        &dialer_id,
        SubstrateConfig {
            relay_client: Some(client_without_relays()),
            ..SubstrateConfig::default()
        },
        trust(&[&target_peer], &[&relay_peer]),
    )
    .expect("the dialer starts");
    // BOTH ROUTES IN THE BOOK, the circuit first -- the book's own
    // order is not the dial's.
    for address in [circuit.clone(), direct.clone()] {
        assert!(
            dialer
                .add_address(target_peer.clone(), address)
                .await
                .expect("the command reaches the task"),
            "a classified peer gets a book entry"
        );
    }
    let mut wire = Wire {
        target: &mut target,
        dialer: &mut dialer,
        relay: &mut relay,
        seen: &mut seen,
    };
    wire.dialer
        .dial_peer(target_peer.clone())
        .await
        .expect("the command reaches the task")
        .expect("the direct candidate is admitted");
    let mut events = drive_stamped(
        &mut wire,
        PATIENCE,
        Some(|s: Side, e: &SwarmEvent| {
            s == Side::Dialer
                && matches!(e, SwarmEvent::Identified { peer, .. } if *peer == target_peer)
        }),
    )
    .await;
    events.extend(drive_stamped::<fn(Side, &SwarmEvent) -> bool>(&mut wire, WINDOW, None).await);
    assert!(
        first_connected_at(&events, Side::Dialer, &target_peer, PeerPath::Direct).is_some(),
        "the direct route landed: {events:?}"
    );
    assert!(
        first_connected_at(&events, Side::Dialer, &target_peer, PeerPath::Relayed).is_none()
            && wire.seen.circuits.is_empty(),
        "the circuit was never dialled: the race was won before the head-start ran out: {:?} {events:?}",
        wire.seen.circuits
    );
    // REUSE: a second ask while the direct connection stands dials
    // nothing -- no new connection, so no new Identify at either end.
    wire.dialer
        .dial_peer(target_peer.clone())
        .await
        .expect("the command reaches the task")
        .expect("answered Ok without a dial");
    let again = drive_stamped::<fn(Side, &SwarmEvent) -> bool>(&mut wire, WINDOW, None).await;
    assert!(
        !again
            .iter()
            .any(|(_, e, _)| matches!(e, SwarmEvent::Identified { .. })),
        "nothing new was established: {again:?}"
    );
    assert!(wire.seen.circuits.is_empty());

    target.shutdown().await.expect("shutdown");
    dialer.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn a_black_holed_direct_route_yields_to_the_circuit_after_the_head_start() {
    let dialer_id = ProfileIdentity::generate();
    let dialer_peer = dialer_id.transport_identity().expect("peer id");
    let Reserved {
        mut relay,
        relay_peer,
        mut target,
        target_peer,
        circuit,
    } = reserved(
        |client| SubstrateConfig {
            relay_client: Some(client),
            ..SubstrateConfig::default()
        },
        |relay_peer| trust(&[&dialer_peer], &[relay_peer]),
    )
    .await;
    // THE BLACK HOLE: a socket that accepts the TCP connection into
    // its backlog and never speaks, so the direct dial's handshake
    // hangs -- a NAT that swallows the packets, in miniature.
    let black_hole = std::net::TcpListener::bind("127.0.0.1:0").expect("binds");
    let dead: Multiaddr = format!(
        "/ip4/127.0.0.1/tcp/{}",
        black_hole.local_addr().expect("bound").port()
    )
    .parse()
    .expect("an address");
    let mut seen = Seen::default();
    let mut dialer = SwarmRuntime::start(
        &dialer_id,
        SubstrateConfig {
            relay_client: Some(client_without_relays()),
            ..SubstrateConfig::default()
        },
        trust(&[&target_peer], &[&relay_peer]),
    )
    .expect("the dialer starts");
    for address in [circuit.clone(), dead.clone()] {
        assert!(
            dialer
                .add_address(target_peer.clone(), address)
                .await
                .expect("the command reaches the task")
        );
    }
    let mut wire = Wire {
        target: &mut target,
        dialer: &mut dialer,
        relay: &mut relay,
        seen: &mut seen,
    };
    let asked_at = tokio::time::Instant::now();
    wire.dialer
        .dial_peer(target_peer.clone())
        .await
        .expect("the command reaches the task")
        .expect("the direct candidate is admitted, the circuit deferred");
    let events = drive_stamped(
        &mut wire,
        PATIENCE,
        Some(|s: Side, e: &SwarmEvent| {
            s == Side::Dialer
                && matches!(e, SwarmEvent::Connected { peer, path: PeerPath::Relayed } if *peer == target_peer)
        }),
    )
    .await;
    let relayed_at = first_connected_at(&events, Side::Dialer, &target_peer, PeerPath::Relayed)
        .expect("matched above");
    assert!(
        relayed_at.duration_since(asked_at) >= HEAD_START,
        "the circuit waited out the head-start: {:?}",
        relayed_at.duration_since(asked_at)
    );
    assert!(
        relayed_at.duration_since(asked_at) < HEAD_START + PATIENCE / 4,
        "and not much longer: {:?}",
        relayed_at.duration_since(asked_at)
    );
    assert_eq!(
        wire.seen.circuits,
        vec![(pid(&dialer_peer), pid(&target_peer))],
        "the relay saw the deferred circuit"
    );
    assert!(
        first_connected_at(&events, Side::Dialer, &target_peer, PeerPath::Direct).is_none(),
        "the black-holed direct never landed: {events:?}"
    );
    drop(black_hole);

    target.shutdown().await.expect("shutdown");
    dialer.shutdown().await.expect("shutdown");
}
