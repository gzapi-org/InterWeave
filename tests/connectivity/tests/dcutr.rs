// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Stage 11 step 8: DCUtR over real sockets.
//!
//! Two production runtimes across a bare relay server that has an
//! external address, as `relayed_paths.rs` builds them, both listening
//! so the address the relay's Identify observes for each is the one it
//! listens on (the TCP transport reuses the listen port for its
//! dials) -- on the host's PRIVATE address where a punch is made, since
//! `DCUTR.md` section 6 refuses a loopback candidate whoever supplies it
//! and admits a private one beside a private listener (ADR-0052), and
//! on loopback where the test is about a refusal or a decline. What is
//! proved:
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
//!   stays relayed at both (the negative, and the cooldown on the wire);
//! - a LOOPBACK candidate is refused before any socket (`DCUTR.md`
//!   section 6, ADR-0052): a bare initiator that sends its loopback
//!   observed address in CONNECT gets its punch refused at this
//!   profile's pending hook as `refused_by_class { special_use }`, the
//!   peer enters the cooldown, and the bare peer's own listener sees no
//!   connection from this profile -- the substrate shows the candidate
//!   REFUSED, not a punch made, which is why the punch-made test above
//!   runs over the host's private address and not loopback;
//! - the boundary FILTERS a punch dial rather than refusing it whole
//!   (ADR-0052 rule 5): a bare initiator on the host's private address
//!   that scripts a loopback candidate beside its observed private one
//!   gets its punch MADE through the private one, the loopback one
//!   removed and counted by class -- the composed case a home-NAT node
//!   meets at a far end that refuses one of its candidates; with the
//!   filter reduced to the whole-list refusal the punch is lost;
//! - the same from the INITIATING end: the subject reserved on the relay
//!   initiates toward a bare responder that answers with a loopback
//!   candidate beside its private one; the subject's crate dial is
//!   denied and reissued, the punch is made, and the bare responder
//!   reports exactly ONE success -- the denial's own dial failure was
//!   not handed to the crate, which would have answered it with another
//!   CONNECT round and a second punch (the far end's success count is
//!   the sensor; with the swallow deleted it goes to two);
//! - the STABILITY GATE (step 9, `DCUTR.md` section 4): after the
//!   punch the relay stays the announced path for the interval, the
//!   move to direct comes only once it has held, and the redundant
//!   relayed connection is then retired -- the relay sees its circuit
//!   close, the peer stays connected; a punched direct connection the
//!   far end closes within the interval is a stability failure: no path
//!   move, the peer in cooldown, the relay never left;
//! - a candidate outside the boundary is WITHHELD from the crate, not
//!   merely counted: a subject that listens on loopback alone, observed
//!   by the relay on loopback, initiates a CONNECT that carries no
//!   address at all, and the bare responder's crate fails the inbound
//!   exchange as a protocol error (an empty list is the one violation a
//!   conforming subject can produce there); with the withholding deleted
//!   the loopback candidate is sent, the bare peer dials it and the
//!   punch completes -- the sensor is the far end's own outcome;
//! - the listeners offered to the crate follow the bound ones: one
//!   after `listen`, none after `stop_listening`;
//! - an INFRASTRUCTURE-ONLY source over a circuit starts no attempt at
//!   a destination with DCUtR on and spends no permit: the data-plane
//!   class gate hands it no DCUtR handler before the wrapper ever sees
//!   the connection (`DCUTR.md` section 2; D1 at the handler beside the
//!   gate) -- the same connection is then closed at settlement, as step
//!   7 pins, but this test is about the WRAPPER never learning of it.
//!
//! What is NOT proved here: a punch that FAILS at the network -- on one
//! host every punch succeeds, so the retry ceiling and the attempt
//! horizon are the wrapper's unit tests' (SPIKE-004: "hole-punch
//! FAILURE is not observed"); the concurrency ceilings on the
//! wire (unit-tested); the stability interval before a punched path
//! counts as preferred (step 9); and any NAT.

#![allow(clippy::expect_used, clippy::panic)]

use std::net::Ipv4Addr;
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

/// A TCP listen address on `ip`, any port.
fn any_port(ip: Ipv4Addr) -> Multiaddr {
    format!("/ip4/{ip}/tcp/0")
        .parse()
        .expect("a listen address")
}

