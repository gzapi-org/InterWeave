// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Stage 11 step 6: the relay SERVER over real sockets, as the runtime
//! runs it -- and, since `RELAY.md` §8's rule of 2026-09-26, as it runs
//! on a host that can verify no address of its own.
//!
//! What is proved here, with the production runtime as the relay and
//! bare relay clients as the requesters:
//!
//! - an authorized infrastructure-only client's inbound is retained
//!   (route 3, widened under `RelayReservation`) and kept;
//! - with no verified direct address -- which loopback is, since
//!   `AUTONAT.md` §6 refuses a loopback candidate -- the hop gate is
//!   SHUT: the retained connection is offered Identify and the
//!   keepalive's ping and nothing else,
//!   a reservation is refused as an unsupported protocol, a circuit
//!   request likewise, and the subject serves nobody anything;
//! - a peer in no trust set is closed at establishment as before (the
//!   negative control);
//! - a DUAL-ROLE subject -- a relay client holding a reservation on an
//!   upstream relay, and a relay server -- is not opened by its
//!   relay-derived address: a circuit through a circuit is no address a
//!   reservation from it can carry.
//!
//! The OPEN gate -- a reservation carrying the verified address, the
//! per-peer and global ceilings exact, a renewal refused on an open
//! connection once the address expires -- is proved at the crate
//! surface by `crates/transport/libp2p/tests/relay_hop_gate.rs`, with
//! the production server field told its address directly, the way the
//! AutoNAT adapter tells it: the runtime can be given one only by a
//! verdict, and no loopback or private-range run yields one. SPIKE-004
//! phase B's node rows are where the runtime relay serves with one.
//!
//! So on loopback no test observes the runtime's relay-server event path
//! -- a `RelayServed` acceptance, denial or circuit event -- or its
//! switching of the relay keepalive toward reservation holders
//! (`relay_server_driver::Reserved`): both need a grant, which needs the
//! gate open. The crate tests cover the field and the keepalive in bare
//! Swarms, `Reserved` its own unit tests, and SPIKE-004 phase B's
//! `ifchange` row the runtime end to end (#129 review F7).

#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::time::Duration;

use futures::StreamExt as _;
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::runtime::relay_driver::{RelayClientSettings, StaticRelay};
use interweave_transport_libp2p::runtime::relay_server_driver::RelayServerSettings;
use interweave_transport_libp2p::{
    RelayReservationOutcome, RelayServerOutcome, SubstrateConfig, SwarmEvent, SwarmRuntime,
};
use interweave_transport_runtime::TrustSources;
use interweave_transport_runtime::relay::ReservationConfig;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::multiaddr::Protocol;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent as Libp2pSwarmEvent};
use libp2p::{Multiaddr, PeerId, identify, identity, relay};

const PATIENCE: Duration = Duration::from_secs(20);
const WINDOW: Duration = Duration::from_secs(3);

