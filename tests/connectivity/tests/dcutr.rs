// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Stage 11 step 8: DCUtR over real sockets.
//!
//! Two production runtimes across a bare relay server that has an
//! external address, as `relayed_paths.rs` builds them, both listening
//! on loopback so the address the relay's Identify observes for each
//! is the one it listens on (the TCP transport reuses the listen port
//! for its dials), which is what a hole punch needs on this machine.
//! What is proved:
//!
//! - with DCUtR on at both ends, the circuit's establishment starts an
//!   attempt at each -- the circuit's listener initiates, as the pinned
//!   crate has it -- every punch dial passes the root gate under
//!   `DcutrHolePunch` with no refusal, and the direct connection that
//!   results is announced at both ends as `PeerPathChanged { relayed
//!   -> direct, HolePunched }` and not a second `Connected`; the
//!   initiating end reports the attempt succeeded and counts it;
//! - with DCUtR off at the responding end, the initiator's attempt
//!   fails (the CONNECT stream finds no protocol), the peer enters the
//!   cooldown, and the next circuit from the same peer is declined for
//!   it -- while the end with DCUtR off reports nothing, and the path
//!   stays relayed at both (the negative, and the cooldown on the wire).
//!
//! What is NOT proved here: a punch that FAILS at the network -- on
//! loopback every punch succeeds, so the retry ceiling and the
//! attempt horizon are the wrapper's unit tests' (SPIKE-004: "hole-
//! punch FAILURE is not observed"); the concurrency ceilings on the
//! wire (unit-tested); the stability interval before a punched path
//! counts as preferred (step 9); and any NAT.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use futures::StreamExt as _;
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::runtime::dcutr_driver::DcutrSettings;
use interweave_transport_libp2p::runtime::relay_driver::{RelayClientSettings, StaticRelay};
use interweave_transport_libp2p::{
    HolePunchOutcome, PathChange, PeerPath, RelayReservationOutcome, SubstrateConfig, SwarmEvent,
    SwarmRuntime,
};
use interweave_transport_runtime::relay::ReservationConfig;
use interweave_transport_runtime::{DialOrigin, TrustSources};
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent as Libp2pSwarmEvent};
use libp2p::{Multiaddr, PeerId, identify, identity, relay};

const PATIENCE: Duration = Duration::from_secs(20);

/// How long a negative is watched for before it counts.
const WINDOW: Duration = Duration::from_secs(3);

/// Past the Swarm's dial timeout, so the initiator's stalled punch dial
/// has failed and whatever the crate does about it has happened.
const STALLED_DIAL_TAIL: Duration = Duration::from_secs(15);

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

/// Wait for one event from either runtime matching `pred`, driving the
/// relay meanwhile, or collect everything for `window` when `pred` is
/// `None`. Returns the match and everything seen before it.
async fn drive<F>(
    wire: &mut Wire<'_>,
    what: &str,
    window: Duration,
    mut pred: Option<F>,
) -> (Option<(Side, SwarmEvent)>, Vec<(Side, SwarmEvent)>)
where
    F: FnMut(Side, &SwarmEvent) -> bool,
{
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            assert!(pred.is_none(), "timed out waiting for {what}: {events:?}");
            return (None, events);
        }
        tokio::select! {
            event = wire.target.next_event() => {
                let event = event.expect("the target is alive");
                if pred.as_mut().is_some_and(|p| p(Side::Target, &event)) {
                    return (Some((Side::Target, event)), events);
                }
                events.push((Side::Target, event));
            }
            event = wire.dialer.next_event() => {
                let event = event.expect("the dialer is alive");
                if pred.as_mut().is_some_and(|p| p(Side::Dialer, &event)) {
                    return (Some((Side::Dialer, event)), events);
                }
                events.push((Side::Dialer, event));
            }
            event = wire.relay.select_next_some() => note_relay(wire.seen, event),
            () = tokio::time::sleep(remaining) => {
                assert!(pred.is_none(), "timed out waiting for {what}: {events:?}");
                return (None, events);
            }
        }
    }
}

