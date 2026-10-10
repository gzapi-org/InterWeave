// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The relay the relayed-path suites put between two daemons: the
//! PRODUCTION relay server field (`relay_server_driver::build_behaviour`,
//! `RELAY.md` §8's defaults) in a bare Swarm on loopback, told its own
//! listen address as its external address the way the AutoNAT adapter's
//! `publish` tells the runtime's Swarm a verdict. The field itself is
//! unchanged: its classifier admits only the peers it is given, as
//! infrastructure, and the hop gate reads the external set as it does in
//! a daemon. What a daemon puts AROUND the field is absent here -- its
//! inbound retention arm, the relay keepalive and pre-auth admission --
//! so nothing of those is exercised on the relay's side.
//!
//! Why not a daemon configured as the relay: `RELAY.md` §8 offers hop
//! only while the relay holds a VERIFIED direct address, a daemon's only
//! source of one is the AutoNAT verdict, and `is_probeable_address` takes
//! a public literal alone -- so on one host a daemon relay keeps its gate
//! shut, as `tests/connectivity/tests/relay_server.rs` pins.
//! `crates/transport/libp2p/tests/relay_hop_gate.rs` pins the gate this
//! field carries; what this relay therefore does NOT prove is a daemon
//! acting as the relay, or any NAT.

// A harness helper panics when the case cannot be set up -- that is its
// failure, by name -- and its value is always used by the case.
#![allow(
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    reason = "test harness: every helper panics on a broken setup"
)]

use std::time::Duration;

use futures::StreamExt as _;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::runtime::relay_server_driver::{
    RelayServerSettings, ServerField, build_behaviour,
};
use interweave_transport_runtime::{ConnectionManager, ConnectionPolicy, TrustSources};
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{Multiaddr, PeerId, identify, identity, relay};
use tokio::sync::{mpsc, oneshot};

use super::PATIENCE;

#[derive(NetworkBehaviour)]
struct RelayBehaviour {
    identify: identify::Behaviour,
    relay: ServerField,
}

/// What the relay did, as its server field reported it.
#[derive(Debug, Clone, Default)]
pub struct RelaySeen {
    /// Reservations granted, by the peer that reserved.
    pub reservations: Vec<PeerId>,
    /// Circuits accepted, as (source, destination).
    pub circuits: Vec<(PeerId, PeerId)>,
    /// Circuit requests denied, as (source, destination).
    pub denied: Vec<(PeerId, PeerId)>,
}

/// A running relay; dropping it stops it.
pub struct Relay {
    /// Its `PeerId`.
    pub peer: TransportIdentity,
    /// Where it listens, ending in `/p2p/<peer>`: the `static_relays`
    /// entry a daemon is given.
    pub address: String,
    seen: mpsc::UnboundedSender<oneshot::Sender<RelaySeen>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Relay {
    /// Serve `clients` -- the daemons, as infrastructure clients of this
    /// relay -- on loopback. Call from inside a tokio runtime.
    pub async fn start(clients: &[&TransportIdentity]) -> Self {
        let mut manager = ConnectionManager::new(ConnectionPolicy::new(64, 64), 64);
        let _ = manager.set_trust(
            TrustSources::new(
                PeerTrustPolicy::new(std::iter::empty()).expect("an empty allowlist"),
                InfrastructureSet::new(clients.iter().map(|c| (*c).clone())).expect("a small set"),
            ),
            &[],
        );
        let policy = manager.handle();
        let keys = identity::Keypair::generate_ed25519();
        let local = keys.public().to_peer_id();
        let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keys)
            .with_tokio()
            .with_tcp(
                libp2p::tcp::Config::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )
            .expect("the daemons' transport stack")
            .with_behaviour(|k| RelayBehaviour {
                identify: identify::Behaviour::new(identify::Config::new(
                    "/interweave-desktop-e2e-relay/1".to_owned(),
                    k.public(),
                )),
                relay: build_behaviour(
                    &RelayServerSettings::default(),
                    k.public().to_peer_id(),
                    policy,
                ),
            })
            .expect("the behaviour")
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(600)))
            .build();
        swarm
            .listen_on("/ip4/127.0.0.1/tcp/0".parse().expect("an address"))
            .expect("listens");
        let listen: Multiaddr = tokio::time::timeout(PATIENCE, async {
            loop {
                if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
                    return address;
                }
            }
        })
        .await
        .expect("the relay's listener bound in time");
        // The verdict a daemon's AutoNAT adapter would publish: the one
        // input the hop gate reads.
        swarm.add_external_address(listen.clone());
        let (seen, mut asks) = mpsc::unbounded_channel::<oneshot::Sender<RelaySeen>>();
        let task = tokio::spawn(async move {
            // The manager owns the policy the classifier reads; it lives
            // as long as the relay does.
            let _manager = manager;
            let mut record = RelaySeen::default();
            loop {
                tokio::select! {
                    ask = asks.recv() => match ask {
                        Some(reply) => { let _ = reply.send(record.clone()); }
                        None => return,
                    },
                    event = swarm.select_next_some() => {
                        if let SwarmEvent::Behaviour(RelayBehaviourEvent::Relay(e)) = event {
                            note(&mut record, &e);
                        }
                    }
                }
            }
        });
        let peer = TransportIdentity::parse(local.to_base58()).expect("canonical");
        Self {
            address: format!("{listen}/p2p/{local}"),
            peer,
            seen,
            task,
        }
    }

    /// What the relay has done so far.
    pub async fn seen(&self) -> RelaySeen {
        let (reply, answer) = oneshot::channel();
        self.seen.send(reply).expect("the relay task is alive");
        answer.await.expect("the relay task answers")
    }

    /// The circuit address through this relay to `target`: the route a
    /// daemon is given when the circuit must be the only one it knows.
    pub fn circuit_to(&self, target: &TransportIdentity) -> String {
        format!("{}/p2p-circuit/p2p/{}", self.address, target.as_str())
    }
}

fn note(record: &mut RelaySeen, event: &relay::Event) {
    match event {
        relay::Event::ReservationReqAccepted { src_peer_id, .. } => {
            record.reservations.push(*src_peer_id);
        }
        relay::Event::CircuitReqAccepted {
            src_peer_id,
            dst_peer_id,
        } => record.circuits.push((*src_peer_id, *dst_peer_id)),
        relay::Event::CircuitReqDenied {
            src_peer_id,
            dst_peer_id,
            ..
        } => record.denied.push((*src_peer_id, *dst_peer_id)),
        _ => {}
    }
}
