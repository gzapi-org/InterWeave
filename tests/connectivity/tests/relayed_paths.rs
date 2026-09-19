// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Stage 11 step 7: relayed peer paths over real sockets.
//!
//! Two production runtimes across a bare relay server that has an
//! external address: a TARGET that reserves on the relay, and a DIALER
//! that reaches it through the circuit. What is proved:
//!
//! - a `Dial` of a `/p2p-circuit` address is judged as an APPLICATION
//!   destination: toward an infrastructure-only far end it is refused
//!   at the gate before any socket -- the relay sees no connection
//!   from the dialer at all -- with the denial naming the data-plane
//!   rule; toward a data-plane peer it is admitted (the control). That
//!   the origin is `RelayCircuit` rather than `Manual` is not
//!   distinguishable here, since the gate applies the same rule to
//!   both; `a_circuit_address_from_a_command_is_a_relay_circuit_dial`
//!   pins the origin;
//! - the circuit completes at both ends as ONE `Connected` per peer
//!   carrying `PeerPath::Relayed` (`contracts/CONNECTIVITY.md` §5), the
//!   relay having accepted the circuit request from the dialer to the
//!   target;
//! - the data plane rides it: a direct v2 message from the dialer is
//!   accepted by the target over the circuit and drained from the
//!   endpoint it named;
//! - a direct connection joining the relayed one is a `PeerPathChanged`
//!   from relayed to direct at both ends and not a second `Connected`;
//! - an infrastructure-only SOURCE arriving over a circuit is refused
//!   at the destination -- established and closed, never announced --
//!   even when the destination serves probes and circuits, which is
//!   when the inbound arm would otherwise ask under an infrastructure
//!   origin; the same source given data-plane trust is retained (the
//!   control, with the same servers on), redialled from the address
//!   book, where the circuit route the first dial worked over was
//!   learned, so `DialPeer` reaches a peer over a remembered circuit.
//!
//! - a circuit route the address book holds is retried by the SCHEDULER
//!   as a relay circuit dial: a circuit the relay denies (the
//!   destination never reserved) is a transient failure, and the retry
//!   reaches the relay again rather than being refused at the gate's
//!   pairing check under the scheduler's own origin and scrubbed from
//!   the book as a structural failure.
//!
//! What is NOT proved here: the relayed-to-direct DOWNGRADE when the
//! last direct connection closes with a circuit remaining -- nothing
//! closes one connection of a pair on demand, so `PathChange::
//! DirectLost` is pinned by `path_events_are_once_per_logical_peer` in
//! the driver's unit tests and not measured on the wire; a hole punch
//! (step 8); and any NAT, since every address here is loopback.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use futures::StreamExt as _;
use interweave_local_client_api::EndpointLease;
use interweave_profile_config::{
    ChannelsConfig, DirectoryConfig, EndpointConfig, EndpointsConfig, ProfileConfig,
    RegistrationPolicy, TrustConfig, TrustPolicyKind,
};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{
    DirectMessageV2, EndpointId, MediaType, MessageId, Payload, TransportIdentity,
};
use interweave_transport_libp2p::runtime::DirectEndpoints;
use interweave_transport_libp2p::runtime::autonat_server_driver::AutonatServerSettings;
use interweave_transport_libp2p::runtime::relay_driver::{RelayClientSettings, StaticRelay};
use interweave_transport_libp2p::runtime::relay_server_driver::RelayServerSettings;
use interweave_transport_libp2p::{
    DialRefusal, PathChange, PeerPath, RelayReservationOutcome, SubstrateConfig, SwarmEvent,
    SwarmRuntime,
};
use interweave_transport_runtime::relay::ReservationConfig;
use interweave_transport_runtime::{DialDenial, TrustSources};
use interweave_trust_api::{EndpointTrustPolicy, InfrastructureSet, PeerTrustPolicy};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent as Libp2pSwarmEvent};
use libp2p::{Multiaddr, PeerId, identify, identity, relay};

const PATIENCE: Duration = Duration::from_secs(20);

/// How long a negative is watched for before it counts.
const WINDOW: Duration = Duration::from_secs(3);

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

fn endpoint(name: &str) -> EndpointId {
    EndpointId::parse(name).expect("valid endpoint id")
}

/// `human` alone, the default: the direct endpoints of both runtimes.
fn endpoints() -> DirectEndpoints {
    let profile = ProfileConfig {
        transport: interweave_profile_config::connectivity::TransportConfig::default(),
        schema_version: 2,
        trust: TrustConfig {
            policy: TrustPolicyKind::default(),
            allowed_peers: std::collections::BTreeSet::new(),
        },
        endpoints: EndpointsConfig {
            registration_policy: RegistrationPolicy::default(),
            default_direct_endpoint: Some(endpoint("human")),
            directory: DirectoryConfig::default(),
            entries: vec![EndpointConfig {
                id: endpoint("human"),
                enabled: true,
                advertise: false,
                allowed_client_kinds: Vec::new(),
                inbound: EndpointTrustPolicy::default(),
                outbound: EndpointTrustPolicy::default(),
            }],
        },
        discovery: interweave_profile_config::DiscoveryConfig::default(),
        channels: ChannelsConfig::default(),
    };
    DirectEndpoints::from_profile(&profile, 8).expect("a valid profile")
}

