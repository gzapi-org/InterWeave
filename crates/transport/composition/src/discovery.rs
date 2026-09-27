// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The configured discovery providers, composed into one
//! `DiscoveryManager` and pumped (`discovery/COMPOSITION.md`).
//!
//! THE BOOK IS REACHED THROUGH THE PEER'S DOOR ONLY. What the manager
//! aggregates goes to `SwarmRuntime::learn`, which judges every address by
//! ADR-0052's discovery predicate and never writes the operator set --
//! never through `add_address`, the operator's door, which would launder
//! a peer-supplied address past the boundary (plan §15). A candidate is
//! handed over when its address list changes, not on every round.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use interweave_discovery_api::{DiscoveryProvider, ProviderHealth};
use interweave_discovery_cache::{PeerCache, PeerCacheDiscovery};
use interweave_discovery_kademlia::{KademliaDiscovery, KademliaProviderConfig};
use interweave_discovery_mdns::MdnsDiscovery;
use interweave_discovery_static::StaticBootstrapDiscovery;
use interweave_kademlia_control_api::{KademliaCommand, RoutingView};
use interweave_transport_api::{Health, TransportIdentity};
use interweave_transport_libp2p::SwarmEvent;
use interweave_transport_libp2p::runtime::kademlia_driver::network_hash;
use interweave_transport_runtime::discovery::OverflowStats;
use interweave_transport_runtime::{AggregatedCandidate, DiscoveryManager};
use interweave_trust_api::{PeerTrustPolicy, TrustDecision};

use crate::translate::{CompositionError, DiscoveryPlan, KADEMLIA_WIRE_MAJOR};

/// Events drained from one provider per round.
const DRAIN_PER_ROUND: usize = 256;

/// Reconnects asked of the substrate per round. A peer in backoff is
/// refused by the gate before any socket, so the cost of asking again is
/// a command; the bound keeps a large candidate set from turning each
/// round into a burst of them.
pub const MAX_RECONNECTS_PER_ROUND: usize = 16;

/// One provider as the diagnostics report it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDiagnostics {
    /// The provider's name, its event source.
    pub name: String,
    /// Its health as the manager holds it.
    pub health: Option<ProviderHealth>,
    /// Its configured priority.
    pub priority: Option<i32>,
    /// The interface version its descriptor declares.
    pub interface_version: Option<String>,
}

/// The discovery half of the runtime's diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryDiagnostics {
    /// How many providers are registered.
    pub provider_count: usize,
    /// Each, by name.
    pub providers: Vec<ProviderDiagnostics>,
    /// Candidates the manager holds.
    pub candidates: usize,
    /// The candidate set's bound at work.
    pub overflow: OverflowStats,
    /// Kademlia's routing target, capped by the trusted population
    /// (`RoutingView::effective_target`); `None` with Kademlia off.
    pub kademlia_effective_target: Option<u32>,
}

pub(crate) struct Discovery {
    manager: DiscoveryManager,
    statics: Option<StaticBootstrapDiscovery>,
    cache: Option<PeerCacheDiscovery>,
    mdns: Option<MdnsDiscovery>,
    kademlia: Option<KademliaDiscovery>,
    trust: PeerTrustPolicy,
    /// What `learn` was last given per peer; pruned to the manager's
    /// current candidates each round, so bounded by the candidate set.
    learned: HashMap<TransportIdentity, Vec<String>>,
}

fn discovery_error(what: &str, e: impl core::fmt::Debug) -> CompositionError {
    CompositionError::Discovery(format!("{what}: {e:?}"))
}

