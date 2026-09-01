// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! The PRODUCTION gate, unmodified, with a relay client behind it.
//!
//! Everything else in this harness runs [`crate::gate::InstrumentedGate`]
//! — a gate shaped the way Stage 11 would have to build one, with
//! attribution. That is a proposal, and measuring a proposal proves
//! only that the proposal works.
//!
//! This module measures the thing that ships. `OutboundAdmission` is
//! taken from `interweave-transport-libp2p` by path, unchanged, and put
//! in front of a real `relay::client::Behaviour`. Whatever it does to
//! the reservation dial is what production does today.
//!
//! F1 says the answer is "refuses it", and says so from reading:
//! the pending hook builds its `DialRequest` with
//! `origin: DialOrigin::KademliaQuery` because Kademlia is the only
//! behaviour that can currently dial, `KademliaQuery.is_data_plane()`
//! is true, and `ConnectionPolicy::admit` refuses a data-plane origin
//! for a `ConnectivityInfrastructureOnly` peer. R6 is that chain run
//! rather than argued.

use std::time::Duration;

use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::OutboundAdmission;
use interweave_transport_libp2p::outbound_gate::InFlightTickets;
use interweave_transport_runtime::{ConnectionManager, ConnectionPolicy, TrustSources};
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::swarm::NetworkBehaviour;
use libp2p::{Multiaddr, PeerId, Swarm, identify, identity, noise, relay, tcp, yamux};

/// Identify plus a relay client, behind the production gate.
#[derive(NetworkBehaviour)]
pub struct ProductionBehaviour {
    /// FIRST, exactly as `SubstrateBehaviour` orders it.
    pub outbound: OutboundAdmission,
    pub identify: identify::Behaviour,
    pub relay_client: relay::client::Behaviour,
}

/// A node running the shipped gate.
pub struct ProductionNode {
    pub swarm: Swarm<ProductionBehaviour>,
    pub peer_id: PeerId,
    pub identity: TransportIdentity,
    /// Dial failures seen, by rendering — where the refusal shows up.
    pub failures: Vec<String>,
    /// Dial attempts the Swarm actually started.
    pub dialing: usize,
    /// Everything else, for diagnosis.
    pub other: Vec<String>,
    pub connected: usize,
}

impl ProductionNode {
    /// Build one whose only authorization for `relay` is
    /// infrastructure, which is what a relay is.
    #[must_use]
    pub fn new(infrastructure: &[TransportIdentity]) -> Self {
        Self::with_trust(&[], infrastructure)
    }

    /// The same node with an explicit data-plane allowlist, for the
    /// control: identical in every way except the class the relay is
    /// authorized under.
    #[must_use]
    pub fn with_trust(
        data_plane: &[TransportIdentity],
        infrastructure: &[TransportIdentity],
    ) -> Self {
        let keypair = identity::Keypair::generate_ed25519();
        let peer_id = PeerId::from_public_key(&keypair.public());
        let identity_str = TransportIdentity::parse(peer_id.to_base58()).expect("canonical");

        let mut manager = ConnectionManager::new(ConnectionPolicy::new(32, 32), 32);
        let _ = manager.set_trust(
            TrustSources::new(
                PeerTrustPolicy::new(data_plane.iter().cloned()).expect("small allowlist"),
                InfrastructureSet::new(infrastructure.iter().cloned()).expect("small"),
            ),
            &[],
        );
        let outbound = OutboundAdmission::new(
            manager.handle(),
            InFlightTickets::default(),
            tokio::time::Instant::now(),
        );

        let swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                tcp::Config::default().nodelay(true),
                noise::Config::new,
                yamux::Config::default,
            )
            .expect("tcp")
            .with_relay_client(noise::Config::new, yamux::Config::default)
            .expect("relay client transport")
            .with_behaviour(|key, relay_client| ProductionBehaviour {
                outbound,
                identify: identify::Behaviour::new(identify::Config::new(
                    "/interweave-spike/1.0.0".to_owned(),
                    key.public(),
                )),
                relay_client,
            })
            .expect("behaviour")
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(30)))
            .build();

        Self {
            swarm,
            peer_id,
            identity: identity_str,
            failures: Vec::new(),
            connected: 0,
            dialing: 0,
            other: Vec::new(),
        }
    }

    /// Listen on loopback and return the bound address.
    pub async fn listen(&mut self) -> Multiaddr {
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/0".parse().expect("loopback");
        self.swarm.listen_on(addr).expect("listen accepted");
        loop {
            if let libp2p::swarm::SwarmEvent::NewListenAddr { address, .. } =
                futures::StreamExt::select_next_some(&mut self.swarm).await
            {
                return address;
            }
        }
    }

    /// Drain whatever the swarm has ready, recording refusals.
    pub fn drain(&mut self, cx: &mut std::task::Context<'_>) {
        use futures::StreamExt;
        while let std::task::Poll::Ready(Some(event)) = self.swarm.poll_next_unpin(cx) {
            match event {
                libp2p::swarm::SwarmEvent::OutgoingConnectionError { error, .. } => {
                    self.failures.push(format!("{error}"));
                }
                libp2p::swarm::SwarmEvent::ConnectionEstablished { .. } => {
                    self.connected += 1;
                }
                libp2p::swarm::SwarmEvent::Dialing { .. } => {
                    self.dialing += 1;
                }
                libp2p::swarm::SwarmEvent::ListenerError { error, .. } => {
                    self.failures.push(format!("listener: {error}"));
                }
                libp2p::swarm::SwarmEvent::ListenerClosed { reason, .. } => {
                    self.failures.push(format!("listener closed: {reason:?}"));
                }
                other => {
                    self.other.push(format!("{other:?}"));
                }
            }
        }
    }
}