async fn configure_human(runtime: &SwarmRuntime) -> EndpointLease {
    runtime
        .configure_direct(endpoints())
        .await
        .expect("the endpoints install");
    runtime
        .claim_endpoint("human", endpoint("human"), "in-process")
        .await
        .expect("the claim reaches the task")
        .expect("the endpoint is configured and free")
}

fn frame(body: &[u8], id: u8) -> DirectMessageV2 {
    DirectMessageV2 {
        message_id: MessageId::from_bytes([id; 16]),
        sent_at_ms: 1_000,
        source_endpoint: endpoint("human"),
        destination_endpoint: None,
        payload: Payload::at_ceiling(
            Some(MediaType::parse("text/plain").expect("valid media type")),
            body.to_vec(),
        )
        .expect("within the ceiling"),
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

#[tokio::test]
async fn a_circuit_is_dialled_under_relay_circuit_and_carries_the_data_plane_at_both_ends() {
    // THE DIALER's identity first, so the target can trust it.
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
    configure_human(&target).await;
    let mut seen = Seen::default();

    // THE DIALER: the circuit transport and no reservation, the relay
    // infrastructure-only, and the target -- FIRST -- infrastructure-
    // only too, so the negative is measured before the positive.
    let mut dialer = SwarmRuntime::start(
        &dialer_id,
        SubstrateConfig {
            relay_client: Some(client_without_relays()),
            ..SubstrateConfig::default()
        },
        trust(&[], &[&relay_peer, &target_peer]),
    )
    .expect("the dialer starts");
    let lease = configure_human(&dialer).await;

    // THE NEGATIVE: a circuit to an infrastructure-only far end is
    // refused at the gate under `RelayCircuit`, before any socket --
    // the relay sees no connection from the dialer, whose client
    // behaviour would have dialled it for the circuit.
    let refused = dialer
        .dial(target_peer.clone(), circuit.clone())
        .await
        .expect("the command reaches the task");
    // The answer names the rule -- the far end is not a data-plane
    // peer -- and the command path answers its caller rather than
    // the gate's own record, which is for the dials nobody is told
    // about (`DialRefusals`).
    assert_eq!(
        refused,
        Err(DialRefusal::Policy(DialDenial::NotAuthorizedForDataPlane)),
        "an infrastructure-only far end is refused under RelayCircuit, before any socket"
    );
    let mut wire = Wire {
        target: &mut target,
        dialer: &mut dialer,
        relay: &mut relay,
        seen: &mut seen,
    };
    let quiet = settle(&mut wire, WINDOW).await;
    assert!(
        connected(&quiet, Side::Dialer, &target_peer).is_empty()
            && connected(&quiet, Side::Target, &dialer_peer).is_empty(),
        "nothing connected: {quiet:?}"
    );
    assert!(
        !wire.seen.established.contains(&pid(&dialer_peer)),
        "the relay never saw the dialer: the refusal precedes the socket"
    );

    // THE POSITIVE: with the target data-plane trusted the same dial
    // is admitted, the relay accepts the circuit from the dialer to
    // the target, and each end announces the other ONCE, relayed.
    wire.dialer
        .set_trust(trust(&[&target_peer], &[&relay_peer]))
        .await
        .expect("trust installs");
    wire.dialer
        .dial(target_peer.clone(), circuit.clone())
        .await
        .expect("the command reaches the task")
        .expect("a circuit to a data-plane peer is admitted");
    let mut events = until(&mut wire, "the target to announce the dialer", |s, e| {
        s == Side::Target && matches!(e, SwarmEvent::Connected { peer, .. } if *peer == dialer_peer)
    })
    .await;
    if connected(&events, Side::Dialer, &target_peer).is_empty() {
        events.extend(
            until(&mut wire, "the dialer to announce the target", |s, e| {
                s == Side::Dialer
                    && matches!(e, SwarmEvent::Connected { peer, .. } if *peer == target_peer)
            })
            .await,
        );
    }
    events.extend(settle(&mut wire, WINDOW).await);
    assert_eq!(
        connected(&events, Side::Dialer, &target_peer),
        vec![PeerPath::Relayed],
        "the dialer announced the target once, relayed: {events:?}"
    );
    assert_eq!(
        connected(&events, Side::Target, &dialer_peer),
        vec![PeerPath::Relayed],
        "the target announced the dialer once, relayed: {events:?}"
    );
    assert_eq!(
        wire.seen.circuits,
        vec![(pid(&dialer_peer), pid(&target_peer))],
        "the relay accepted exactly the one circuit, dialer to target"
    );

    // THE DATA PLANE RIDES THE CIRCUIT: a direct v2 message from the
    // dialer's lease reaches the target's default endpoint.
    let sent = tokio::select! {
        answer = wire.dialer.send_direct(&lease, target_peer.clone(), frame(b"over the circuit", 1)) => answer,
        () = async {
            loop {
                tokio::select! {
                    _ = wire.relay.select_next_some() => {}
                    _ = wire.target.next_event() => {}
                }
            }
        } => unreachable!("drives forever"),
    };
    let resolved = sent
        .expect("the command reaches the task")
        .expect("the exchange is accepted over the circuit");
    assert_eq!(resolved, endpoint("human"));
    let delivered = wire
        .target
        .drain_endpoint(endpoint("human"))
        .await
        .expect("the target answers");
    assert_eq!(delivered.len(), 1, "exactly one delivery");
    assert_eq!(delivered[0].payload.bytes(), b"over the circuit");

    // A DIRECT CONNECTION JOINS: the target listens, the dialer dials
    // it directly, and both ends report the path moving from relayed
    // to direct -- and NOT a second `Connected`.
    let direct = wire
        .target
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("a loopback address"))
        .await
        .expect("the target listens");
    wire.dialer
        .dial(target_peer.clone(), direct)
        .await
        .expect("the command reaches the task")
        .expect("the direct dial is admitted");
    let mut after = until(&mut wire, "the target's path to move", |s, e| {
        s == Side::Target
            && matches!(e, SwarmEvent::PeerPathChanged { peer, .. } if *peer == dialer_peer)
    })
    .await;
    if path_changes(&after, Side::Dialer, &target_peer).is_empty() {
        after.extend(
            until(&mut wire, "the dialer's path to move", |s, e| {
                s == Side::Dialer
                    && matches!(e, SwarmEvent::PeerPathChanged { peer, .. } if *peer == target_peer)
            })
            .await,
        );
    }
    after.extend(settle(&mut wire, WINDOW).await);
    let up = (
        PeerPath::Relayed,
        PeerPath::Direct,
        PathChange::DirectEstablished,
    );
    assert_eq!(path_changes(&after, Side::Dialer, &target_peer), vec![up]);
    assert_eq!(path_changes(&after, Side::Target, &dialer_peer), vec![up]);
    assert!(
        connected(&after, Side::Dialer, &target_peer).is_empty()
            && connected(&after, Side::Target, &dialer_peer).is_empty(),
        "a second connection to a connected peer is not a second Connected: {after:?}"
    );

    target.shutdown().await.expect("shutdown");
    dialer.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn an_infrastructure_only_source_over_a_circuit_is_refused_at_the_destination_with_the_servers_on()
 {
    // THE DIALER's identity first, so the target can hold it
    // infrastructure-only.
    let dialer_id = ProfileIdentity::generate();
    let dialer_peer = dialer_id.transport_identity().expect("peer id");
    // THE TARGET SERVES PROBES AND CIRCUITS, so its inbound arm would
    // ask under an infrastructure origin for every inbound -- which is
    // exactly what must NOT apply to a circuit's source.
    let Reserved {
        mut relay,
        relay_peer,
        mut target,
        target_peer,
        circuit,
    } = reserved(
        |client| SubstrateConfig {
            relay_client: Some(client),
            autonat_server: Some(AutonatServerSettings::default()),
            relay_server: Some(RelayServerSettings::default()),
            ..SubstrateConfig::default()
        },
        |relay_peer| trust(&[], &[relay_peer, &dialer_peer]),
    )
    .await;
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
    let mut wire = Wire {
        target: &mut target,
        dialer: &mut dialer,
        relay: &mut relay,
        seen: &mut seen,
    };

    // THE NEGATIVE: the dialer's circuit is admitted at ITS gate (the
    // target is data-plane trusted there) and accepted by the relay;
    // at the target it is established, refused under `RelayCircuit`,
    // and closed -- never announced as connected, never as
    // disconnected. The dialer, whose Noise completed, sees it come
    // and go.
    wire.dialer
        .dial(target_peer.clone(), circuit.clone())
        .await
        .expect("the command reaches the task")
        .expect("the dialer's gate admits a circuit to a data-plane peer");
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
        connected(&events, Side::Target, &dialer_peer).is_empty(),
        "an infrastructure-only source over a circuit is never announced: {events:?}"
    );
    assert!(
        !events.iter().any(|(s, e)| *s == Side::Target
            && matches!(e, SwarmEvent::Disconnected { peer } if *peer == dialer_peer)),
        "nor its close: {events:?}"
    );
    assert_eq!(
        connected(&events, Side::Dialer, &target_peer),
        vec![PeerPath::Relayed],
        "the dialer saw the circuit come up before the target closed it"
    );

    // THE CONTROL, with the same servers on: the same source given
    // data-plane trust is retained over the circuit. Dialled THROUGH
    // THE BOOK this time: the first circuit established at the dialer
    // before the target closed it, so the route it worked over is
    // remembered -- the relay's address with the circuit marker, the
    // target's own suffix stripped -- and `DialPeer` redials it as a
    // relay circuit from the stored string.
    wire.target
        .set_trust(trust(&[&dialer_peer], &[&relay_peer]))
        .await
        .expect("trust installs");
    wire.dialer
        .dial_peer(target_peer.clone())
        .await
        .expect("the command reaches the task")
        .expect("the learned circuit route is admitted from the book");
    let mut retained = until(&mut wire, "the target to announce the dialer", |s, e| {
        s == Side::Target && matches!(e, SwarmEvent::Connected { peer, .. } if *peer == dialer_peer)
    })
    .await;
    retained.extend(settle(&mut wire, WINDOW).await);
    assert_eq!(
        connected(&retained, Side::Target, &dialer_peer),
        vec![PeerPath::Relayed]
    );
    assert!(
        !retained.iter().any(|(s, e)| *s == Side::Target
            && matches!(e, SwarmEvent::Disconnected { peer } if *peer == dialer_peer)),
        "and it stays: {retained:?}"
    );
    assert_eq!(
        wire.seen.circuits.len(),
        2,
        "two circuits, the relay's count"
    );

    target.shutdown().await.expect("shutdown");
    dialer.shutdown().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn a_circuit_route_that_failed_is_retried_as_a_relay_circuit() {
    // THE RELAY, with an external address and NOBODY reserved on it:
    // every circuit request is denied NoReservation, which at the
    // dialer is a transient failure of the route -- the kind the
    // scheduler retries. Paused time, so the thirty-second backoff
    // costs nothing; the relay is driven in the loop below.
    let relay_keys = identity::Keypair::generate_ed25519();
    let relay_peer = identity_of(&relay_keys);
    let mut relay = relay_server(relay_keys);
    let relay_addr = bound(&mut relay).await;
    relay.add_external_address(relay_addr.clone());
    let mut seen = Seen::default();

    // THE FAR END: a data-plane peer that exists nowhere; the circuit
    // to it is the route under test.
    let far_id = ProfileIdentity::generate();
    let far_peer = far_id.transport_identity().expect("peer id");
    let circuit = circuit_of(&relay_addr, &relay_peer, &far_peer);

    let dialer_id = ProfileIdentity::generate();
    let dialer_peer = dialer_id.transport_identity().expect("peer id");
    let mut dialer = SwarmRuntime::start(
        &dialer_id,
        SubstrateConfig {
            relay_client: Some(client_without_relays()),
            ..SubstrateConfig::default()
        },
        trust(&[&far_peer], &[&relay_peer]),
    )
    .expect("the dialer starts");

    // ONE dial from the command path; every later attempt is the
    // scheduler's, since nobody here asks again.
    dialer
        .dial(far_peer.clone(), circuit)
        .await
        .expect("the command reaches the task")
        .expect("a circuit to a data-plane peer is admitted");

    // Two failures reported and two circuit requests DENIED AT THE
    // RELAY: the first is the command's, the second can only be the
    // scheduler's retry, and that it reached the relay at all is the
    // claim -- a retry ticketed under the scheduler's own origin is
    // refused at the gate's pairing check before any socket, reported
    // as undialable, and never seen by the relay.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
    let mut failures: Vec<String> = Vec::new();
    while failures.len() < 2 || seen.denied.len() < 2 {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "the scheduler's retry never reached the relay: failures {failures:?}, denied {:?}",
            seen.denied
        );
        tokio::select! {
            event = dialer.next_event() => {
                if let SwarmEvent::DialFailed { peer, detail } = event.expect("the dialer is alive") {
                    assert_eq!(peer.as_ref(), Some(&far_peer));
                    failures.push(detail);
                }
            }
            event = relay.select_next_some() => note_relay(&mut seen, event),
            () = tokio::time::sleep(remaining) => {}
        }
    }
    assert_eq!(
        seen.denied,
        vec![(pid(&dialer_peer), pid(&far_peer)); 2],
        "both attempts reached the relay and were denied there"
    );
    assert!(
        failures.iter().all(|d| !d.contains("admission claims")),
        "no attempt was refused at the pairing check: {failures:?}"
    );

    dialer.shutdown().await.expect("shutdown");
}
