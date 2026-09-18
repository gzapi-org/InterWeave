// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Stage 11 step 5, second half: the relay CLIENT over real sockets.
//!
//! What is proved here, against a bare relay server that has an
//! external address (SPIKE-004 note 10: without one no reservation
//! carries an address and no circuit completes):
//!
//! - a static relay is dialled by the client behaviour under
//!   `RelayReservation` -- nobody asked the subject to dial -- and the
//!   gate admits it toward an infrastructure-only peer (route 1);
//! - the reservation the relay accepts is advertised as the circuit
//!   address the relay reported, and the relay itself records the
//!   acceptance from the subject (the positive control: two ends of
//!   one exchange);
//! - what the subject offers the relay on that retained connection is
//!   Identify and the stop protocol and nothing else (`ClassGated` for
//!   the infrastructure service);
//! - a static relay in no trust set is refused by the gate under the
//!   same origin, never reaches a socket, and the manager records the
//!   failed ask (the negative control, beside the positive one);
//! - the loss of the relay's connection withdraws the address in the
//!   same second (SPIKE-004 R10.10's bound), and the relay is asked
//!   again after its backoff and accepted again;
//! - under the opt-in, a data-plane-trusted peer whose Identify
//!   advertises the hop protocol is learned and reserved on OVER THE
//!   EXISTING connection -- the relay sees one connection, not two --
//!   and forgotten with its address when it loses its authorization;
//!   a trusted peer advertising no hop protocol is never a relay.
//!
//! What is NOT proved here: a circuit through the reservation (step
//! 7), and a reservation's renewal (the crate's default lifetime is an
//! hour). Loopback addresses stand in for the relay's public ones: the
//! relay advertises what it bound, as SPIKE-004's harness did.

#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::time::Duration;

use futures::StreamExt as _;
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;

use interweave_transport_libp2p::runtime::relay_driver::{RelayClientSettings, StaticRelay};
use interweave_transport_libp2p::{
    RelayReservationOutcome, SubstrateConfig, SwarmEvent, SwarmRuntime,
};
use interweave_transport_runtime::relay::{ReservationConfig, Standing};
use interweave_transport_runtime::{DialOrigin, TrustSources};
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent as Libp2pSwarmEvent};
use libp2p::{Multiaddr, PeerId, identify, identity, relay};

const PATIENCE: Duration = Duration::from_secs(20);

/// How long a negative is watched for before it counts.
const WINDOW: Duration = Duration::from_secs(3);

/// `RELAY.md` §4: the withdrawal follows the loss, not a timer.
const WITHDRAWAL_BOUND: Duration = Duration::from_secs(1);

/// What the subject offers a relay on the connection it reserves over.
const OFFERED_TO_A_RELAY: &[&str] = &[
    "/ipfs/id/1.0.0",
    "/ipfs/id/push/1.0.0",
    "/libp2p/circuit/relay/0.2.0/stop",
];

#[derive(NetworkBehaviour)]
struct RelayBehaviour {
    identify: identify::Behaviour,
    relay: relay::Behaviour,
}

/// A bare relay server: the crate's own, defaults, no policy -- the
/// subject is what is under test.
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

/// A trusted peer that is not a relay: Identify only.
fn bystander(keys: identity::Keypair) -> libp2p::Swarm<identify::Behaviour> {
    libp2p::SwarmBuilder::with_existing_identity(keys)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("the same transport stack the subject uses")
        .with_behaviour(|k| {
            identify::Behaviour::new(identify::Config::new(
                "/interweave-relay-test-bystander/1".to_owned(),
                k.public(),
            ))
        })
        .expect("behaviour")
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(600)))
        .build()
}