/// What a retained infrastructure-only client is offered while the hop
/// gate is shut: Identify and the keepalive's ping, which every
/// relay-configured profile answers (`relay_keepalive`), and nothing
/// else.
const OFFERED_TO_A_CLIENT: &[&str] = &["/ipfs/id/1.0.0", "/ipfs/id/push/1.0.0", "/ipfs/ping/1.0.0"];

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
    /// The circuit addresses this client's reservation listener
    /// reported: the subject's external set as a circuit through it.
    listen_addrs: Vec<String>,
    /// Why this client's dials failed.
    dial_errors: Vec<String>,
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
        Libp2pSwarmEvent::NewListenAddr { address, .. } => {
            seen.listen_addrs.push(address.to_string());
        }
        Libp2pSwarmEvent::OutgoingConnectionError { error, .. } => {
            seen.dial_errors.push(format!("{error:?}"));
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

/// Every `RelayServed` the subject raised, whoever it names.
fn any_served(events: &[SwarmEvent]) -> Vec<&SwarmEvent> {
    events
        .iter()
        .filter(|e| matches!(e, SwarmEvent::RelayServed { .. }))
        .collect()
}

fn refused_unsupported(seen: &Seen) -> bool {
    seen.listeners_closed
        .iter()
        .any(|r| r.contains("Unsupported"))
}

#[tokio::test]
async fn the_relay_server_retains_authorized_peers_and_serves_nobody_without_a_verified_address() {
    // THE SUBJECT: the production runtime as a relay, two peers
    // authorized infrastructure-only, one stranger.
    let keys_a = identity::Keypair::generate_ed25519();
    let keys_b = identity::Keypair::generate_ed25519();
    let keys_d = identity::Keypair::generate_ed25519();
    let (peer_a, peer_b, peer_d) = (
        identity_of(&keys_a),
        identity_of(&keys_b),
        identity_of(&keys_d),
    );
    let subject_id = ProfileIdentity::generate();
    let subject_peer = subject_id.transport_identity().expect("peer id");
    let subject_pid: PeerId = subject_peer.as_str().parse().expect("a libp2p identity");
    let config = SubstrateConfig {
        relay_server: Some(RelayServerSettings::default()),
        ..SubstrateConfig::default()
    };
    let mut subject = SwarmRuntime::start(
        &subject_id,
        config,
        infrastructure_only(&[&peer_a, &peer_b]),
    )
    .expect("the runtime starts");
    let subject_addr = subject
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("a listen address"))
        .await
        .expect("the subject listens");

    // A ASKS FOR A RESERVATION. Nobody dialled the subject for it: the
    // client's behaviour dials the relay, the subject sees an inbound
    // from an infrastructure-only peer and RETAINS it -- and, holding no
    // verified address, refuses the hop stream.
    let mut a = client(keys_a);
    let mut seen_a = Seen::default();
    reserve_on(&mut a, &subject_addr, subject_pid);
    let mut all = drive(
        &mut subject,
        subject_pid,
        &mut [(&mut a, &mut seen_a)],
        "A's inbound to be retained",
        PATIENCE,
        Some(|e: &SwarmEvent| matches!(e, SwarmEvent::Connected { peer, .. } if *peer == peer_a)),
    )
    .await;
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
    assert!(
        refused_unsupported(&seen_a),
        "A's reservation refused as an unsupported protocol: {seen_a:?}"
    );
    assert_eq!(seen_a.accepted, 0, "and never granted: {seen_a:?}");
    let expected: BTreeSet<String> = OFFERED_TO_A_CLIENT
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    assert_eq!(
        seen_a.offered.as_ref(),
        Some(&expected),
        "the retained infrastructure-only inbound is offered Identify and ping and nothing else while \
         the gate is shut"
    );
    assert_eq!(seen_a.connections_closed, 0, "and the connection stays");

    // A CIRCUIT REQUEST is refused the same way: B asks for A through
    // the subject, and CONNECT rides the same hop stream.
    let mut b = client(keys_b);
    let mut seen_b = Seen::default();
    let circuit: Multiaddr = subject_addr
        .clone()
        .with(Protocol::P2p(subject_pid))
        .with(Protocol::P2pCircuit)
        .with(Protocol::P2p(a.local_peer_id().to_owned()));
    b.dial(circuit)
        .expect("a circuit dial is accepted by the transport");
    all.extend(
        drive::<fn(&SwarmEvent) -> bool>(
            &mut subject,
            subject_pid,
            &mut [(&mut a, &mut seen_a), (&mut b, &mut seen_b)],
            "settling",
            WINDOW,
            None,
        )
        .await,
    );
    assert!(
        seen_b.dial_errors.iter().any(|e| e.contains("Unsupported")),
        "B's circuit refused as an unsupported protocol: {seen_b:?}"
    );

    // THE NEGATIVE CONTROL: a peer in no trust set. Its inbound is
    // established and closed as it always was.
    let mut d = client(keys_d);
    let mut seen_d = Seen::default();
    reserve_on(&mut d, &subject_addr, subject_pid);
    let events = drive::<fn(&SwarmEvent) -> bool>(
        &mut subject,
        subject_pid,
        &mut [
            (&mut a, &mut seen_a),
            (&mut b, &mut seen_b),
            (&mut d, &mut seen_d),
        ],
        "settling",
        WINDOW,
        None,
    )
    .await;
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SwarmEvent::Connected { peer, .. } if *peer == peer_d)),
        "the stranger is never announced: {events:?}"
    );
    assert!(
        seen_d.connections_closed >= 1,
        "D saw its connection closed: {seen_d:?}"
    );
    all.extend(events);

    // THE SUBJECT SERVED NOBODY ANYTHING -- no acceptance, no denial, no
    // circuit event: every request failed negotiation before the crate
    // saw it. And the authorized clients kept their connections.
    assert!(any_served(&all).is_empty(), "{:?}", any_served(&all));
    assert_eq!(seen_a.connections_closed + seen_b.connections_closed, 0);

    subject.shutdown().await.expect("shutdown");
}

#[derive(NetworkBehaviour)]
struct UpstreamBehaviour {
    identify: identify::Behaviour,
    relay: relay::Behaviour,
}

