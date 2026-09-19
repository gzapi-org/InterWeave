// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Stage 11 phase 8: the relay rows of `transport/libp2p/CONNECTIVITY.md`
//! §25 over real sockets -- items 2, 3 and 18.
//!
//! A subject reserves on two of three static relays (the target for an
//! unknown verdict is two; the third is a candidate the manager does
//! not ask while the target is met). What is proved:
//!
//! - item 2: with both reservations held, a dialer reaches the subject
//!   through either relay -- a circuit through the first connects the
//!   peer, a circuit through the second is admitted as a second path
//!   to the same peer and the second relay saw it;
//! - item 3: the first relay disappears -- its process ends -- and the
//!   subject's reservation on it is lost, the standing falls to
//!   partial, the manager asks the spare, and the target is met again;
//!   the subject's PeerId is unchanged, since a dialer reaches it
//!   through the spare's circuit at the same identity;
//! - item 18: a dialer that held ONLY the lost relay's circuit is told
//!   `Disconnected`, and a direct send it makes with no path left is
//!   answered `PeerUnreachable` at once -- a transport-v2 verdict, not
//!   a hang -- while the dialer that still holds the second relay's
//!   circuit sends through it.
//!
//! What is NOT proved here: a relay that fails mid-exchange (the
//! request-response failure is the crate's; `dcutr.rs` pins an exchange
//! outliving a retirement), and any NAT: every address is loopback.

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
    DirectMessageV2, EndpointId, MediaType, MessageId, Payload, TransportError, TransportIdentity,
};
use interweave_transport_libp2p::runtime::DirectEndpoints;
use interweave_transport_libp2p::runtime::relay_driver::{RelayClientSettings, StaticRelay};
use interweave_transport_libp2p::{
    PeerPath, RelayReservationOutcome, SubstrateConfig, SwarmEvent, SwarmRuntime,
};
use interweave_transport_runtime::TrustSources;
use interweave_transport_runtime::relay::{ReservationConfig, Standing};
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

/// A bare relay server: the crate's own, defaults, no policy.
fn relay_server(keys: identity::Keypair) -> libp2p::Swarm<RelayBehaviour> {
    libp2p::SwarmBuilder::with_existing_identity(keys)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("the same transport stack the subject uses")
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

fn circuit_of(
    relay_addr: &Multiaddr,
    relay: &TransportIdentity,
    subject: &TransportIdentity,
) -> Multiaddr {
    format!(
        "{relay_addr}/p2p/{}/p2p-circuit/p2p/{}",
        relay.as_str(),
        subject.as_str()
    )
    .parse()
    .expect("a circuit address")
}

fn endpoint(name: &str) -> EndpointId {
    EndpointId::parse(name).expect("valid endpoint id")
}

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

/// What the relays saw.
#[derive(Default, Debug)]
struct Seen {
    accepted: Vec<PeerId>,
    circuits: Vec<(PeerId, PeerId)>,
}

fn note_relay(seen: &mut Seen, event: Libp2pSwarmEvent<RelayBehaviourEvent>) {
    match event {
        Libp2pSwarmEvent::Behaviour(RelayBehaviourEvent::Relay(
            relay::Event::ReservationReqAccepted { src_peer_id, .. },
        )) => seen.accepted.push(src_peer_id),
        Libp2pSwarmEvent::Behaviour(RelayBehaviourEvent::Relay(
            relay::Event::CircuitReqAccepted {
                src_peer_id,
                dst_peer_id,
                ..
            },
        )) => seen.circuits.push((src_peer_id, dst_peer_id)),
        _ => {}
    }
}

/// The next event of a relay that may have been dropped: a dropped one
/// never yields, so its arm never fires.
async fn next_relay(
    relay: &mut Option<libp2p::Swarm<RelayBehaviour>>,
) -> Libp2pSwarmEvent<RelayBehaviourEvent> {
    match relay {
        Some(swarm) => swarm.select_next_some().await,
        None => std::future::pending().await,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Subject,
    Dialer,
    Other,
}

/// Everything the test drives: three relays -- any of which may have
/// been dropped, which is how a relay "disappears" -- the subject, and
/// two dialers.
struct Wire {
    relays: [Option<libp2p::Swarm<RelayBehaviour>>; 3],
    seen: [Seen; 3],
    subject: SwarmRuntime,
    dialer: SwarmRuntime,
    other: SwarmRuntime,
}

impl Wire {
    /// Drive until `pred` matches, or for `window` when it is `None`.
    async fn drive<F>(
        &mut self,
        what: &str,
        window: Duration,
        mut pred: Option<F>,
    ) -> Vec<(Side, SwarmEvent)>
    where
        F: FnMut(Side, &SwarmEvent) -> bool,
    {
        let mut events = Vec::new();
        let deadline = tokio::time::Instant::now() + window;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                assert!(pred.is_none(), "timed out waiting for {what}: {events:?}");
                return events;
            }
            let [a, b, c] = &mut self.relays;
            tokio::select! {
                event = self.subject.next_event() => {
                    let event = event.expect("the subject is alive");
                    let hit = pred.as_mut().is_some_and(|p| p(Side::Subject, &event));
                    events.push((Side::Subject, event));
                    if hit { return events; }
                }
                event = self.dialer.next_event() => {
                    let event = event.expect("the dialer is alive");
                    let hit = pred.as_mut().is_some_and(|p| p(Side::Dialer, &event));
                    events.push((Side::Dialer, event));
                    if hit { return events; }
                }
                event = self.other.next_event() => {
                    let event = event.expect("the other dialer is alive");
                    let hit = pred.as_mut().is_some_and(|p| p(Side::Other, &event));
                    events.push((Side::Other, event));
                    if hit { return events; }
                }
                event = next_relay(a) => note_relay(&mut self.seen[0], event),
                event = next_relay(b) => note_relay(&mut self.seen[1], event),
                event = next_relay(c) => note_relay(&mut self.seen[2], event),
                () = tokio::time::sleep(remaining) => {
                    assert!(pred.is_none(), "timed out waiting for {what}: {events:?}");
                    return events;
                }
            }
        }
    }

    async fn until<F>(&mut self, what: &str, pred: F) -> Vec<(Side, SwarmEvent)>
    where
        F: FnMut(Side, &SwarmEvent) -> bool,
    {
        self.drive(what, PATIENCE, Some(pred)).await
    }

    async fn settle(&mut self, window: Duration) -> Vec<(Side, SwarmEvent)> {
        self.drive::<fn(Side, &SwarmEvent) -> bool>("settling", window, None)
            .await
    }
}

