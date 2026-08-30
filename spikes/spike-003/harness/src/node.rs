// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! One spike node, and the topology helpers the experiments share.
//!
//! The behaviour is configured exactly as `kademlia-integration.md` §11
//! maps the project's settings onto `kad::Config`. The point of the
//! spike is that the mapping is checked against the real crate rather
//! than against the research note that proposed it.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::time::Duration;

use libp2p::identity::Keypair;
use libp2p::kad::store::MemoryStore;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{Multiaddr, PeerId, Swarm, StreamProtocol, identify, kad, noise, tcp, yamux};
use libp2p::swarm::behaviour::toggle::Toggle;

use crate::gate::{InstrumentedGate, Mode};
use interweave_transport_runtime::{
    ConnectionManager, ConnectionPolicy, SnapshotHandle, TrustSources,
};
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};

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

/// The three query classes §9 names, which share the global budgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum QueryClass {
    Bootstrap,
    Targeted,
    Exploration,
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
    /// The root pending-dial ceiling, so an experiment can exhaust it.
    pub max_pending_dials: usize,
    /// The root connection ceiling, likewise.
    pub max_connections: usize,
    /// The address-state table bound, so an experiment can fill it.
    pub max_addresses: usize,
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
            max_pending_dials: 64,
            max_connections: 64,
            max_addresses: 8_192,
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
    /// The root admission this node's gate consults — trust, backoff,
    /// drain, AND the pending/connection ceilings.
    ///
    /// Behind a `Mutex` because revising trust, recording a failure and
    /// beginning a drain all need `&mut`; the gate itself never takes
    /// this lock, holding a `SnapshotHandle` instead.
    pub manager: std::sync::Arc<std::sync::Mutex<ConnectionManager>>,
    /// Peers this node trusts, mirrored so `trust` can republish the
    /// whole set — `set_trust` replaces rather than adds.
    trusted: Vec<PeerId>,
    /// Query ids this node started deliberately, WITH their class.
    ///
    /// `SnapshotResult::active_queries_by_class` needs the class, and a
    /// bare id set cannot supply one. Anything not in this map is
    /// unattributed library work, which the brief requires be counted
    /// rather than assumed absent — and which must not be subtracted
    /// from an explicit query that is still running.
    pub own_queries: HashMap<kad::QueryId, QueryClass>,
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
        // THE ONE root admission this node has. Built before the
        // behaviour, because the gate needs its handle.
        let mut policy = ConnectionPolicy::new(config.max_pending_dials, config.max_connections);
        policy.max_addresses = config.max_addresses;
        let manager = ConnectionManager::new(policy, config.max_pending_dials);
        let admission = manager.handle();
        let manager = std::sync::Arc::new(std::sync::Mutex::new(manager));

        let cfg = config.clone();
        let gate_admission = admission.clone();
        let gate_manager = std::sync::Arc::clone(&manager);
        let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                tcp::Config::default().nodelay(true),
                noise::Config::new,
                yamux::Config::default,
            )
            .expect("tcp transport")
            .with_behaviour(move |key| build_behaviour(key, &cfg, gate_admission, gate_manager))
            .expect("behaviour")
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(30)))
            .build();

        let gate = &swarm.behaviour().gate;
        let ledger = gate.ledger();
        let admitted = gate.admitted();

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
            manager,
            trusted: Vec::new(),
            own_queries: HashMap::new(),
            observed: crate::topology::Observations::default(),
        }
    }

    /// The address another node dials to reach this one.
    #[must_use]
    pub fn dial_address(&self) -> Multiaddr {
        self.listen.clone()
    }

    /// Mark `peer` data-plane trusted, through the REAL trust policy.
    ///
    /// `set_trust` replaces the whole set rather than adding to it, so
    /// the accumulated list is republished each time. An earlier version
    /// kept a private `Vec<PeerId>` the gate consulted directly, which
    /// meant the gate answered from a set the `ConnectionManager` had
    /// never seen — the classification would have been the harness's
    /// opinion rather than the product's.
    ///
    /// # Panics
    /// If a generated `PeerId` is not a canonical identity, which would
    /// mean the identity types disagree with libp2p.
    pub fn trust(&mut self, peer: PeerId) {
        self.trusted.push(peer);
        let ids: Vec<interweave_transport_api::TransportIdentity> = self
            .trusted
            .iter()
            .map(|p| {
                interweave_transport_api::TransportIdentity::parse(p.to_base58())
                    .expect("a libp2p PeerId is a canonical identity")
            })
            .collect();
        let sources = TrustSources::new(
            PeerTrustPolicy::new(ids.into_iter()).expect("within bounds"),
            InfrastructureSet::new(std::iter::empty()).expect("empty"),
        );
        let _ = self
            .manager
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_trust(sources, &[]);
    }

    /// Withdraw trust from `peer`, republishing the remaining set.
    ///
    /// # Panics
    /// If a generated `PeerId` is not a canonical identity.
    pub fn revoke(&mut self, peer: PeerId) -> usize {
        self.trusted.retain(|p| *p != peer);
        let ids: Vec<interweave_transport_api::TransportIdentity> = self
            .trusted
            .iter()
            .map(|p| {
                interweave_transport_api::TransportIdentity::parse(p.to_base58())
                    .expect("a libp2p PeerId is a canonical identity")
            })
            .collect();
        let sources = TrustSources::new(
            PeerTrustPolicy::new(ids.into_iter()).expect("within bounds"),
            InfrastructureSet::new(std::iter::empty()).expect("empty"),
        );
        // THE LIVE PEERS, so the manager can say which connections a
        // revision invalidates. Passing an empty slice asked "what
        // changed?" while withholding the only input that answers it —
        // and the experiments then disconnected by hand, which meant a
        // Stage 10 implementation that left revoked peers connected and
        // routable would have passed unchanged.
        let live: Vec<interweave_transport_api::TransportIdentity> = self
            .swarm
            .connected_peers()
            .filter_map(|p| interweave_transport_api::TransportIdentity::parse(p.to_base58()).ok())
            .collect();
        let revoked = self
            .manager
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_trust(sources, &live);

        // ROUTING REMOVAL IS UNCONDITIONAL. §11 requires it immediately,
        // and `revoked` only names peers the manager classified from the
        // LIVE connection list — so a peer that is routed but not
        // currently connected produced no entry, `remove_peer` was never
        // called, and it went on occupying the bounded table and being
        // selected for queries. Whether a peer has a socket open right
        // now is unrelated to whether it may be routed.
        let mut acted = 0;
        if let Some(k) = self.swarm.behaviour_mut().kad.as_mut() {
            k.remove_peer(&peer);
            acted += 1;
        }

        // The manager's result is still what says which CONNECTIONS the
        // revision invalidates — that is the question it can answer and
        // this cannot.
        for r in &revoked {
            let Ok(id) = r.peer.as_str().parse::<PeerId>() else {
                continue;
            };
            if let Some(k) = self.swarm.behaviour_mut().kad.as_mut() {
                k.remove_peer(&id);
            }
            let _ = self.swarm.disconnect_peer_id(id);
        }
        let _ = self.swarm.disconnect_peer_id(peer);
        acted
    }

    /// This node's own view of who it trusts.
    #[must_use]
    pub fn trusts(&self) -> Vec<PeerId> {
        self.trusted.clone()
    }

    /// Dial through the ROOT admission: register the ticket the way
    /// `GatedSwarm::dial` does, so the gate sees an admitted dial.
    ///
    /// # Panics
    /// If the dial cannot be started at all.
    pub fn dial_admitted(&mut self, address: Multiaddr) {
        use libp2p::swarm::dial_opts::DialOpts;
        let opts: DialOpts = address.into();
        // SAVED BEFORE `opts` MOVES. The cleanup used to forget a
        // hardcoded id zero, which is not the id that was registered —
        // so a synchronously rejected dial left its real announcement in
        // `AdmittedDials`, where a later dial could consume it and be
        // classified as ticket-admitted when it was behaviour-
        // originated. That is a leak in the measurement apparatus, not
        // only in the cleanup.
        let id = opts.connection_id();
        self.admitted.register(id);
        if self.swarm.dial(opts).is_err() {
            self.admitted.forget(id);
        }
    }

    /// Active queries by class: started here, not yet finished.
    ///
    /// Queries this node did NOT start are excluded, which is the part a
    /// set of ids gets wrong — a completed implicit bootstrap would
    /// otherwise be subtracted from an explicit query still in flight.
    #[must_use]
    pub fn active_queries_by_class(&self) -> std::collections::BTreeMap<QueryClass, usize> {
        let mut out = std::collections::BTreeMap::new();
        for (id, class) in &self.own_queries {
            if !self.observed.finished_queries.contains(id) {
                *out.entry(*class).or_insert(0) += 1;
            }
        }
        out
    }

    /// The Kademlia behaviour, when this node has one.
    pub fn kad(&mut self) -> Option<&mut kad::Behaviour<MemoryStore>> {
        self.swarm.behaviour_mut().kad.as_mut()
    }

    /// Whether this peer is already in the routing table.
    ///
    /// A re-announcement for a routed peer is an address UPDATE, not a
    /// new entry, so a population bound must not refuse it.
    pub fn routes(&mut self, peer: &PeerId) -> bool {
        self.swarm.behaviour_mut().kad.as_mut().is_some_and(|k| {
            k.kbuckets()
                .any(|b| b.iter().any(|e| e.node.key.preimage() == peer))
        })
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
    admission: SnapshotHandle,
    manager: std::sync::Arc<std::sync::Mutex<ConnectionManager>>,
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
        gate: InstrumentedGate::new(config.gate_mode, admission, manager),
        identify,
        kad,
    }
}