async fn bound(swarm: &mut libp2p::Swarm<RelayBehaviour>, ip: Ipv4Addr) -> Multiaddr {
    swarm.listen_on(any_port(ip)).expect("listens");
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
    /// Circuits the relay saw close.
    closed_circuits: usize,
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
        Libp2pSwarmEvent::Behaviour(RelayBehaviourEvent::Relay(relay::Event::CircuitClosed {
            ..
        })) => seen.closed_circuits += 1,
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
        direct_head_start_ms: 750,
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
    ip: Ipv4Addr,
    target_config: impl FnOnce(RelayClientSettings) -> SubstrateConfig,
    trust: impl FnOnce(&TransportIdentity) -> TrustSources,
) -> Reserved {
    let relay_keys = identity::Keypair::generate_ed25519();
    let relay_peer = identity_of(&relay_keys);
    let mut relay = relay_server(relay_keys);
    let relay_addr = bound(&mut relay, ip).await;
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
/// Two seconds of stability, so the gate is seen inside a test's
/// patience; the profile's default is ten.
const STABILITY: Duration = Duration::from_secs(2);

/// DCUtR on, with the test's stability interval.
fn punching() -> DcutrSettings {
    DcutrSettings {
        direct_stability_period_ms: STABILITY.as_millis() as u64,
        ..DcutrSettings::default()
    }
}

fn dialer_config(dcutr: Option<DcutrSettings>) -> SubstrateConfig {
    SubstrateConfig {
        relay_client: Some(client_without_relays()),
        dcutr,
        ..SubstrateConfig::default()
    }
}

async fn listening(runtime: &SwarmRuntime, ip: Ipv4Addr) -> Multiaddr {
    runtime.listen(any_port(ip)).await.expect("listens")
}

const LOOPBACK: Ipv4Addr = Ipv4Addr::LOCALHOST;

/// A private-range (RFC 1918) address of this host, if it has one: the
/// interface the kernel would route a private destination through,
/// read off an unconnected UDP socket -- no packet is sent. On this
/// machine and on the hosted CI runners that is the machine's own
/// private address; a host with none has no LAN to punch across, and
/// the punch-made test says so and stands down, because `DCUTR.md`
/// section 6 refuses a loopback candidate and there is no test-only
/// knob to admit one (ADR-0052).
fn private_interface_v4() -> Option<Ipv4Addr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("10.255.255.255:9").ok()?;
    match socket.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(ip) if ip.is_private() && !ip.is_loopback() => Some(ip),
        _ => None,
    }
}