impl Discovery {
    pub(crate) fn new(
        plan: DiscoveryPlan,
        cache_file: Option<&Path>,
        trust: PeerTrustPolicy,
        trusted: BTreeSet<TransportIdentity>,
        local: &TransportIdentity,
        now_ms: u64,
    ) -> Result<Self, CompositionError> {
        let mut manager = DiscoveryManager::new();
        let mut register =
            |provider: &mut dyn DiscoveryProvider, priority: i32| -> Result<(), CompositionError> {
                manager
                    .register(provider.descriptor(), priority)
                    .map_err(|e| discovery_error("register", e))?;
                provider
                    .start(now_ms)
                    .map_err(|e| discovery_error("start", e))
            };
        let statics = match plan.static_bootstrap {
            Some((entries, priority)) => {
                let mut p = StaticBootstrapDiscovery::new(entries)
                    .map_err(|e| discovery_error("static-bootstrap", e))?;
                register(&mut p, priority)?;
                Some(p)
            }
            None => None,
        };
        let cache = match plan.peer_cache {
            Some((limits, priority)) => {
                let path = cache_file.ok_or(CompositionError::Discovery(
                    "peer-cache is enabled and no cache file was given".to_owned(),
                ))?;
                let cache =
                    PeerCache::load(path, limits).map_err(|e| discovery_error("peer-cache", e))?;
                let mut p = PeerCacheDiscovery::new(cache);
                register(&mut p, priority)?;
                Some(p)
            }
            None => None,
        };
        let mdns = match plan.mdns {
            Some(priority) => {
                let mut p = MdnsDiscovery::new();
                register(&mut p, priority)?;
                Some(p)
            }
            None => None,
        };
        let kademlia = match plan.kademlia {
            Some((k, priority)) => {
                let config = KademliaProviderConfig {
                    mode: k.mode,
                    wire_major: KADEMLIA_WIRE_MAJOR,
                    network_hash: network_hash(&k.network_id),
                    candidate_ttl_ms: k.candidate_ttl_ms,
                    targeted_lookup_cooldown_ms: u64::from(k.targeted_lookup_cooldown_ms),
                    target_routing_peers: k.target_routing_peers,
                    max_routing_peers: k.max_routing_peers,
                    exploration_interval_ms: u64::from(k.exploration_interval_ms),
                    exploration_jitter_percent: k.exploration_jitter_percent,
                    max_concurrent_queries: k.max_concurrent_queries,
                    max_queries_per_minute: k.max_queries_per_minute,
                    bootstrap_min_interval_ms: u64::from(k.bootstrap_min_interval_ms),
                    bootstrap_refresh_interval_ms: u64::from(k.bootstrap_refresh_interval_ms),
                };
                let mut p = KademliaDiscovery::new(config, local.clone())
                    .map_err(|e| discovery_error("kademlia", e))?;
                register(&mut p, priority)?;
                p.set_remote_trusted(trusted);
                Some(p)
            }
            None => None,
        };
        Ok(Self {
            manager,
            statics,
            cache,
            mdns,
            kademlia,
            trust,
            learned: HashMap::new(),
        })
    }

    /// Drain every provider into the manager. A refused event is the
    /// manager's contract working (an untrusted or malformed candidate)
    /// and is counted there, not here.
    pub(crate) fn pump(&mut self, now_ms: u64) {
        let providers: [Option<&mut dyn DiscoveryProvider>; 4] = [
            self.statics
                .as_mut()
                .map(|p| p as &mut dyn DiscoveryProvider),
            self.cache.as_mut().map(|p| p as &mut dyn DiscoveryProvider),
            self.mdns.as_mut().map(|p| p as &mut dyn DiscoveryProvider),
            self.kademlia
                .as_mut()
                .map(|p| p as &mut dyn DiscoveryProvider),
        ];
        for provider in providers.into_iter().flatten() {
            let source = provider.descriptor().name;
            for event in provider.drain_events(now_ms, DRAIN_PER_ROUND) {
                let _ = self.manager.on_event(&source, event, now_ms, &self.trust);
            }
        }
    }

    /// Advance Kademlia's own schedule and take what it asks the driver
    /// to do.
    pub(crate) fn kademlia_commands(&mut self, now_ms: u64) -> Vec<KademliaCommand> {
        let Some(kademlia) = self.kademlia.as_mut() else {
            return Vec::new();
        };
        let _ = kademlia.tick(now_ms, rand::random());
        kademlia.drain_commands(usize::MAX)
    }

    /// Feed a substrate event to the provider it belongs to. `true` when
    /// it was discovery's.
    pub(crate) fn on_swarm_event(&mut self, event: &SwarmEvent, now_ms: u64) -> bool {
        match event {
            SwarmEvent::Kademlia { event } => {
                if let Some(kademlia) = self.kademlia.as_mut() {
                    kademlia.ingest_driver_event(event.clone(), now_ms);
                }
                true
            }
            SwarmEvent::MdnsDiscovered { candidates } => {
                if let Some(mdns) = self.mdns.as_mut() {
                    for candidate in candidates {
                        for address in &candidate.addresses {
                            let _ =
                                mdns.push_discovered(candidate.peer_id.as_str(), address, now_ms);
                        }
                    }
                }
                true
            }
            SwarmEvent::MdnsExpired { expired } => {
                if let Some(mdns) = self.mdns.as_mut() {
                    for (peer, address) in expired {
                        let _ = mdns.push_expired(peer.as_str(), address, now_ms);
                    }
                }
                true
            }
            // `providers/mdns.md` §Failure: an interface, the watcher or a
            // rebuild failing, or mDNS unavailable, is the provider's
            // degraded state; silence is not.
            SwarmEvent::MdnsInterfaceFailed { .. }
            | SwarmEvent::MdnsWatcherFailed { .. }
            | SwarmEvent::MdnsRebuildFailed { .. }
            | SwarmEvent::MdnsUnavailable { .. } => {
                if let Some(mdns) = self.mdns.as_mut() {
                    mdns.report_backend_down(now_ms);
                }
                true
            }
            _ => false,
        }
    }