/// A bare relay the dual-role subject reserves on: scenery, driven on
/// its own task and observed by nobody -- what it hands the subject is
/// read from the subject's own event.
async fn upstream_relay() -> (TransportIdentity, Multiaddr) {
    let keys = identity::Keypair::generate_ed25519();
    let peer = identity_of(&keys);
    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keys)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("the same transport stack the subject uses")
        .with_behaviour(|k| UpstreamBehaviour {
            identify: identify::Behaviour::new(identify::Config::new(
                "/interweave-relay-test-upstream/1".to_owned(),
                k.public(),
            )),
            relay: relay::Behaviour::new(k.public().to_peer_id(), relay::Config::default()),
        })
        .expect("behaviour")
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(600)))
        .build();
    swarm
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().expect("a listen address"))
        .expect("listens");
    let address = loop {
        if let Libp2pSwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
            break address;
        }
    };
    // ADVERTISING what it bound (SPIKE-004 note 10): its reservations
    // carry an address, so the subject holds a relay-derived one.
    swarm.add_external_address(address.clone());
    tokio::spawn(async move {
        loop {
            let _ = swarm.select_next_some().await;
        }
    });
    (peer, address)
}

#[tokio::test]
async fn a_dual_role_relays_derived_address_does_not_open_its_hop_gate() {
    let (upstream_peer, upstream_addr) = upstream_relay().await;
    let keys_a = identity::Keypair::generate_ed25519();
    let peer_a = identity_of(&keys_a);
    let subject_id = ProfileIdentity::generate();
    let subject_peer = subject_id.transport_identity().expect("peer id");
    let subject_pid: PeerId = subject_peer.as_str().parse().expect("a libp2p identity");

    // THE SUBJECT, in both roles: reserving on the upstream relay, and
    // serving `a`.
    let config = SubstrateConfig {
        relay_client: Some(RelayClientSettings {
            static_relays: vec![StaticRelay {
                peer: upstream_peer.clone(),
                address: format!("{upstream_addr}/p2p/{}", upstream_peer.as_str()),
            }],
            use_authorized_identify_relays: false,
            reservations: ReservationConfig::default(),
            direct_head_start_ms: 750,
        }),
        relay_server: Some(RelayServerSettings::default()),
        ..SubstrateConfig::default()
    };
    let mut subject = SwarmRuntime::start(
        &subject_id,
        config,
        infrastructure_only(&[&upstream_peer, &peer_a]),
    )
    .expect("the runtime starts");
    let subject_addr = subject
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("a listen address"))
        .await
        .expect("the subject listens");

    // THE SUBJECT HOLDS A RELAY-DERIVED ADDRESS: its own reservation is
    // accepted and advertised, so the Swarm's external set has one
    // circuit address in it -- the thing the server must not hand on.
    let events = drive(
        &mut subject,
        subject_pid,
        &mut [],
        "the subject's reservation on the upstream relay",
        PATIENCE,
        Some(|e: &SwarmEvent| {
            matches!(
                e,
                SwarmEvent::RelayReservationChanged {
                    outcome: RelayReservationOutcome::Accepted,
                    ..
                }
            )
        }),
    )
    .await;
    let Some(SwarmEvent::RelayReservationChanged { addresses, .. }) = events.last() else {
        unreachable!("matched above");
    };
    assert_eq!(
        addresses.len(),
        1,
        "one relay-derived address: {addresses:?}"
    );
    assert!(addresses[0].contains("/p2p-circuit/"));

    // `a` ASKS THE SUBJECT FOR A RESERVATION. The subject holds a
    // relay-derived address and no verified direct one on loopback: the
    // relay-derived one does not open the hop gate, so the request is
    // refused and `a` is handed no address -- above all no circuit
    // through a circuit.
    let mut a = client(keys_a);
    let mut seen_a = Seen::default();
    reserve_on(&mut a, &subject_addr, subject_pid);
    let events = drive::<fn(&SwarmEvent) -> bool>(
        &mut subject,
        subject_pid,
        &mut [(&mut a, &mut seen_a)],
        "settling",
        WINDOW + WINDOW,
        None,
    )
    .await;
    assert!(
        refused_unsupported(&seen_a),
        "a's reservation refused as an unsupported protocol: {seen_a:?}"
    );
    assert!(
        seen_a.listen_addrs.is_empty(),
        "a was handed no address: {:?}",
        seen_a.listen_addrs
    );
    assert!(
        seen_a
            .offered
            .as_ref()
            .is_some_and(|o| !o.iter().any(|p| p.ends_with("/relay/0.2.0/hop"))),
        "and the subject offered it no hop: {:?}",
        seen_a.offered
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            SwarmEvent::RelayServed {
                outcome: RelayServerOutcome::ReservationAccepted,
                ..
            }
        )),
        "{events:?}"
    );

    subject.shutdown().await.expect("shutdown");
}