#[tokio::test]
async fn a_relayed_peer_is_upgraded_by_a_hole_punch_at_both_ends() {
    // OVER A PRIVATE-RANGE PAIR, not loopback: section 6 refuses a
    // loopback candidate whoever supplies it, and admits a private one
    // beside a private listener of the same family -- which is what a
    // LAN punch is, and what two runtimes on this host's private
    // address are.
    let Some(ip) = private_interface_v4() else {
        eprintln!(
            "no private-range interface on this host: the punch-made test did not run \
             (DCUTR.md section 6 refuses loopback and there is no knob to admit it)"
        );
        return;
    };
    let dialer_id = ProfileIdentity::generate();
    let dialer_peer = dialer_id.transport_identity().expect("peer id");
    let Reserved {
        mut relay,
        relay_peer,
        mut target,
        target_peer,
        circuit,
    } = reserved(
        ip,
        |client| SubstrateConfig {
            relay_client: Some(client),
            dcutr: Some(punching()),
            ..SubstrateConfig::default()
        },
        |relay_peer| trust(&[&dialer_peer], &[relay_peer]),
    )
    .await;
    // BOTH LISTEN on the private address, so each is dialable at the
    // address the relay observes and each holds the private listener
    // that admits the other's private candidate.
    let _target_direct = listening(&target, ip).await;
    let mut seen = Seen::default();
    let mut dialer = SwarmRuntime::start(
        &dialer_id,
        dialer_config(Some(punching())),
        trust(&[&target_peer], &[&relay_peer]),
    )
    .expect("the dialer starts");
    let _dialer_direct = listening(&dialer, ip).await;
    let mut wire = Wire {
        target: &mut target,
        dialer: &mut dialer,
        relay: &mut relay,
        seen: &mut seen,
    };

    // THE CIRCUIT, then the punch: the attempt succeeds at the
    // initiator, and the path moves to direct at both ends -- NOT at
    // once but after the stability interval (`DCUTR.md` section 4, step
    // 9): within it the relay stays the announced path.
    wire.dialer
        .dial(target_peer.clone(), circuit)
        .await
        .expect("the command reaches the task")
        .expect("a circuit to a data-plane peer is admitted");
    let mut events = until(&mut wire, "the initiator's attempt to succeed", |s, e| {
        s == Side::Target
            && matches!(e, SwarmEvent::HolePunch { peer, outcome: HolePunchOutcome::Succeeded } if *peer == dialer_peer)
    })
    .await;
    let succeeded_at = tokio::time::Instant::now();
    events.extend(settle(&mut wire, STABILITY / 2).await);
    assert!(
        path_changes(&events, Side::Target, &dialer_peer).is_empty()
            && path_changes(&events, Side::Dialer, &target_peer).is_empty(),
        "within the stability interval the relay stays the announced path: {events:?}"
    );
    events.extend(
        until(&mut wire, "the target's path to move to direct", |s, e| {
            s == Side::Target
                && matches!(e, SwarmEvent::PeerPathChanged { peer, current: PeerPath::Direct, .. } if *peer == dialer_peer)
        })
        .await,
    );
    let moved_at = tokio::time::Instant::now();
    assert!(
        moved_at.duration_since(succeeded_at) >= STABILITY,
        "the move waited out the interval: {:?}",
        moved_at.duration_since(succeeded_at)
    );
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
    // THE RETIREMENT (section 13's last arrow, step 9): once the stable
    // punched direct is the announced path, the redundant relayed
    // connection is closed by whichever end retires first, both report
    // it (each closes its own or sees the other's close), the relay
    // sees its circuit end, and the peer stays connected -- no
    // Disconnected, and the announced path stays direct.
    assert!(
        events.iter().any(|(s, e)| *s == Side::Target
            && matches!(e, SwarmEvent::RelayedConnectionRetired { peer } if *peer == dialer_peer)),
        "the target retired the relayed connection: {events:?}"
    );
    assert!(
        wire.seen.closed_circuits >= 1,
        "the relay saw its circuit close: {:?}",
        wire.seen.closed_circuits
    );
    assert!(
        !events
            .iter()
            .any(|(_, e)| matches!(e, SwarmEvent::Disconnected { .. })),
        "the peer stays connected over the direct path: {events:?}"
    );
    assert_eq!(
        counters.upgrades_stable, 1,
        "the upgrade held: {counters:?}"
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
        LOOPBACK,
        |client| SubstrateConfig {
            relay_client: Some(client),
            dcutr: Some(punching()),
            ..SubstrateConfig::default()
        },
        |relay_peer| trust(&[&dialer_peer], &[relay_peer]),
    )
    .await;
    let _target_direct = listening(&target, LOOPBACK).await;
    let mut seen = Seen::default();
    // THE DIALER PUNCHES NOTHING: no DCUtR field, so the initiator's
    // CONNECT stream finds no protocol.
    let mut dialer = SwarmRuntime::start(
        &dialer_id,
        dialer_config(None),
        trust(&[&target_peer], &[&relay_peer]),
    )
    .expect("the dialer starts");
    let _dialer_direct = listening(&dialer, LOOPBACK).await;
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

#[tokio::test]
async fn an_infrastructure_only_source_over_a_circuit_starts_no_attempt() {
    let dialer_id = ProfileIdentity::generate();
    let dialer_peer = dialer_id.transport_identity().expect("peer id");
    // THE TARGET holds the dialer infrastructure-only and punches.
    let Reserved {
        mut relay,
        relay_peer,
        mut target,
        target_peer,
        circuit,
    } = reserved(
        LOOPBACK,
        |client| SubstrateConfig {
            relay_client: Some(client),
            dcutr: Some(punching()),
            ..SubstrateConfig::default()
        },
        |relay_peer| trust(&[], &[relay_peer, &dialer_peer]),
    )
    .await;
    let _target_direct = listening(&target, LOOPBACK).await;
    let mut seen = Seen::default();
    let mut dialer = SwarmRuntime::start(
        &dialer_id,
        dialer_config(Some(punching())),
        trust(&[&target_peer], &[&relay_peer]),
    )
    .expect("the dialer starts");
    let _dialer_direct = listening(&dialer, LOOPBACK).await;
    let mut wire = Wire {
        target: &mut target,
        dialer: &mut dialer,
        relay: &mut relay,
        seen: &mut seen,
    };

    // The circuit is admitted at the dialer (the target is data-plane
    // there), accepted by the relay, and closed by the target at
    // settlement; what the target's WRAPPER saw of it is nothing.
    wire.dialer
        .dial(target_peer.clone(), circuit)
        .await
        .expect("the command reaches the task")
        .expect("admitted at the dialer");
    let mut events = until(&mut wire, "the dialer to see the circuit close", |s, e| {
        s == Side::Dialer && matches!(e, SwarmEvent::Disconnected { peer } if *peer == target_peer)
    })
    .await;
    events.extend(settle(&mut wire, WINDOW).await);
    assert_eq!(
        wire.seen.circuits,
        vec![(pid(&dialer_peer), pid(&target_peer))],
        "the relay accepted the circuit: the refusal is the target's"
    );
    assert!(
        punches(&events, Side::Target, &dialer_peer).is_empty(),
        "no attempt, no decline: the wrapper never saw the connection: {events:?}"
    );
    let counters = wire
        .target
        .dcutr_counters()
        .expect("the target hole punches");
    assert_eq!(counters.inflight, 0);
    assert!(counters.declined.is_empty(), "{counters:?}");
    assert!(counters.attempts_ended.is_empty(), "{counters:?}");
    // THE CONTROL, in the same shape: the dialer, which holds the
    // target data-plane trusted, started an attempt on its outbound
    // relayed connection and abandoned it when the target closed.
    assert_eq!(
        punches(&events, Side::Dialer, &target_peer),
        vec![HolePunchOutcome::Started, HolePunchOutcome::Abandoned],
        "{events:?}"
    );

    target.shutdown().await.expect("shutdown");
    dialer.shutdown().await.expect("shutdown");
}

/// A bare peer that punches with the crate as it comes: Identify, the
/// relay client and DCUtR, no boundary of any kind -- so what it
/// sends in CONNECT is whatever a peer's Identify observed, loopback
/// included.
#[derive(NetworkBehaviour)]
struct BareInitiator {
    identify: identify::Behaviour,
    relay: relay::client::Behaviour,
    dcutr: libp2p::dcutr::Behaviour,
    /// Candidates a test scripts into the bare peer's set, beside what
    /// its Identify observed: a second address in its CONNECT.
    extra: ScriptedCandidates,
}

/// A behaviour that announces whatever candidates it is handed, once
/// each, so a bare peer's CONNECT can carry an address nobody observed
/// it on.
#[derive(Default)]
struct ScriptedCandidates {
    queued: std::collections::VecDeque<Multiaddr>,
}

impl NetworkBehaviour for ScriptedCandidates {
    type ConnectionHandler = libp2p::swarm::dummy::ConnectionHandler;
    type ToSwarm = ();

    fn handle_established_inbound_connection(
        &mut self,
        _: libp2p::swarm::ConnectionId,
        _: PeerId,
        _: &Multiaddr,
        _: &Multiaddr,
    ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
        Ok(libp2p::swarm::dummy::ConnectionHandler)
    }

    fn handle_established_outbound_connection(
        &mut self,
        _: libp2p::swarm::ConnectionId,
        _: PeerId,
        _: &Multiaddr,
        _: libp2p::core::Endpoint,
        _: libp2p::core::transport::PortUse,
    ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
        Ok(libp2p::swarm::dummy::ConnectionHandler)
    }

    fn on_swarm_event(&mut self, _: libp2p::swarm::FromSwarm<'_>) {}

    fn on_connection_handler_event(
        &mut self,
        _: PeerId,
        _: libp2p::swarm::ConnectionId,
        _: libp2p::swarm::THandlerOutEvent<Self>,
    ) {
    }

    fn poll(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<libp2p::swarm::ToSwarm<Self::ToSwarm, libp2p::swarm::THandlerInEvent<Self>>>
    {
        self.queued
            .pop_front()
            .map_or(std::task::Poll::Pending, |addr| {
                std::task::Poll::Ready(libp2p::swarm::ToSwarm::NewExternalAddrCandidate(addr))
            })
    }
}

fn bare_initiator(keys: identity::Keypair) -> libp2p::Swarm<BareInitiator> {
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
        .with_behaviour(|k, relay| BareInitiator {
            identify: identify::Behaviour::new(identify::Config::new(
                "/interweave-dcutr-test-bare/1".to_owned(),
                k.public(),
            )),
            relay,
            dcutr: libp2p::dcutr::Behaviour::new(k.public().to_peer_id()),
            extra: ScriptedCandidates::default(),
        })
        .expect("behaviour")
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(600)))
        .build()
}

#[tokio::test]
async fn a_loopback_candidate_is_refused_before_any_socket() {
    // THE SUBJECT NEEDS ONE CANDIDATE OF ITS OWN inside the boundary,
    // or the exchange never reaches the dial: the crate's protocol
    // refuses an empty candidate list on either side, and on loopback
    // alone this profile has none to send (section 6 keeps loopback
    // out). Its listener on the host's private address is that
    // candidate; without one the test stands down, as the punch-made
    // test does.
    let Some(ip) = private_interface_v4() else {
        eprintln!(
            "no private-range interface on this host: the loopback-refused test did not run \
             (the subject would have no candidate of its own to send)"
        );
        return;
    };
    // THE RELAY on loopback, external address added; THE BARE
    // INITIATOR listens on loopback, reserves on the relay, and so is
    // observed by the relay's Identify on loopback -- the candidate it
    // will send.
    let relay_keys = identity::Keypair::generate_ed25519();
    let relay_peer = identity_of(&relay_keys);
    let mut relay = relay_server(relay_keys);
    let relay_addr = bound(&mut relay, LOOPBACK).await;
    relay.add_external_address(relay_addr.clone());
    let mut seen = Seen::default();

    let bare_keys = identity::Keypair::generate_ed25519();
    let bare_peer = identity_of(&bare_keys);
    let mut bare = bare_initiator(bare_keys);
    bare.listen_on(any_port(LOOPBACK)).expect("listens");
    bare.listen_on(
        relay_addr
            .clone()
            .with(libp2p::multiaddr::Protocol::P2p(pid(&relay_peer)))
            .with(libp2p::multiaddr::Protocol::P2pCircuit),
    )
    .expect("a circuit listen is accepted");
    let circuit = circuit_of(&relay_addr, &relay_peer, &bare_peer);

    // THE SUBJECT: the production runtime as the circuit's dialer -- the
    // RESPONDER, which dials whatever the initiator's CONNECT names.
    let subject_id = ProfileIdentity::generate();
    let subject_peer = subject_id.transport_identity().expect("peer id");
    let subject_pid = pid(&subject_peer);
    let mut subject = SwarmRuntime::start(
        &subject_id,
        dialer_config(Some(punching())),
        trust(&[&bare_peer], &[&relay_peer]),
    )
    .expect("the subject starts");
    let _subject_direct = listening(&subject, ip).await;

    // Drive the bare peer and the relay until the bare peer's
    // reservation is accepted, then dial the circuit.
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut reserved_on_relay = false;
    let mut bare_direct_inbounds = 0_usize;
    while !reserved_on_relay {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "the bare peer's reservation was never accepted"
        );
        tokio::select! {
            event = bare.select_next_some() => {
                if let Libp2pSwarmEvent::Behaviour(BareInitiatorEvent::Relay(
                    relay::client::Event::ReservationReqAccepted { .. },
                )) = event
                {
                    reserved_on_relay = true;
                }
            }
            event = relay.select_next_some() => note_relay(&mut seen, event),
            () = tokio::time::sleep(remaining) => {}
        }
    }
    subject
        .dial(bare_peer.clone(), circuit)
        .await
        .expect("the command reaches the task")
        .expect("a circuit to a data-plane peer is admitted");

    // The subject's attempt begins on its outbound relayed connection,
    // the bare initiator sends its loopback candidate, and the punch
    // dial is refused at the hook naming the class.
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut events = Vec::new();
    let refused = loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "the refusal never came: {events:?}");
        tokio::select! {
            event = subject.next_event() => {
                let event = event.expect("the subject is alive");
                let hit = matches!(
                    &event,
                    SwarmEvent::HolePunch { peer, outcome: HolePunchOutcome::RefusedByClass { .. } }
                        if *peer == bare_peer
                );
                events.push(event);
                if hit {
                    break events.last().cloned();
                }
            }
            event = bare.select_next_some() => {
                if let Libp2pSwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } = &event
                    && *peer_id == subject_pid
                    && matches!(endpoint, libp2p::core::ConnectedPoint::Listener { .. })
                    && !endpoint.is_relayed()
                {
                    bare_direct_inbounds += 1;
                }
            }
            event = relay.select_next_some() => note_relay(&mut seen, event),
            () = tokio::time::sleep(remaining) => {}
        }
    };
    assert_eq!(
        refused,
        Some(SwarmEvent::HolePunch {
            peer: bare_peer.clone(),
            outcome: HolePunchOutcome::RefusedByClass {
                class: "special_use"
            }
        })
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            SwarmEvent::HolePunch { peer, outcome: HolePunchOutcome::Started } if *peer == bare_peer
        )),
        "the attempt had begun: {events:?}"
    );
    // AND NO SOCKET: the bare peer's loopback listener saw no direct
    // connection from the subject, and the subject's gate refused no
    // punch dial -- the hook after it did, before any socket, and the
    // gate took its ticket back.
    let settle_until = tokio::time::Instant::now() + WINDOW;
    loop {
        let remaining = settle_until.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        tokio::select! {
            _ = subject.next_event() => {}
            event = bare.select_next_some() => {
                if let Libp2pSwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } = &event
                    && *peer_id == subject_pid
                    && matches!(endpoint, libp2p::core::ConnectedPoint::Listener { .. })
                    && !endpoint.is_relayed()
                {
                    bare_direct_inbounds += 1;
                }
            }
            event = relay.select_next_some() => note_relay(&mut seen, event),
            () = tokio::time::sleep(remaining) => {}
        }
    }
    assert_eq!(
        bare_direct_inbounds, 0,
        "no punch dial reached the bare peer"
    );
    let counters = subject.dcutr_counters().expect("the subject hole punches");
    assert_eq!(counters.attempts_ended.get("refused_by_class"), Some(&1));
    assert_eq!(counters.cooldown_peers, 1, "the bare peer is in cooldown");
    assert_eq!(
        subject.dial_refusals().released_after_admission(),
        1,
        "the gate admitted the punch dial and took its ticket back when the hook refused it"
    );

    subject.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn a_loopback_only_subject_sends_no_candidate_at_all() {
    // THE SUBJECT reserves on the loopback relay and listens on
    // loopback only, so everything a peer observes it on is loopback;
    // the BARE RESPONDER dials the circuit and answers the subject's
    // CONNECT with the crate as it comes. What the subject's CONNECT
    // carried is read off the bare peer's outcome: a CONNECT with no
    // address is a protocol violation its crate reports by name.
    let Reserved {
        mut relay,
        relay_peer,
        target: mut subject,
        target_peer: subject_peer,
        circuit,
    } = reserved(
        LOOPBACK,
        |client| SubstrateConfig {
            relay_client: Some(client),
            dcutr: Some(punching()),
            ..SubstrateConfig::default()
        },
        |relay_peer| trust(&[], &[relay_peer]),
    )
    .await;
    let _subject_direct = listening(&subject, LOOPBACK).await;
    let mut seen = Seen::default();

    let bare_keys = identity::Keypair::generate_ed25519();
    let bare_peer = identity_of(&bare_keys);
    let mut bare = bare_initiator(bare_keys);
    bare.listen_on(any_port(LOOPBACK)).expect("listens");
    // The subject trusts the bare peer for the data plane once it is
    // known; the relay stays infrastructure.
    subject
        .set_trust(trust(&[&bare_peer], &[&relay_peer]))
        .await
        .expect("trust installs");
    bare.dial(circuit)
        .expect("a circuit dial is accepted by the transport");

    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut subject_events = Vec::new();
    let outcome = loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "the bare peer reported no outcome: {subject_events:?}"
        );
        tokio::select! {
            event = subject.next_event() => {
                subject_events.push(event.expect("the subject is alive"));
            }
            event = bare.select_next_some() => {
                if let Libp2pSwarmEvent::Behaviour(BareInitiatorEvent::Dcutr(
                    libp2p::dcutr::Event { remote_peer_id, result },
                )) = event
                    && remote_peer_id == pid(&subject_peer)
                {
                    break result.map(|_| ()).map_err(|e| e.to_string());
                }
            }
            event = relay.select_next_some() => note_relay(&mut seen, event),
            () = tokio::time::sleep(remaining) => {}
        }
    };
    // The crate's public error flattens the violation to "Protocol
    // error" (its Display names neither the variant nor a source), and
    // an empty candidate list is the only violation a conforming
    // subject can produce on the inbound side -- the others are codec
    // and message-type errors. With the withholding deleted the subject
    // sends its loopback candidate, the bare peer dials it, and this
    // outcome is `Ok`.
    let detail = outcome.expect_err("the bare peer's punch cannot complete without a candidate");
    assert_eq!(
        detail, "Failed to hole-punch connection: Inbound stream error: Protocol error",
        "the subject's CONNECT carried no address"
    );
    let counters = subject.dcutr_counters().expect("the subject hole punches");
    assert!(
        counters
            .candidates_withheld
            .get("special_use")
            .is_some_and(|n| *n >= 1),
        "and the subject counted what it withheld: {counters:?}"
    );

    subject.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn the_listeners_offered_to_the_crate_follow_the_bound_ones() {
    let Some(ip) = private_interface_v4() else {
        eprintln!("no private-range interface on this host: a loopback listener is never offered");
        return;
    };
    let id = ProfileIdentity::generate();
    let runtime =
        SwarmRuntime::start(&id, dialer_config(Some(punching())), trust(&[], &[])).expect("starts");
    let offered = |runtime: &SwarmRuntime| {
        runtime
            .dcutr_counters()
            .expect("hole punches")
            .listeners_offered
    };
    assert_eq!(offered(&runtime), 0);
    let address = listening(&runtime, ip).await;
    assert_eq!(offered(&runtime), 1, "bound, offered");
    assert!(
        runtime
            .stop_listening(address)
            .await
            .expect("the command reaches the task"),
        "the listener was active"
    );
    assert_eq!(offered(&runtime), 0, "closed, forgotten");
    let _again = listening(&runtime, ip).await;
    assert_eq!(offered(&runtime), 1, "bound again, offered again");
    runtime.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn a_punch_dial_is_filtered_rather_than_refused_whole() {
    let Some(ip) = private_interface_v4() else {
        eprintln!("no private-range interface on this host: the filtered-punch test did not run");
        return;
    };
    // THE RELAY on the private address; THE BARE INITIATOR listens
    // there too (so the relay observes it there -- its admitted
    // candidate) and scripts a loopback candidate beside it.
    let relay_keys = identity::Keypair::generate_ed25519();
    let relay_peer = identity_of(&relay_keys);
    let mut relay = relay_server(relay_keys);
    let relay_addr = bound(&mut relay, ip).await;
    relay.add_external_address(relay_addr.clone());
    let mut seen = Seen::default();

    let bare_keys = identity::Keypair::generate_ed25519();
    let bare_peer = identity_of(&bare_keys);
    let mut bare = bare_initiator(bare_keys);
    bare.listen_on(any_port(ip)).expect("listens");
    bare.behaviour_mut()
        .extra
        .queued
        .push_back("/ip4/127.0.0.1/tcp/4001".parse().expect("an address"));
    bare.listen_on(
        relay_addr
            .clone()
            .with(libp2p::multiaddr::Protocol::P2p(pid(&relay_peer)))
            .with(libp2p::multiaddr::Protocol::P2pCircuit),
    )
    .expect("a circuit listen is accepted");
    let circuit = circuit_of(&relay_addr, &relay_peer, &bare_peer);

    // THE SUBJECT, the responder, listening on the private address: a
    // private candidate is admitted beside its private listener, the
    // loopback one is not.
    let subject_id = ProfileIdentity::generate();
    let mut subject = SwarmRuntime::start(
        &subject_id,
        dialer_config(Some(punching())),
        trust(&[&bare_peer], &[&relay_peer]),
    )
    .expect("the subject starts");
    let _subject_direct = listening(&subject, ip).await;

    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut reserved_on_relay = false;
    while !reserved_on_relay {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "the bare peer's reservation was never accepted"
        );
        tokio::select! {
            event = bare.select_next_some() => {
                if let Libp2pSwarmEvent::Behaviour(BareInitiatorEvent::Relay(
                    relay::client::Event::ReservationReqAccepted { .. },
                )) = event
                {
                    reserved_on_relay = true;
                }
            }
            event = relay.select_next_some() => note_relay(&mut seen, event),
            () = tokio::time::sleep(remaining) => {}
        }
    }
    subject
        .dial(bare_peer.clone(), circuit)
        .await
        .expect("the command reaches the task")
        .expect("admitted");

    // THE PUNCH IS MADE through the admitted candidate: the subject's
    // path to the bare peer moves to direct, the punch, and the
    // loopback candidate was removed and counted -- never refused the
    // attempt.
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut events = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "the punch was never made: {events:?}");
        tokio::select! {
            event = subject.next_event() => {
                let event = event.expect("the subject is alive");
                let hit = matches!(
                    &event,
                    SwarmEvent::PeerPathChanged { peer, current: PeerPath::Direct, .. } if *peer == bare_peer
                );
                events.push(event);
                if hit {
                    break;
                }
            }
            _ = bare.select_next_some() => {}
            event = relay.select_next_some() => note_relay(&mut seen, event),
            () = tokio::time::sleep(remaining) => {}
        }
    }
    assert_eq!(
        path_changes(
            &events
                .iter()
                .map(|e| (Side::Dialer, e.clone()))
                .collect::<Vec<_>>(),
            Side::Dialer,
            &bare_peer
        ),
        vec![(PeerPath::Relayed, PeerPath::Direct, PathChange::HolePunched)]
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            SwarmEvent::HolePunch {
                outcome: HolePunchOutcome::RefusedByClass { .. },
                ..
            }
        )),
        "nothing was refused whole: {events:?}"
    );
    let counters = subject.dcutr_counters().expect("the subject hole punches");
    assert_eq!(
        counters.candidates_removed.get("special_use"),
        Some(&1),
        "the loopback candidate was removed and counted: {counters:?}"
    );
    assert_eq!(counters.backstop_refusals, 0);
    assert_eq!(counters.attempts_ended.get("succeeded"), Some(&1));
    // AND THE GATE WROTE NOTHING DOWN for the denied crate dial: the
    // reissue's cause reached it un-rewrapped, and a filtered punch is
    // not a refusal in its ring.
    let refusals = subject.dial_refusals();
    assert_eq!(
        refusals.released_after_admission(),
        0,
        "the denied crate dial was taken back silently: {:?}",
        refusals.counts()
    );

    subject.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn a_filtered_punch_from_the_initiating_end_opens_one_connect_round() {
    let Some(ip) = private_interface_v4() else {
        eprintln!(
            "no private-range interface on this host: the initiating-end filter test did not run"
        );
        return;
    };
    // THE SUBJECT reserves on the private relay and listens there: the
    // circuit's listener, the initiator.
    let bare_keys = identity::Keypair::generate_ed25519();
    let bare_peer = identity_of(&bare_keys);
    let Reserved {
        mut relay,
        relay_peer: _,
        target: mut subject,
        target_peer: subject_peer,
        circuit,
    } = reserved(
        ip,
        |client| SubstrateConfig {
            relay_client: Some(client),
            dcutr: Some(punching()),
            ..SubstrateConfig::default()
        },
        |relay_peer| trust(&[&bare_peer], &[relay_peer]),
    )
    .await;
    let _subject_direct = listening(&subject, ip).await;
    let mut seen = Seen::default();
    // THE BARE RESPONDER listens on the private address and scripts a
    // loopback candidate beside the one the relay observes.
    let mut bare = bare_initiator(bare_keys);
    bare.listen_on(any_port(ip)).expect("listens");
    bare.behaviour_mut()
        .extra
        .queued
        .push_back("/ip4/127.0.0.1/tcp/4001".parse().expect("an address"));
    bare.dial(circuit).expect("a circuit dial is accepted");

    // The subject's punch is made (its path moves to direct), and the
    // bare responder reports its success. Then a window: no second.
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut events = Vec::new();
    let mut bare_successes = 0_usize;
    let mut punched = false;
    while !(punched && bare_successes >= 1) {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "the punch was never made at both ends: {events:?} bare successes {bare_successes}"
        );
        tokio::select! {
            event = subject.next_event() => {
                let event = event.expect("the subject is alive");
                punched |= matches!(
                    &event,
                    SwarmEvent::PeerPathChanged { peer, current: PeerPath::Direct, .. } if *peer == bare_peer
                );
                events.push(event);
            }
            event = bare.select_next_some() => {
                if let Libp2pSwarmEvent::Behaviour(BareInitiatorEvent::Dcutr(
                    libp2p::dcutr::Event { remote_peer_id, result: Ok(_) },
                )) = &event
                    && *remote_peer_id == pid(&subject_peer)
                {
                    bare_successes += 1;
                }
            }
            event = relay.select_next_some() => note_relay(&mut seen, event),
            () = tokio::time::sleep(remaining) => {}
        }
    }
    let settle_until = tokio::time::Instant::now() + WINDOW;
    loop {
        let remaining = settle_until.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        tokio::select! {
            event = subject.next_event() => { events.push(event.expect("alive")); }
            event = bare.select_next_some() => {
                if let Libp2pSwarmEvent::Behaviour(BareInitiatorEvent::Dcutr(
                    libp2p::dcutr::Event { remote_peer_id, result: Ok(_) },
                )) = &event
                    && *remote_peer_id == pid(&subject_peer)
                {
                    bare_successes += 1;
                }
            }
            event = relay.select_next_some() => note_relay(&mut seen, event),
            () = tokio::time::sleep(remaining) => {}
        }
    }
    assert_eq!(
        bare_successes, 1,
        "one CONNECT round, one punch: the denied dial's failure reached no crate: {events:?}"
    );
    let counters = subject.dcutr_counters().expect("the subject hole punches");
    assert_eq!(counters.candidates_removed.get("special_use"), Some(&1));
    assert_eq!(counters.attempts_ended.get("succeeded"), Some(&1));

    subject.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn a_punched_connection_that_dies_within_the_interval_leaves_the_relay_preferred() {
    let Some(ip) = private_interface_v4() else {
        eprintln!(
            "no private-range interface on this host: the stability-failure test did not run"
        );
        return;
    };
    let bare_keys = identity::Keypair::generate_ed25519();
    let bare_peer = identity_of(&bare_keys);
    let Reserved {
        mut relay,
        relay_peer: _,
        target: mut subject,
        target_peer: subject_peer,
        circuit,
    } = reserved(
        ip,
        |client| SubstrateConfig {
            relay_client: Some(client),
            dcutr: Some(punching()),
            ..SubstrateConfig::default()
        },
        |relay_peer| trust(&[&bare_peer], &[relay_peer]),
    )
    .await;
    let _subject_direct = listening(&subject, ip).await;
    let mut seen = Seen::default();
    let mut bare = bare_initiator(bare_keys);
    bare.listen_on(any_port(ip)).expect("listens");
    bare.dial(circuit).expect("a circuit dial is accepted");

    // The bare responder's punch dial lands; the crate hands it the
    // connection's id, and once the subject's Identify has answered on
    // it -- the subject has established and retained it, so a close
    // now is a close of a PUNCHED connection and not of one the subject
    // never had (closed at once, it raced the subject's establishment
    // and the subject saw nothing, measured) -- the bare peer CLOSES it,
    // inside the interval.
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut events = Vec::new();
    let mut punched: Option<libp2p::swarm::ConnectionId> = None;
    let mut closed_early = false;
    while !closed_early {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "the punch was never made: {events:?}");
        tokio::select! {
            event = subject.next_event() => { events.push(event.expect("alive")); }
            event = bare.select_next_some() => match &event {
                Libp2pSwarmEvent::Behaviour(BareInitiatorEvent::Dcutr(
                    libp2p::dcutr::Event { remote_peer_id, result: Ok(direct) },
                )) if *remote_peer_id == pid(&subject_peer) => {
                    punched = Some(*direct);
                }
                Libp2pSwarmEvent::Behaviour(BareInitiatorEvent::Identify(
                    identify::Event::Received { connection_id, .. },
                )) if Some(*connection_id) == punched => {
                    assert!(bare.close_connection(*connection_id), "the punched connection is known");
                    closed_early = true;
                }
                _ => {}
            },
            event = relay.select_next_some() => note_relay(&mut seen, event),
            () = tokio::time::sleep(remaining) => {}
        }
    }
    // The subject: the attempt succeeded, then the punched connection
    // closed within the interval -- Unstable, the peer in cooldown, no
    // path move at all, the relayed connection kept.
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "no Unstable came: {events:?}");
        tokio::select! {
            event = subject.next_event() => {
                let event = event.expect("alive");
                let hit = matches!(&event, SwarmEvent::HolePunch { peer, outcome: HolePunchOutcome::Unstable } if *peer == bare_peer);
                events.push(event);
                if hit { break; }
            }
            _ = bare.select_next_some() => {}
            event = relay.select_next_some() => note_relay(&mut seen, event),
            () = tokio::time::sleep(remaining) => {}
        }
    }
    // Past the interval, still nothing moved.
    let settle_until = tokio::time::Instant::now() + STABILITY + WINDOW;
    loop {
        let remaining = settle_until.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        tokio::select! {
            event = subject.next_event() => { events.push(event.expect("alive")); }
            _ = bare.select_next_some() => {}
            event = relay.select_next_some() => note_relay(&mut seen, event),
            () = tokio::time::sleep(remaining) => {}
        }
    }
    let as_side: Vec<(Side, SwarmEvent)> =
        events.iter().map(|e| (Side::Target, e.clone())).collect();
    assert!(
        path_changes(&as_side, Side::Target, &bare_peer).is_empty(),
        "the relay was never left: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SwarmEvent::Disconnected { peer } if *peer == bare_peer)),
        "the relayed connection kept the peer connected: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SwarmEvent::RelayedConnectionRetired { .. })),
        "nothing was retired: {events:?}"
    );
    let counters = subject.dcutr_counters().expect("the subject hole punches");
    assert_eq!(counters.stability_failures, 1, "{counters:?}");
    assert_eq!(counters.upgrades_stable, 0);
    assert_eq!(counters.cooldown_peers, 1, "the peer is in cooldown");
    assert_eq!(
        seen.closed_circuits, 0,
        "the circuit at the relay is still up"
    );

    subject.shutdown().await.expect("shutdown");
}
