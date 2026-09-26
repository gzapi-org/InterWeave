// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `RELAY.md` §8's verified-address gate over real sockets, with the
//! PRODUCTION server field (`relay_server_driver::build_behaviour`) in a
//! bare Swarm and the crate's own relay client as the requester.
//!
//! Why a bare Swarm and not the runtime: the gate opens on a confirmed
//! direct external address, and the runtime's only source of one is the
//! AutoNAT verdict -- which loopback and a private range can never
//! produce (`is_probeable_address` takes a public literal only). Here
//! the Swarm is told the address directly, exactly as the AutoNAT
//! adapter's `publish` tells it, so the gate is driven through the one
//! input it reads. `tests/connectivity/tests/relay_server.rs` pins the
//! runtime's side: retention, and the gate SHUT on loopback.
//!
//! Proved here:
//! - shut with no external address: a reservation is refused as an
//!   unsupported protocol (`ReserveError::Unsupported`), the server
//!   never sees the request, and Identify advertises no hop;
//! - a relay-derived circuit address does not open it;
//! - a direct address opens it: advertised to a client ALREADY
//!   connected with no traffic of its own (Identify's address-change
//!   notification polls the connection; `hop_gate`'s module note), and the reservation granted carries that address --
//!   the control for every refusal above;
//! - expiring it shuts the gate PER REQUEST: the renewal on the
//!   connection opened while it was open is refused, the connection
//!   kept;
//! - with the gate open, the per-peer and global ceilings are exact and
//!   a peer in no trust set is offered nothing.

#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::time::Duration;

use futures::StreamExt as _;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::runtime::relay_server_driver::{
    RelayServerSettings, ServerField, build_behaviour,
};
use interweave_transport_runtime::{ConnectionManager, ConnectionPolicy, TrustSources};
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::multiaddr::Protocol;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{Multiaddr, PeerId, identify, identity, relay};
use tokio::sync::mpsc;

const PATIENCE: Duration = Duration::from_secs(20);
const HOP: &str = "/libp2p/circuit/relay/0.2.0/hop";

#[derive(NetworkBehaviour)]
struct ServerBehaviour {
    identify: identify::Behaviour,
    relay: ServerField,
}

#[derive(NetworkBehaviour)]
struct ClientBehaviour {
    identify: identify::Behaviour,
    relay: relay::client::Behaviour,
}

/// What the test tells the server task to do to its external set.
enum External {
    Add(Multiaddr),
    Remove(Multiaddr),
}

/// A server running on its own task: the external set is changed by
/// command, and every relay event it raises is forwarded.
struct Server {
    peer: PeerId,
    addr: Multiaddr,
    external: mpsc::UnboundedSender<External>,
    events: mpsc::UnboundedReceiver<relay::Event>,
}

impl Server {
    fn circuit_listen(&self) -> Multiaddr {
        self.addr
            .clone()
            .with(Protocol::P2p(self.peer))
            .with(Protocol::P2pCircuit)
    }

    fn set(&self, change: External) {
        self.external
            .send(change)
            .expect("the server task is alive");
    }

    /// Every relay event raised so far.
    fn drain(&mut self) -> Vec<relay::Event> {
        let mut out = Vec::new();
        while let Ok(e) = self.events.try_recv() {
            out.push(e);
        }
        out
    }
}

fn identity_of(keys: &identity::Keypair) -> TransportIdentity {
    TransportIdentity::parse(keys.public().to_peer_id().to_base58()).expect("canonical")
}

async fn server(settings: RelayServerSettings, infra: &[&identity::Keypair]) -> Server {
    let mut manager = ConnectionManager::new(ConnectionPolicy::new(64, 64), 64);
    let _ = manager.set_trust(
        TrustSources::new(
            PeerTrustPolicy::new(std::iter::empty()).expect("an empty allowlist"),
            InfrastructureSet::new(infra.iter().map(|k| identity_of(k))).expect("a small set"),
        ),
        &[],
    );
    let policy = manager.handle();
    let keys = identity::Keypair::generate_ed25519();
    let peer = keys.public().to_peer_id();
    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keys)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("tcp")
        .with_behaviour(|k| ServerBehaviour {
            identify: identify::Behaviour::new(identify::Config::new(
                "/interweave-hop-gate-server/1".to_owned(),
                k.public(),
            )),
            relay: build_behaviour(&settings, k.public().to_peer_id(), policy),
        })
        .expect("behaviour")
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(600)))
        .build();
    swarm
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .expect("listens");
    let addr = loop {
        if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
            break address;
        }
    };
    let (external, mut commands) = mpsc::unbounded_channel();
    let (to_test, events) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        // The manager owns the policy the gate reads; it lives as long
        // as the server does.
        let _manager = manager;
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(External::Add(a)) => swarm.add_external_address(a),
                    Some(External::Remove(a)) => swarm.remove_external_address(&a),
                    None => return,
                },
                event = swarm.select_next_some() => {
                    if let SwarmEvent::Behaviour(ServerBehaviourEvent::Relay(e)) = event {
                        let _ = to_test.send(e);
                    }
                }
            }
        }
    });
    Server {
        peer,
        addr,
        external,
        events,
    }
}

