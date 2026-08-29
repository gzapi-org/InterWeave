// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! One spike node, and the topology helpers the experiments share.
//!
//! The behaviour is configured exactly as `kademlia-integration.md` §11
//! maps the project's settings onto `kad::Config`. The point of the
//! spike is that the mapping is checked against the real crate rather
//! than against the research note that proposed it.

use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::time::Duration;

use libp2p::identity::Keypair;
use libp2p::kad::store::MemoryStore;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{Multiaddr, PeerId, Swarm, StreamProtocol, identify, kad, noise, tcp, yamux};
use libp2p::swarm::behaviour::toggle::Toggle;

use crate::gate::{InstrumentedGate, Mode};
use interweave_transport_runtime::ConnectionPolicy;

/// The identify protocol this spike advertises, so a node's Kademlia
/// mode is observable the way the design says it must be: through an
/// authenticated Identify exchange, not by assumption.
const IDENTIFY_PROTOCOL: &str = "/interweave-spike/id/1.0.0";

#[derive(NetworkBehaviour)]
pub struct SpikeBehaviour {
    /// FIRST, so it sees every outbound dial before anything else can
    /// act on it — the same position `OutboundAdmission` holds in the
    /// production `SubstrateBehaviour`.
    pub gate: InstrumentedGate,
    pub identify: identify::Behaviour,
    /// Behind a `Toggle`, because "no Kademlia activity when disabled"
    /// is one of the observations, and a disabled behaviour that is
    /// still constructed proves nothing about a build that omits it.
    pub kad: Toggle<kad::Behaviour<MemoryStore>>,
}

/// What a node is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KadRole {
    Disabled,
    Client,
    Server,
}

/// Everything an experiment can vary about a node.
#[derive(Debug, Clone)]
pub struct NodeConfig {
    pub role: KadRole,
    pub network_id: String,
    pub gate_mode: Mode,
    pub kbucket_size: NonZeroUsize,
    pub parallelism: NonZeroUsize,
    pub query_timeout: Duration,
    pub disjoint_paths: bool,
    /// `None` is what the design requires: the provider scheduler owns
    /// the refresh, so the library must not run its own.
    pub periodic_bootstrap: Option<Duration>,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            role: KadRole::Client,
            network_id: "spike-003".to_owned(),
            gate_mode: Mode::PolicyAdmit,
            kbucket_size: NonZeroUsize::new(20).expect("nonzero"),
            parallelism: NonZeroUsize::new(3).expect("nonzero"),
            query_timeout: Duration::from_secs(10),
            disjoint_paths: true,
            periodic_bootstrap: None,
        }
    }
}

/// A running node: its swarm, its identity, and the counters its gate
/// writes.
pub struct Node {
    pub swarm: Swarm<SpikeBehaviour>,
    pub peer_id: PeerId,
    pub listen: Multiaddr,
    pub ledger: crate::gate::DialLedger,
    pub admitted: interweave_transport_libp2p::outbound_gate::AdmittedDials,
    pub policy: std::sync::Arc<std::sync::Mutex<ConnectionPolicy>>,
    pub trusted: std::sync::Arc<std::sync::Mutex<Vec<PeerId>>>,
    /// Query ids this node started deliberately. Anything else the
    /// library ran is unattributed work, which the brief requires be
    /// counted rather than assumed absent.
    pub own_queries: HashSet<kad::QueryId>,
    /// What this node saw, written only by `topology::record`.
    pub observed: crate::topology::Observations,
}