    /// Expire stale candidates and return those whose addresses changed
    /// since they were last handed to the book.
    pub(crate) fn changed_candidates(
        &mut self,
        now_ms: u64,
    ) -> Vec<(TransportIdentity, Vec<String>)> {
        self.manager.sweep(now_ms);
        let candidates: Vec<AggregatedCandidate> = self.manager.candidates(now_ms);
        let current: BTreeSet<&TransportIdentity> = candidates.iter().map(|c| &c.peer_id).collect();
        self.learned.retain(|peer, _| current.contains(peer));
        let mut changed = Vec::new();
        for candidate in &candidates {
            let addresses: Vec<String> = candidate
                .address_list()
                .into_iter()
                .map(str::to_owned)
                .collect();
            if self.learned.get(&candidate.peer_id) != Some(&addresses) {
                self.learned
                    .insert(candidate.peer_id.clone(), addresses.clone());
                changed.push((candidate.peer_id.clone(), addresses));
            }
        }
        changed
    }

    /// Data-plane-trusted candidates holding no connection, at most
    /// [`MAX_RECONNECTS_PER_ROUND`]: whom discovery's reconnect dials.
    /// Only an ALLOWED peer: discovery grants no trust, and a candidate
    /// for anyone else is advisory data the book may hold and nothing
    /// dials on discovery's account.
    pub(crate) fn reconnect_targets(
        &self,
        now_ms: u64,
        connected: impl Fn(&TransportIdentity) -> bool,
    ) -> Vec<TransportIdentity> {
        self.manager
            .candidates(now_ms)
            .into_iter()
            .map(|c| c.peer_id)
            .filter(|peer| self.trust.decide(peer) == TrustDecision::Allowed && !connected(peer))
            .take(MAX_RECONNECTS_PER_ROUND)
            .collect()
    }

    /// Write the peer cache if its debounce has passed.
    pub(crate) fn flush(&mut self, now_ms: u64) {
        if let Some(cache) = self.cache.as_mut() {
            let _ = cache.cache_mut().flush_if_due(now_ms);
        }
    }

    /// The discovery component's health: the manager's aggregate.
    pub(crate) fn health(&self) -> Health {
        match self.manager.aggregate_health() {
            ProviderHealth::Healthy => Health::Healthy,
            ProviderHealth::Degraded => Health::Degraded,
            ProviderHealth::Unavailable => Health::Unavailable,
        }
    }

    pub(crate) fn diagnostics(&self) -> DiscoveryDiagnostics {
        let names = [
            self.statics.as_ref().map(|p| p.descriptor().name),
            self.cache.as_ref().map(|p| p.descriptor().name),
            self.mdns.as_ref().map(|p| p.descriptor().name),
            self.kademlia.as_ref().map(|p| p.descriptor().name),
        ];
        DiscoveryDiagnostics {
            provider_count: self.manager.provider_count(),
            providers: names
                .into_iter()
                .flatten()
                .map(|name| ProviderDiagnostics {
                    health: self.manager.provider_health(&name),
                    priority: self.manager.provider_priority(&name),
                    interface_version: self
                        .manager
                        .descriptor(&name)
                        .map(|d| d.interface_version.clone()),
                    name,
                })
                .collect(),
            candidates: self.manager.candidate_count(),
            overflow: self.manager.overflow_stats(),
            kademlia_effective_target: self.kademlia.as_ref().map(|k| {
                let view: RoutingView = k.routing_view();
                view.effective_target()
            }),
        }
    }

    pub(crate) fn shutdown(&mut self, now_ms: u64) {
        self.flush(now_ms);
        let providers: [Option<&mut dyn DiscoveryProvider>; 4] = [
            self.statics
                .as_mut()
                .map(|p| p as &mut dyn DiscoveryProvider),
            self.cache.as_mut().map(|p| p as &mut dyn DiscoveryProvider),
            self.mdns.as_mut().map(|p| p as &mut dyn DiscoveryProvider),
            self.kademlia
                .as_mut()
                .map(|p| p as &mut dyn DiscoveryProvider),
        ];
        for provider in providers.into_iter().flatten() {
            provider.shutdown(now_ms);
        }
    }
}