fn client(keys: identity::Keypair) -> libp2p::Swarm<ClientBehaviour> {
    libp2p::SwarmBuilder::with_existing_identity(keys)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("tcp")
        .with_relay_client(libp2p::noise::Config::new, libp2p::yamux::Config::default)
        .expect("relay client")
        .with_behaviour(|k, relay| ClientBehaviour {
            identify: identify::Behaviour::new(identify::Config::new(
                "/interweave-hop-gate-client/1".to_owned(),
                k.public(),
            )),
            relay,
        })
        .expect("behaviour")
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(600)))
        .build()
}

/// What one client saw, in order.
#[derive(Debug, Clone)]
enum Seen {
    Offered(BTreeSet<String>),
    Accepted { renewal: bool },
    ListenAddr(Multiaddr),
    ListenerClosed(String),
    ConnectionClosed,
}

/// Drive `client` until `pred` matches what it saw, or fail naming
/// `what` and everything seen.
async fn until(
    client: &mut libp2p::Swarm<ClientBehaviour>,
    server: PeerId,
    what: &str,
    mut pred: impl FnMut(&Seen) -> bool,
) -> Vec<Seen> {
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let event = tokio::time::timeout_at(deadline, client.select_next_some())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}: {seen:?}"));
        let noted =
            match event {
                SwarmEvent::Behaviour(ClientBehaviourEvent::Identify(
                    identify::Event::Received { peer_id, info, .. },
                )) if peer_id == server => Some(Seen::Offered(
                    info.protocols.iter().map(ToString::to_string).collect(),
                )),
                SwarmEvent::Behaviour(ClientBehaviourEvent::Relay(
                    relay::client::Event::ReservationReqAccepted { renewal, .. },
                )) => Some(Seen::Accepted { renewal }),
                SwarmEvent::NewListenAddr { address, .. } => Some(Seen::ListenAddr(address)),
                SwarmEvent::ListenerClosed { reason, .. } => {
                    Some(Seen::ListenerClosed(format!("{reason:?}")))
                }
                SwarmEvent::ConnectionClosed { peer_id, .. } if peer_id == server => {
                    Some(Seen::ConnectionClosed)
                }
                _ => None,
            };
        if let Some(noted) = noted {
            let hit = pred(&noted);
            seen.push(noted);
            if hit {
                return seen;
            }
        }
    }
}

fn offers_hop(seen: &Seen) -> Option<bool> {
    match seen {
        Seen::Offered(protocols) => Some(protocols.contains(HOP)),
        _ => None,
    }
}

fn refused_unsupported(seen: &Seen) -> bool {
    matches!(seen, Seen::ListenerClosed(r) if r.contains("Unsupported"))
}