impl Node {
    /// Build and start listening on an ephemeral loopback port.
    ///
    /// # Panics
    /// On any transport construction failure: a spike that cannot build
    /// a node has nothing to measure and should say so loudly.
    pub async fn start(config: &NodeConfig) -> Self {
        let keypair = Keypair::generate_ed25519();
        let peer_id = keypair.public().to_peer_id();
        let policy = ConnectionPolicy::new(64, 64);

        let cfg = config.clone();
        let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                tcp::Config::default().nodelay(true),
                noise::Config::new,
                yamux::Config::default,
            )
            .expect("tcp transport")
            .with_behaviour(move |key| build_behaviour(key, &cfg, policy))
            .expect("behaviour")
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(30)))
            .build();

        let gate = &swarm.behaviour().gate;
        let ledger = gate.ledger();
        let admitted = gate.admitted();
        let policy_handle = gate.policy();
        let trusted = gate.trusted();

        swarm
            .listen_on("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
            .expect("listen");

        // Wait for the listen address, which is the only way to know
        // where to point the other nodes.
        let listen = loop {
            if let SwarmEvent::NewListenAddr { address, .. } =
                futures::StreamExt::select_next_some(&mut swarm).await
            {
                break address;
            }
        };

        Self {
            swarm,
            peer_id,
            listen,
            ledger,
            admitted,
            policy: policy_handle,
            trusted,
            own_queries: HashSet::new(),
            observed: crate::topology::Observations::default(),
        }
    }

    /// The address another node dials to reach this one.
    #[must_use]
    pub fn dial_address(&self) -> Multiaddr {
        self.listen.clone()
    }

    /// Mark `peer` data-plane trusted for this node's gate AND policy.
    pub fn trust(&self, peer: PeerId) {
        self.trusted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(peer);
    }

    /// Dial through the ROOT admission: register the ticket the way
    /// `GatedSwarm::dial` does, so the gate sees an admitted dial.
    ///
    /// # Panics
    /// If the dial cannot be started at all.
    pub fn dial_admitted(&mut self, address: Multiaddr) {
        use libp2p::swarm::dial_opts::DialOpts;
        let opts: DialOpts = address.into();
        self.admitted.register(opts.connection_id());
        if self.swarm.dial(opts).is_err() {
            // The id would otherwise stay registered and be reusable by
            // a later dial that carries no admission.
            self.admitted.forget(libp2p::swarm::ConnectionId::new_unchecked(0));
        }
    }

    /// The Kademlia behaviour, when this node has one.
    pub fn kad(&mut self) -> Option<&mut kad::Behaviour<MemoryStore>> {
        self.swarm.behaviour_mut().kad.as_mut()
    }

    /// How many peers are in the routing table.
    pub fn routing_peers(&mut self) -> usize {
        self.swarm
            .behaviour_mut()
            .kad
            .as_mut()
            .map_or(0, |k| k.kbuckets().map(|b| b.num_entries()).sum())
    }
}

fn build_behaviour(
    key: &Keypair,
    config: &NodeConfig,
    policy: ConnectionPolicy,
) -> SpikeBehaviour {
    let identify = identify::Behaviour::new(identify::Config::new(
        IDENTIFY_PROTOCOL.to_owned(),
        key.public(),
    ));

    let kad = match config.role {
        KadRole::Disabled => Toggle::from(None),
        KadRole::Client | KadRole::Server => {
            let protocol = StreamProtocol::try_from_owned(crate::namespace::protocol_name(
                &config.network_id,
            ))
            .expect("derived protocol is valid");
            let mut kc = kad::Config::new(protocol);
            // THE MAPPING FROM §11, applied verbatim so the spike checks
            // the research note's proposal against the real crate.
            kc.set_kbucket_inserts(kad::BucketInserts::Manual);
            kc.set_kbucket_size(config.kbucket_size);
            kc.set_query_timeout(config.query_timeout);
            kc.set_parallelism(config.parallelism);
            kc.disjoint_query_paths(config.disjoint_paths);
            kc.set_periodic_bootstrap_interval(config.periodic_bootstrap);
            kc.set_caching(kad::Caching::Disabled);
            kc.set_record_filtering(kad::StoreInserts::FilterBoth);
            kc.set_publication_interval(None);
            kc.set_replication_interval(None);
            kc.set_provider_publication_interval(None);

            let mut b = kad::Behaviour::with_config(
                key.public().to_peer_id(),
                MemoryStore::new(key.public().to_peer_id()),
                kc,
            );
            b.set_mode(Some(match config.role {
                KadRole::Server => kad::Mode::Server,
                _ => kad::Mode::Client,
            }));
            Toggle::from(Some(b))
        }
    };

    SpikeBehaviour {
        gate: InstrumentedGate::new(config.gate_mode, policy),
        identify,
        kad,
    }
}