async fn until<F>(wire: &mut Wire<'_>, what: &str, pred: F) -> Vec<(Side, SwarmEvent)>
where
    F: FnMut(Side, &SwarmEvent) -> bool,
{
    let (hit, mut before) = drive(wire, what, PATIENCE, Some(pred)).await;
    before.push(hit.expect("returned only on a match"));
    before
}

async fn settle(wire: &mut Wire<'_>, window: Duration) -> Vec<(Side, SwarmEvent)> {
    drive::<fn(Side, &SwarmEvent) -> bool>(wire, "settling", window, None)
        .await
        .1
}

/// Every `Connected` for `peer` on `side`, with its path.
fn connected(events: &[(Side, SwarmEvent)], side: Side, peer: &TransportIdentity) -> Vec<PeerPath> {
    events
        .iter()
        .filter_map(|(s, e)| match e {
            SwarmEvent::Connected { peer: p, path } if *s == side && p == peer => Some(*path),
            _ => None,
        })
        .collect()
}

fn path_changes(
    events: &[(Side, SwarmEvent)],
    side: Side,
    peer: &TransportIdentity,
) -> Vec<(PeerPath, PeerPath, PathChange)> {
    events
        .iter()
        .filter_map(|(s, e)| match e {
            SwarmEvent::PeerPathChanged {
                peer: p,
                previous,
                current,
                reason,
            } if *s == side && p == peer => Some((*previous, *current, *reason)),
            _ => None,
        })
        .collect()
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

/// Every hole-punch outcome for `peer` on `side`.
fn punches(
    events: &[(Side, SwarmEvent)],
    side: Side,
    peer: &TransportIdentity,
) -> Vec<HolePunchOutcome> {
    events
        .iter()
        .filter_map(|(s, e)| match e {
            SwarmEvent::HolePunch { peer: p, outcome } if *s == side && p == peer => {
                Some(outcome.clone())
            }
            _ => None,
        })
        .collect()
}

/// The dialer: the circuit transport with no reservation, listening on
/// loopback so its observed address is dialable, DCUtR as given.
fn dialer_config(dcutr: Option<DcutrSettings>) -> SubstrateConfig {
    SubstrateConfig {
        relay_client: Some(client_without_relays()),
        dcutr,
        ..SubstrateConfig::default()
    }
}

async fn listening(runtime: &SwarmRuntime) -> Multiaddr {
    runtime
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("a loopback address"))
        .await
        .expect("listens")
}

#[tokio::test]
async fn a_relayed_peer_is_upgraded_by_a_hole_punch_at_both_ends() {
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
            dcutr: Some(DcutrSettings::default()),
            ..SubstrateConfig::default()
        },
        |relay_peer| trust(&[&dialer_peer], &[relay_peer]),
    )
    .await;
    // BOTH LISTEN, so each is dialable at the address the relay observes.
    let _target_direct = listening(&target).await;
    let mut seen = Seen::default();
    let mut dialer = SwarmRuntime::start(
        &dialer_id,
        dialer_config(Some(DcutrSettings::default())),
        trust(&[&target_peer], &[&relay_peer]),
    )
    .expect("the dialer starts");
    let _dialer_direct = listening(&dialer).await;
    let mut wire = Wire {
        target: &mut target,
        dialer: &mut dialer,
        relay: &mut relay,
        seen: &mut seen,
    };

    // THE CIRCUIT, then the punch: the path moves to direct at both
    // ends, named the punch.
    wire.dialer
        .dial(target_peer.clone(), circuit)
        .await
        .expect("the command reaches the task")
        .expect("a circuit to a data-plane peer is admitted");
    let mut events = until(&mut wire, "the target's path to move to direct", |s, e| {
        s == Side::Target
            && matches!(e, SwarmEvent::PeerPathChanged { peer, current: PeerPath::Direct, .. } if *peer == dialer_peer)
    })
    .await;
    if path_changes(&events, Side::Dialer, &target_peer).is_empty() {
        events.extend(
            until(&mut wire, "the dialer's path to move to direct", |s, e| {
                s == Side::Dialer
                    && matches!(e, SwarmEvent::PeerPathChanged { peer, current: PeerPath::Direct, .. } if *peer == target_peer)
            })
            .await,
        );
    }
    events.extend(settle(&mut wire, WINDOW).await);
    // AND THE TAIL: the initiator's own role-overridden dial landed on
    // the responder's listener and stalls until the Swarm's dial
    // timeout. Its failure must NOT restart the crate's CONNECT round
    // -- the attempt ended when the responder's dial landed -- so past
    // that timeout there is exactly the one failure at the initiator,
    // none at the responder (its dial landed), and no further punch
    // dial. MEASURED by deleting the wrapper's filter: the crate
    // retries to its ceiling and each retry stalls the same way, so
    // the initiator's failures go past one.
    events.extend(settle(&mut wire, STALLED_DIAL_TAIL).await);
    let failures = |side: Side, peer: &TransportIdentity| {
        events
            .iter()
            .filter(|(s, e)| {
                *s == side && matches!(e, SwarmEvent::DialFailed { peer: p, .. } if p.as_ref() == Some(peer))
            })
            .count()
    };
    assert_eq!(
        failures(Side::Target, &dialer_peer),
        1,
        "the initiator's stalled dial failed once and was not retried: {events:?}"
    );
    assert_eq!(
        failures(Side::Dialer, &target_peer),
        0,
        "the responder's dial landed: {events:?}"
    );

    let punched = (PeerPath::Relayed, PeerPath::Direct, PathChange::HolePunched);
    assert_eq!(
        path_changes(&events, Side::Target, &dialer_peer),
        vec![punched],
        "the target's path moved once, by the punch: {events:?}"
    );
    assert_eq!(
        path_changes(&events, Side::Dialer, &target_peer),
        vec![punched],
        "the dialer's path moved once, by the punch: {events:?}"
    );
    assert_eq!(
        connected(&events, Side::Target, &dialer_peer),
        vec![PeerPath::Relayed],
        "one Connected at the target, and it was the circuit's"
    );
    assert_eq!(
        connected(&events, Side::Dialer, &target_peer),
        vec![PeerPath::Relayed],
        "one Connected at the dialer, and it was the circuit's"
    );
    // THE INITIATOR -- the circuit's listener -- started and finished
    // its attempt; the responder started one too (its handler awaited
    // the CONNECT) and its outcome is whatever the crate told it.
    let at_target = punches(&events, Side::Target, &dialer_peer);
    assert_eq!(
        at_target,
        vec![HolePunchOutcome::Started, HolePunchOutcome::Succeeded],
        "the initiator's attempt: {events:?}"
    );
    let at_dialer = punches(&events, Side::Dialer, &target_peer);
    assert_eq!(
        at_dialer.first(),
        Some(&HolePunchOutcome::Started),
        "the responder's attempt began on the circuit: {events:?}"
    );
    // NO PUNCH DIAL WAS REFUSED AT EITHER GATE: both ends' dials were
    // admitted toward a data-plane peer under DcutrHolePunch.
    for (side, runtime) in [(Side::Target, &*wire.target), (Side::Dialer, &*wire.dialer)] {
        let refusals = runtime.dial_refusals();
        assert!(
            !refusals
                .counts()
                .keys()
                .any(|(origin, _)| *origin == Some(DialOrigin::DcutrHolePunch)),
            "{side:?} refused a punch dial: {:?}",
            refusals.counts()
        );
    }
    let counters = wire
        .target
        .dcutr_counters()
        .expect("the target hole punches");
    assert_eq!(counters.attempts_ended.get("succeeded"), Some(&1));
    assert_eq!(counters.inflight, 0);
    assert_eq!(counters.cooldown_peers, 0);
    assert_eq!(
        wire.seen.circuits,
        vec![(pid(&dialer_peer), pid(&target_peer))],
        "one circuit at the relay, and the direct path is not through it"
    );

    target.shutdown().await.expect("shutdown");
    dialer.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn a_peer_that_does_not_punch_fails_the_attempt_and_the_next_circuit_is_declined_for_cooldown()
 {
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
            dcutr: Some(DcutrSettings::default()),
            ..SubstrateConfig::default()
        },
        |relay_peer| trust(&[&dialer_peer], &[relay_peer]),
    )
    .await;
    let _target_direct = listening(&target).await;
    let mut seen = Seen::default();
    // THE DIALER PUNCHES NOTHING: no DCUtR field, so the initiator's
    // CONNECT stream finds no protocol.
    let mut dialer = SwarmRuntime::start(
        &dialer_id,
        dialer_config(None),
        trust(&[&target_peer], &[&relay_peer]),
    )
    .expect("the dialer starts");
    let _dialer_direct = listening(&dialer).await;
    let mut wire = Wire {
        target: &mut target,
        dialer: &mut dialer,
        relay: &mut relay,
        seen: &mut seen,
    };

    wire.dialer
        .dial(target_peer.clone(), circuit.clone())
        .await
        .expect("the command reaches the task")
        .expect("admitted");
    let mut events = until(&mut wire, "the target's attempt to fail", |s, e| {
        s == Side::Target
            && matches!(e, SwarmEvent::HolePunch { peer, outcome: HolePunchOutcome::Failed { .. } } if *peer == dialer_peer)
    })
    .await;
    events.extend(settle(&mut wire, WINDOW).await);
    assert!(
        matches!(
            punches(&events, Side::Target, &dialer_peer).as_slice(),
            [HolePunchOutcome::Started, HolePunchOutcome::Failed { .. }]
        ),
        "started and failed, nothing else: {events:?}"
    );
    assert!(
        punches(&events, Side::Dialer, &target_peer).is_empty(),
        "the end without DCUtR reports nothing: {events:?}"
    );
    assert!(
        path_changes(&events, Side::Target, &dialer_peer).is_empty()
            && path_changes(&events, Side::Dialer, &target_peer).is_empty(),
        "the path stays relayed at both ends: {events:?}"
    );
    let counters = wire
        .target
        .dcutr_counters()
        .expect("the target hole punches");
    assert_eq!(counters.attempts_ended.get("failed"), Some(&1));
    assert_eq!(counters.cooldown_peers, 1, "the dialer is in cooldown");

    // THE COOLDOWN ON THE WIRE: a second circuit from the same peer is
    // a relayed inbound the wrapper declines, and no attempt begins.
    wire.dialer
        .dial(target_peer.clone(), circuit)
        .await
        .expect("the command reaches the task")
        .expect("admitted again");
    let mut again = until(&mut wire, "the second circuit to be declined", |s, e| {
        s == Side::Target
            && matches!(e, SwarmEvent::HolePunch { peer, outcome: HolePunchOutcome::Declined { .. } } if *peer == dialer_peer)
    })
    .await;
    again.extend(settle(&mut wire, WINDOW).await);
    assert_eq!(
        punches(&again, Side::Target, &dialer_peer),
        vec![HolePunchOutcome::Declined {
            reason: "declined_cooldown"
        }],
        "declined for the cooldown and nothing started: {again:?}"
    );
    assert_eq!(
        wire.target
            .dcutr_counters()
            .expect("the target hole punches")
            .declined
            .get("declined_cooldown"),
        Some(&1)
    );

    target.shutdown().await.expect("shutdown");
    dialer.shutdown().await.expect("shutdown");
}