fn reservation(
    events: &[(Side, SwarmEvent)],
    relay: &TransportIdentity,
) -> Vec<RelayReservationOutcome> {
    events
        .iter()
        .filter_map(|(s, e)| match e {
            SwarmEvent::RelayReservationChanged {
                relay: r, outcome, ..
            } if *s == Side::Subject && r == relay => Some(*outcome),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn two_reservations_are_held_the_peer_is_reached_through_either_and_a_lost_relay_is_replaced()
{
    // THREE RELAYS, each advertising what it bound.
    let mut relays = Vec::new();
    for _ in 0..3 {
        let keys = identity::Keypair::generate_ed25519();
        let peer = identity_of(&keys);
        let mut swarm = relay_server(keys);
        let addr = bound(&mut swarm).await;
        swarm.add_external_address(addr.clone());
        relays.push((swarm, peer, addr));
    }
    let relay_peers: Vec<TransportIdentity> = relays.iter().map(|(_, p, _)| p.clone()).collect();
    let relay_addrs: Vec<Multiaddr> = relays.iter().map(|(_, _, a)| a.clone()).collect();
    let infra: Vec<&TransportIdentity> = relay_peers.iter().collect();

    // THE SUBJECT: all three static, the target two.
    let subject_id = ProfileIdentity::generate();
    let subject_peer = subject_id.transport_identity().expect("peer id");
    let dialer_id = ProfileIdentity::generate();
    let dialer_peer = dialer_id.transport_identity().expect("peer id");
    let other_id = ProfileIdentity::generate();
    let other_peer = other_id.transport_identity().expect("peer id");
    let subject = SwarmRuntime::start(
        &subject_id,
        SubstrateConfig {
            relay_client: Some(RelayClientSettings {
                static_relays: relay_peers
                    .iter()
                    .zip(&relay_addrs)
                    .map(|(peer, addr)| StaticRelay {
                        peer: peer.clone(),
                        address: format!("{addr}/p2p/{}", peer.as_str()),
                    })
                    .collect(),
                use_authorized_identify_relays: false,
                reservations: ReservationConfig {
                    retry_min_ms: 1_000,
                    retry_max_ms: 4_000,
                    ..ReservationConfig::default()
                },
                direct_head_start_ms: 750,
            }),
            ..SubstrateConfig::default()
        },
        trust(&[&dialer_peer, &other_peer], &infra),
    )
    .expect("the subject starts");
    configure_human(&subject).await;
    let without_relays = || SubstrateConfig {
        relay_client: Some(RelayClientSettings {
            static_relays: Vec::new(),
            use_authorized_identify_relays: false,
            reservations: ReservationConfig::default(),
            direct_head_start_ms: 750,
        }),
        ..SubstrateConfig::default()
    };
    let dialer = SwarmRuntime::start(
        &dialer_id,
        without_relays(),
        trust(&[&subject_peer], &infra),
    )
    .expect("the dialer starts");
    let dialer_lease = configure_human(&dialer).await;
    let other = SwarmRuntime::start(&other_id, without_relays(), trust(&[&subject_peer], &infra))
        .expect("the other dialer starts");
    let other_lease = configure_human(&other).await;
    let mut relays = relays.into_iter();
    let (a, _, _) = relays.next().expect("three");
    let (b, _, _) = relays.next().expect("three");
    let (c, _, _) = relays.next().expect("three");
    let mut wire = Wire {
        relays: [Some(a), Some(b), Some(c)],
        seen: [Seen::default(), Seen::default(), Seen::default()],
        subject,
        dialer,
        other,
    };

    // ITEM 2, FIRST HALF: two reservations held, the target met, the
    // third relay not asked.
    let mut events = wire
        .until("the target to be met", |s, e| {
            s == Side::Subject
                && matches!(
                    e,
                    SwarmEvent::RelayStandingChanged {
                        standing: Standing::Satisfied,
                        active: 2,
                        ..
                    }
                )
        })
        .await;
    events.extend(wire.settle(WINDOW).await);
    let held: Vec<usize> = (0..3)
        .filter(|i| {
            reservation(&events, &relay_peers[*i]).contains(&RelayReservationOutcome::Accepted)
        })
        .collect();
    assert_eq!(held.len(), 2, "two reservations held: {events:?}");
    let spare = (0..3).find(|i| !held.contains(i)).expect("one spare");
    assert!(
        wire.seen[spare].accepted.is_empty(),
        "the spare was not asked while the target was met"
    );
    let circuit = |i: usize| circuit_of(&relay_addrs[i], &relay_peers[i], &subject_peer);

    // ITEM 2, SECOND HALF: reached through either. The dialer connects
    // through the first held relay; a circuit through the second is a
    // second path to the same peer -- admitted, seen at that relay,
    // and no second Connected.
    wire.dialer
        .dial(subject_peer.clone(), circuit(held[0]))
        .await
        .expect("the command reaches the task")
        .expect("admitted");
    events.extend(
        wire.until("the dialer to connect through the first relay", |s, e| {
            s == Side::Dialer
                && matches!(e, SwarmEvent::Connected { peer, path: PeerPath::Relayed } if *peer == subject_peer)
        })
        .await,
    );
    wire.dialer
        .dial(subject_peer.clone(), circuit(held[1]))
        .await
        .expect("the command reaches the task")
        .expect("admitted");
    events.extend(wire.settle(WINDOW).await);
    assert_eq!(
        wire.seen[held[0]].circuits,
        vec![(pid(&dialer_peer), pid(&subject_peer))],
        "the first relay carried the dialer's circuit"
    );
    assert_eq!(
        wire.seen[held[1]].circuits,
        vec![(pid(&dialer_peer), pid(&subject_peer))],
        "and the second carried the second one"
    );
    assert_eq!(
        events
            .iter()
            .filter(|(s, e)| *s == Side::Dialer
                && matches!(e, SwarmEvent::Connected { peer, .. } if *peer == subject_peer))
            .count(),
        1,
        "one Connected for the one logical peer: {events:?}"
    );
    // THE OTHER DIALER holds only the first relay's circuit.
    wire.other
        .dial(subject_peer.clone(), circuit(held[0]))
        .await
        .expect("the command reaches the task")
        .expect("admitted");
    events.extend(
        wire.until("the other dialer to connect", |s, e| {
            s == Side::Other
                && matches!(e, SwarmEvent::Connected { peer, path: PeerPath::Relayed } if *peer == subject_peer)
        })
        .await,
    );

    // ITEM 3: THE FIRST HELD RELAY DISAPPEARS -- its process ends.
    let lost = held[0];
    let kept = held[1];
    drop(wire.relays[lost].take());
    let mut after = wire
        .until("the spare to be reserved on", |s, e| {
            s == Side::Subject
                && matches!(e, SwarmEvent::RelayReservationChanged { relay, outcome: RelayReservationOutcome::Accepted, .. } if *relay == relay_peers[spare])
        })
        .await;
    after.extend(wire.settle(WINDOW).await);
    assert!(
        reservation(&after, &relay_peers[lost]).contains(&RelayReservationOutcome::Lost),
        "the lost relay's reservation was reported lost: {after:?}"
    );
    assert!(
        after.iter().any(|(s, e)| *s == Side::Subject
            && matches!(
                e,
                SwarmEvent::RelayStandingChanged {
                    standing: Standing::Partial,
                    active: 1,
                    ..
                }
            )),
        "the standing fell to partial on the loss: {after:?}"
    );
    assert!(
        after.iter().any(|(s, e)| *s == Side::Subject
            && matches!(
                e,
                SwarmEvent::RelayStandingChanged {
                    standing: Standing::Satisfied,
                    active: 2,
                    ..
                }
            )),
        "and the target was met again through the spare: {after:?}"
    );
    assert!(
        !reservation(&after, &relay_peers[kept])
            .iter()
            .any(|o| *o != RelayReservationOutcome::Renewed),
        "the kept relay's reservation was untouched: {after:?}"
    );
    // ITEM 18: the other dialer, whose only path was the lost relay's
    // circuit, is told, and a send with no path left is answered at
    // once -- PeerUnreachable, the transport-v2 verdict -- not held.
    assert!(
        after.iter().any(|(s, e)| *s == Side::Other
            && matches!(e, SwarmEvent::Disconnected { peer } if *peer == subject_peer)),
        "the other dialer lost its only path: {after:?}"
    );
    let asked_at = tokio::time::Instant::now();
    let answer = wire
        .other
        .send_direct(
            &other_lease,
            subject_peer.clone(),
            frame(b"nowhere to go", 1),
        )
        .await
        .expect("the command reaches the task");
    assert_eq!(answer, Err(TransportError::PeerUnreachable), "no path left");
    assert!(
        asked_at.elapsed() < Duration::from_secs(1),
        "answered at once, not held to a timeout: {:?}",
        asked_at.elapsed()
    );
    // The dialer that still holds the kept relay's circuit sends
    // through it, and the peer stayed connected for it.
    assert!(
        !after.iter().any(|(s, e)| *s == Side::Dialer
            && matches!(e, SwarmEvent::Disconnected { peer } if *peer == subject_peer)),
        "the dialer holding the kept relay's circuit stayed connected: {after:?}"
    );
    let sent = tokio::select! {
        answer = wire.dialer.send_direct(&dialer_lease, subject_peer.clone(), frame(b"through the kept relay", 2)) => answer,
        () = async {
            loop {
                let [a, b, c] = &mut wire.relays;
                tokio::select! {
                    _ = wire.subject.next_event() => {}
                    _ = wire.other.next_event() => {}
                    _ = next_relay(a) => {}
                    _ = next_relay(b) => {}
                    _ = next_relay(c) => {}
                }
            }
        } => unreachable!("drives forever"),
    };
    assert_eq!(
        sent.expect("the command reaches the task")
            .expect("delivered through the kept relay"),
        endpoint("human")
    );
    // ITEM 3's LAST CLAUSE: the same PeerId through the spare -- the
    // other dialer reaches the subject again, at the identity it had.
    wire.other
        .dial(subject_peer.clone(), circuit(spare))
        .await
        .expect("the command reaches the task")
        .expect("admitted");
    let again = wire
        .until("the other dialer to reconnect through the spare", |s, e| {
            s == Side::Other
                && matches!(e, SwarmEvent::Connected { peer, path: PeerPath::Relayed } if *peer == subject_peer)
        })
        .await;
    assert!(
        wire.seen[spare]
            .circuits
            .contains(&(pid(&other_peer), pid(&subject_peer))),
        "the spare carried the circuit: {:?}",
        wire.seen[spare]
    );
    drop(again);

    wire.subject.shutdown().await.expect("shutdown");
    wire.dialer.shutdown().await.expect("shutdown");
    wire.other.shutdown().await.expect("shutdown");
}