async fn bound<B: NetworkBehaviour>(swarm: &mut libp2p::Swarm<B>) -> Multiaddr
where
    B::ToSwarm: std::fmt::Debug,
{
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

fn infrastructure_only(infra: &[&TransportIdentity]) -> TrustSources {
    TrustSources::new(
        PeerTrustPolicy::new(std::iter::empty()).expect("an empty allowlist"),
        InfrastructureSet::new(infra.iter().map(|p| (*p).clone())).expect("a small set"),
    )
}

fn data_plane(peers: &[&TransportIdentity]) -> TrustSources {
    TrustSources::new(
        PeerTrustPolicy::new(peers.iter().map(|p| (*p).clone())).expect("a small allowlist"),
        InfrastructureSet::new(std::iter::empty()).expect("an empty set"),
    )
}

/// What the bare swarms saw, kept across the whole test so a control
/// can be read at the end.
#[derive(Default)]
struct Seen {
    /// Reservation acceptances the relay recorded, by source.
    accepted: Vec<PeerId>,
    /// Connections the relay saw established, by peer.
    established: Vec<PeerId>,
    /// The protocol set the relay was told the subject offers, per
    /// connection it identified.
    offered: Vec<BTreeSet<String>>,
}

fn note_relay(seen: &mut Seen, event: Libp2pSwarmEvent<RelayBehaviourEvent>) {
    match event {
        Libp2pSwarmEvent::ConnectionEstablished { peer_id, .. } => seen.established.push(peer_id),
        Libp2pSwarmEvent::Behaviour(RelayBehaviourEvent::Relay(
            relay::Event::ReservationReqAccepted { src_peer_id, .. },
        )) => seen.accepted.push(src_peer_id),
        Libp2pSwarmEvent::Behaviour(RelayBehaviourEvent::Identify(identify::Event::Received {
            info,
            ..
        })) => seen
            .offered
            .push(info.protocols.iter().map(ToString::to_string).collect()),
        _ => {}
    }
}

/// The circuit address a reservation on `relay` at `relay_addr` gives
/// `subject`: what the relay reports, as the crate suffixes it.
fn circuit_of(
    relay_addr: &Multiaddr,
    relay: &TransportIdentity,
    subject: &TransportIdentity,
) -> String {
    format!(
        "{relay_addr}/p2p/{}/p2p-circuit/p2p/{}",
        relay.as_str(),
        subject.as_str()
    )
}

/// The bare relays a test drives while it waits on the subject: one,
/// and possibly a second.
struct Relays<'a> {
    first: (&'a mut libp2p::Swarm<RelayBehaviour>, &'a mut Seen),
    second: Option<(&'a mut libp2p::Swarm<RelayBehaviour>, &'a mut Seen)>,
}

/// Wait for one subject event matching `pred`, driving the relay swarms
/// meanwhile and noting what they saw, or collect every subject event
/// for `window` when `pred` is `None`. Returns the matched event and
/// when it arrived, and everything seen before it.
async fn drive<F>(
    subject: &mut SwarmRuntime,
    relays: &mut Relays<'_>,
    what: &str,
    window: Duration,
    mut pred: Option<F>,
) -> (Option<(SwarmEvent, tokio::time::Instant)>, Vec<SwarmEvent>)
where
    F: FnMut(&SwarmEvent) -> bool,
{
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            assert!(pred.is_none(), "timed out waiting for {what}");
            return (None, events);
        }
        let has_second = relays.second.is_some();
        let (first, first_seen) = (&mut *relays.first.0, &mut *relays.first.1);
        let second_swarm = relays.second.as_mut();
        let (second, second_seen) = match second_swarm {
            Some((swarm, seen)) => (Some(&mut **swarm), Some(&mut **seen)),
            None => (None, None),
        };
        let second_next = async {
            match second {
                Some(swarm) => swarm.select_next_some().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            event = subject.next_event() => {
                let event = event.expect("the runtime is alive");
                if pred.as_mut().is_some_and(|p| p(&event)) {
                    return (Some((event, tokio::time::Instant::now())), events);
                }
                events.push(event);
            }
            event = first.select_next_some() => note_relay(first_seen, event),
            event = second_next, if has_second => {
                if let Some(seen) = second_seen {
                    note_relay(seen, event);
                }
            }
            () = tokio::time::sleep(remaining) => {
                assert!(pred.is_none(), "timed out waiting for {what}");
                return (None, events);
            }
        }
    }
}

async fn subject_event<F>(
    subject: &mut SwarmRuntime,
    relays: &mut Relays<'_>,
    what: &str,
    pred: F,
) -> (SwarmEvent, tokio::time::Instant, Vec<SwarmEvent>)
where
    F: FnMut(&SwarmEvent) -> bool,
{
    let (hit, before) = drive(subject, relays, what, PATIENCE, Some(pred)).await;
    let (event, at) = hit.expect("returned only on a match");
    (event, at, before)
}