fn accepted(events: &[relay::Event]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, relay::Event::ReservationReqAccepted { .. }))
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_relay_offers_hop_only_while_it_holds_a_verified_direct_address() {
    let keys_a = identity::Keypair::generate_ed25519();
    // Four seconds: the client renews at three quarters of it, so the
    // renewal the expired address must refuse arrives within PATIENCE.
    let mut s = server(
        RelayServerSettings {
            reservation_duration_ms: 4_000,
            ..RelayServerSettings::default()
        },
        &[&keys_a],
    )
    .await;
    let mut a = client(keys_a);

    // SHUT: no external address. The first Identify carries no hop, and
    // the reservation fails negotiation.
    a.listen_on(s.circuit_listen()).expect("a circuit listen");
    let seen = until(
        &mut a,
        s.peer,
        "the reservation refused",
        refused_unsupported,
    )
    .await;
    assert!(
        seen.iter().filter_map(offers_hop).all(|hop| !hop),
        "no hop advertised while shut: {seen:?}"
    );
    assert!(
        !seen
            .iter()
            .any(|x| matches!(x, Seen::Accepted { .. } | Seen::ConnectionClosed)),
        "refused, not granted, and the connection kept: {seen:?}"
    );

    // A RELAY-DERIVED ADDRESS DOES NOT OPEN IT: a circuit address is
    // not one a reservation from here can carry.
    let circuit: Multiaddr = format!(
        "/ip4/192.0.2.9/tcp/4001/p2p/{}/p2p-circuit",
        PeerId::random()
    )
    .parse()
    .expect("valid");
    s.set(External::Add(circuit));
    tokio::time::sleep(Duration::from_millis(300)).await;
    a.listen_on(s.circuit_listen()).expect("a circuit listen");
    let seen = until(
        &mut a,
        s.peer,
        "the reservation refused with only a circuit address",
        refused_unsupported,
    )
    .await;
    assert!(
        !seen.iter().any(|x| matches!(x, Seen::Accepted { .. })),
        "{seen:?}"
    );
    assert_eq!(accepted(&s.drain()), 0, "the server saw no request at all");

    // OPEN: a direct address. The client, idle and already connected,
    // is told of hop without doing anything -- Identify's address-change
    // notification polled its connection -- and then reserves, and the grant carries it.
    let direct: Multiaddr = "/ip4/198.51.100.7/tcp/4001".parse().expect("valid");
    s.set(External::Add(direct.clone()));
    until(&mut a, s.peer, "hop advertised after the flip", |x| {
        offers_hop(x) == Some(true)
    })
    .await;
    a.listen_on(s.circuit_listen()).expect("a circuit listen");
    let seen = until(&mut a, s.peer, "the reservation granted", |x| {
        matches!(x, Seen::ListenAddr(addr) if addr.to_string().starts_with(&direct.to_string()))
    })
    .await;
    assert!(
        seen.iter()
            .any(|x| matches!(x, Seen::Accepted { renewal: false })),
        "{seen:?}"
    );

    // SHUT PER REQUEST: the address expires; the renewal on the SAME
    // connection -- opened while the gate was open -- is refused.
    s.set(External::Remove(direct));
    let seen = until(&mut a, s.peer, "the renewal refused", refused_unsupported).await;
    assert!(
        !seen
            .iter()
            .any(|x| matches!(x, Seen::Accepted { renewal: true })),
        "no renewal granted: {seen:?}"
    );
    assert!(
        !seen.iter().any(|x| matches!(x, Seen::ConnectionClosed)),
        "the gate refuses the request and keeps the connection: {seen:?}"
    );
    assert_eq!(
        accepted(&s.drain()),
        1,
        "the one grant, while open; no renewal reached the server"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_the_gate_open_the_ceilings_are_exact_and_a_stranger_is_offered_nothing() {
    let keys_a = identity::Keypair::generate_ed25519();
    let keys_b = identity::Keypair::generate_ed25519();
    let keys_c = identity::Keypair::generate_ed25519();
    let mut s = server(
        RelayServerSettings {
            max_reservations: 2,
            max_reservations_per_peer: 1,
            ..RelayServerSettings::default()
        },
        &[&keys_a, &keys_b, &keys_c],
    )
    .await;
    s.set(External::Add(
        "/ip4/198.51.100.7/tcp/4001".parse().expect("valid"),
    ));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let granted = |x: &Seen| matches!(x, Seen::Accepted { .. });
    let limited =
        |x: &Seen| matches!(x, Seen::ListenerClosed(r) if r.contains("ResourceLimitExceeded"));

    // A reserves: the control for every denial below.
    let mut a = client(keys_a.clone());
    a.listen_on(s.circuit_listen()).expect("listen");
    until(&mut a, s.peer, "A granted", granted).await;

    // THE PER-PEER CEILING IS EXACT: the same PeerId on a second
    // connection is denied, which the crate's own `>` would admit.
    let mut a2 = client(keys_a);
    a2.listen_on(s.circuit_listen()).expect("listen");
    until(&mut a2, s.peer, "A's second denied", limited).await;

    // B takes the second of two; C, authorized, is denied the global.
    let mut b = client(keys_b);
    b.listen_on(s.circuit_listen()).expect("listen");
    until(&mut b, s.peer, "B granted", granted).await;
    let mut c = client(keys_c);
    c.listen_on(s.circuit_listen()).expect("listen");
    until(&mut c, s.peer, "C denied the global ceiling", limited).await;

    // A STRANGER, gate open: the class gate offers it nothing, so its
    // request fails negotiation exactly as a shut gate's does.
    let mut d = client(identity::Keypair::generate_ed25519());
    d.listen_on(s.circuit_listen()).expect("listen");
    let seen = until(&mut d, s.peer, "the stranger refused", refused_unsupported).await;
    assert!(
        seen.iter().filter_map(offers_hop).all(|hop| !hop),
        "{seen:?}"
    );
    assert_eq!(
        accepted(&s.drain()),
        2,
        "A and B, and nobody else, were granted"
    );
}