async fn settle(
    subject: &mut SwarmRuntime,
    relays: &mut Relays<'_>,
    window: Duration,
) -> Vec<SwarmEvent> {
    drive::<fn(&SwarmEvent) -> bool>(subject, relays, "settling", window, None)
        .await
        .1
}

fn settings(static_relays: Vec<StaticRelay>, learn: bool) -> RelayClientSettings {
    RelayClientSettings {
        static_relays,
        use_authorized_identify_relays: learn,
        reservations: ReservationConfig {
            // Short, so the re-ask after a loss is seen inside the
            // test's patience; the profile's floor is 1 s.
            retry_min_ms: 1_000,
            retry_max_ms: 4_000,
            ..ReservationConfig::default()
        },
    }
}

#[tokio::test]
async fn a_static_relay_is_reserved_on_under_relay_reservation_and_the_address_follows_the_reservation()
 {
    // THE RELAY, listening first and ADVERTISING what it bound (note
    // 10): a reservation's addresses are the relay's external ones.
    let relay_keys = identity::Keypair::generate_ed25519();
    let relay_peer = identity_of(&relay_keys);
    let mut relay = relay_server(relay_keys);
    let relay_addr = bound(&mut relay).await;
    relay.add_external_address(relay_addr.clone());
    let mut relay_seen = Seen::default();

    // THE STRANGER: a relay like the first, configured static too,
    // in no trust set. The negative control for the gate.
    let stranger_keys = identity::Keypair::generate_ed25519();
    let stranger_peer = identity_of(&stranger_keys);
    let mut stranger = relay_server(stranger_keys);
    let stranger_addr = bound(&mut stranger).await;
    stranger.add_external_address(stranger_addr.clone());
    let mut stranger_seen = Seen::default();

    // THE SUBJECT: the production runtime, both relays static, the
    // first infrastructure-only and the second nothing.
    let subject_id = ProfileIdentity::generate();
    let subject_peer = subject_id.transport_identity().expect("peer id");
    let subject_pid: PeerId = subject_peer.as_str().parse().expect("a libp2p identity");
    let config = SubstrateConfig {
        relay_client: Some(settings(
            vec![
                StaticRelay {
                    peer: relay_peer.clone(),
                    address: format!("{relay_addr}/p2p/{}", relay_peer.as_str()),
                },
                StaticRelay {
                    peer: stranger_peer.clone(),
                    address: format!("{stranger_addr}/p2p/{}", stranger_peer.as_str()),
                },
            ],
            false,
        )),
        ..SubstrateConfig::default()
    };
    let mut subject = SwarmRuntime::start(&subject_id, config, infrastructure_only(&[&relay_peer]))
        .expect("the runtime starts");
    let circuit = circuit_of(&relay_addr, &relay_peer, &subject_peer);

    // ROUTE 1. Nobody asked the subject to dial; the client behaviour
    // dials the relay for its reservation under `RelayReservation`, the
    // gate admits it toward an infrastructure-only peer, the relay
    // accepts, and the address the relay reported is advertised.
    let mut relays = Relays {
        first: (&mut relay, &mut relay_seen),
        second: Some((&mut stranger, &mut stranger_seen)),
    };
    let (accepted, _, before) = subject_event(
        &mut subject,
        &mut relays,
        "the reservation to be accepted",
        |e| {
            matches!(
                e,
                SwarmEvent::RelayReservationChanged {
                    relay,
                    outcome: RelayReservationOutcome::Accepted,
                    ..
                } if *relay == relay_peer
            )
        },
    )
    .await;
    let SwarmEvent::RelayReservationChanged { addresses, .. } = &accepted else {
        unreachable!("matched above");
    };
    assert_eq!(
        addresses,
        &vec![circuit.clone()],
        "the advertised address is the relay's external address as a circuit to the subject"
    );

    // THE NEGATIVE CONTROL, and the standing: the stranger's ask fails
    // at the gate, and the target counts it as a candidate the manager
    // cannot fill.
    let mut events = before;
    events.push(accepted.clone());
    events.extend(settle(&mut subject, &mut relays, WINDOW).await);
    assert!(
        events.iter().any(|e| matches!(
            e,
            SwarmEvent::RelayReservationChanged {
                relay,
                outcome: RelayReservationOutcome::Failed,
                ..
            } if *relay == stranger_peer
        )),
        "the stranger's ask fails: {events:?}"
    );
    let refusals = subject.dial_refusals();
    assert!(
        refusals
            .counts()
            .keys()
            .any(|(origin, _)| *origin == Some(DialOrigin::RelayReservation)),
        "and the gate recorded the refusal under RelayReservation: {:?}",
        refusals.counts()
    );
    assert!(
        relays
            .second
            .as_ref()
            .is_some_and(|(_, seen)| seen.established.is_empty()),
        "the stranger never saw a connection: the refusal precedes the socket"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            SwarmEvent::RelayStandingChanged {
                standing: Standing::Partial,
                active: 1,
                target: 2
            }
        )),
        "one of two candidates held is Partial, reported rather than retried into: {events:?}"
    );

    // THE POSITIVE CONTROL AT THE OTHER END: the relay recorded the
    // subject's reservation, on the one connection the subject
    // dialled, and was told the subject offers Identify and the stop
    // protocol and nothing else on it.
    assert_eq!(
        relays.first.1.accepted,
        vec![subject_pid],
        "the relay accepted exactly one reservation, from the subject"
    );
    assert_eq!(relays.first.1.established, vec![subject_pid]);
    let expected: BTreeSet<String> = OFFERED_TO_A_RELAY.iter().map(|s| (*s).to_owned()).collect();
    assert_eq!(
        relays.first.1.offered.first(),
        Some(&expected),
        "the retained connection to an infrastructure-only relay is class-gated"
    );

    // THE LOSS. The relay drops the connection; the subject withdraws
    // the address within a second of seeing the connection go, and the
    // relay is asked again after its backoff.
    assert!(relays.first.0.disconnect_peer_id(subject_pid).is_ok());
    let mut disconnected_at = None;
    let (lost, lost_at, _) = subject_event(
        &mut subject,
        &mut relays,
        "the reservation to be lost",
        |e| {
            if matches!(e, SwarmEvent::Disconnected { peer } if *peer == relay_peer) {
                disconnected_at = Some(tokio::time::Instant::now());
            }
            matches!(
                e,
                SwarmEvent::RelayReservationChanged {
                    relay,
                    outcome: RelayReservationOutcome::Lost,
                    ..
                } if *relay == relay_peer
            )
        },
    )
    .await;
    let SwarmEvent::RelayReservationChanged { addresses, .. } = &lost else {
        unreachable!("matched above");
    };
    assert_eq!(
        addresses,
        &vec![circuit.clone()],
        "the withdrawn address is the advertised one"
    );
    if let Some(at) = disconnected_at {
        assert!(
            lost_at.saturating_duration_since(at) <= WITHDRAWAL_BOUND,
            "withdrawn within {WITHDRAWAL_BOUND:?} of the connection closing"
        );
    }
    let _ = subject_event(
        &mut subject,
        &mut relays,
        "the relay to be reserved on again after its backoff",
        |e| {
            matches!(
                e,
                SwarmEvent::RelayReservationChanged {
                    relay,
                    outcome: RelayReservationOutcome::Accepted,
                    addresses,
                    ..
                } if *relay == relay_peer && *addresses == vec![circuit.clone()]
            )
        },
    )
    .await;
    assert_eq!(
        relays.first.1.accepted,
        vec![subject_pid, subject_pid],
        "the relay recorded the second reservation too"
    );
    assert!(
        relays
            .second
            .as_ref()
            .is_some_and(|(_, seen)| seen.established.is_empty()),
        "and the stranger still never saw a connection"
    );

    subject.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn an_authorized_peer_advertising_hop_is_learned_reserved_on_over_its_connection_and_forgotten_when_deauthorized()
 {
    let relay_keys = identity::Keypair::generate_ed25519();
    let relay_peer = identity_of(&relay_keys);
    let mut relay = relay_server(relay_keys);
    let relay_addr = bound(&mut relay).await;
    relay.add_external_address(relay_addr.clone());
    let mut relay_seen = Seen::default();

    // THE CONTROL: trusted like the relay, advertising no hop protocol.
    let bystander_keys = identity::Keypair::generate_ed25519();
    let bystander_peer = identity_of(&bystander_keys);
    let mut bystander = bystander(bystander_keys);
    let bystander_addr = bound(&mut bystander).await;

    // THE SUBJECT: no static relay, learning on, both peers trusted
    // on the data plane.
    let subject_id = ProfileIdentity::generate();
    let subject_peer = subject_id.transport_identity().expect("peer id");
    let subject_pid: PeerId = subject_peer.as_str().parse().expect("a libp2p identity");
    let config = SubstrateConfig {
        relay_client: Some(settings(Vec::new(), true)),
        ..SubstrateConfig::default()
    };
    let mut subject = SwarmRuntime::start(
        &subject_id,
        config,
        data_plane(&[&relay_peer, &bystander_peer]),
    )
    .expect("the runtime starts");
    let circuit = circuit_of(&relay_addr, &relay_peer, &subject_peer);

    // The subject dials both for its own reasons.
    subject
        .dial(relay_peer.clone(), relay_addr.clone())
        .await
        .expect("delivered")
        .expect("admitted");
    subject
        .dial(bystander_peer.clone(), bystander_addr)
        .await
        .expect("delivered")
        .expect("admitted");

    // Identify names the hop protocol; the relay is learned, asked on
    // the next tick, and accepted -- over the connection that already
    // exists, so the relay sees one connection from the subject.
    let bystander_task = tokio::spawn(async move {
        loop {
            let _ = bystander.select_next_some().await;
        }
    });
    let mut relays = Relays {
        first: (&mut relay, &mut relay_seen),
        second: None,
    };
    let (accepted, _, _) = subject_event(
        &mut subject,
        &mut relays,
        "the learned relay to be reserved on",
        |e| {
            matches!(
                e,
                SwarmEvent::RelayReservationChanged {
                    relay,
                    outcome: RelayReservationOutcome::Accepted,
                    ..
                } if *relay == relay_peer
            )
        },
    )
    .await;
    let SwarmEvent::RelayReservationChanged { addresses, .. } = &accepted else {
        unreachable!("matched above");
    };
    assert_eq!(addresses, &vec![circuit.clone()]);
    assert_eq!(relays.first.1.accepted, vec![subject_pid]);
    assert_eq!(
        relays.first.1.established,
        vec![subject_pid],
        "one connection: the reservation rode the existing one, no second dial"
    );
    let events = settle(&mut subject, &mut relays, WINDOW).await;
    assert!(
        !events.iter().any(|e| matches!(
            e,
            SwarmEvent::RelayReservationChanged { relay, .. } if *relay == bystander_peer
        )),
        "a trusted peer without the hop protocol is never a relay: {events:?}"
    );

    // DE-AUTHORIZED: the relay leaves every trust set. A learned relay
    // is forgotten with its address, and the connection it rode goes
    // with the revocation.
    let closed = subject
        .set_trust(data_plane(&[&bystander_peer]))
        .await
        .expect("delivered");
    assert_eq!(closed, 1, "the relay's connection is revoked");
    let (released, _, _) = subject_event(
        &mut subject,
        &mut relays,
        "the learned relay to be forgotten",
        |e| {
            matches!(
                e,
                SwarmEvent::RelayReservationChanged {
                    relay,
                    outcome: RelayReservationOutcome::Released,
                    ..
                } if *relay == relay_peer
            )
        },
    )
    .await;
    let SwarmEvent::RelayReservationChanged {
        addresses, detail, ..
    } = &released
    else {
        unreachable!("matched above");
    };
    assert_eq!(
        addresses,
        &vec![circuit],
        "its address is withdrawn with it"
    );
    assert!(
        detail
            .as_deref()
            .is_some_and(|d| d.contains("no longer authorized")),
        "and the reason is the de-authorization: {detail:?}"
    );
    let events = settle(&mut subject, &mut relays, WINDOW).await;
    assert!(
        !events.iter().any(|e| matches!(
            e,
            SwarmEvent::RelayReservationChanged { relay, .. } if *relay == relay_peer
        )),
        "and it is not asked again: {events:?}"
    );
    assert_eq!(
        relays.first.1.established,
        vec![subject_pid],
        "the relay still saw exactly one connection from the subject"
    );

    bystander_task.abort();
    subject.shutdown().await.expect("shutdown");
}
