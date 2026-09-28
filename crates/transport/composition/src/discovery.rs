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

use interweave_discovery_api::{DiscoveryEvent, DiscoveryProvider, PeerHint, ProviderHealth};
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

/// How long a candidate's unchanged addresses go without being offered to
/// the book again. The memo records what `learn` was OFFERED, not what the
/// book kept: an address the book refused -- a full book -- or evicted
/// later would otherwise never be offered again while discovery kept
/// reporting it (#137 carried R2). One `learn` per candidate per interval,
/// bounded by the candidate set.
pub const RELEARN_INTERVAL_MS: u64 = 300_000;

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
    /// What `learn` was last given per peer, and when; pruned to the
    /// manager's current candidates each round, so bounded by the
    /// candidate set, and offered again after [`RELEARN_INTERVAL_MS`].
    learned: HashMap<TransportIdentity, (Vec<String>, u64)>,
    /// Where the next round's reconnect window starts.
    reconnect_cursor: usize,
    /// The providers whose candidates seed Kademlia (`seed_sources`).
    seed_sources: BTreeSet<String>,
    /// Whether the bootstrap owed on a non-empty routing table was asked
    /// for; cleared when routing empties, so recovery bootstraps again.
    bootstrapped: bool,
}

/// At most `max` items of `items` starting at `start` (modulo the
/// length), wrapping, and where the next window starts.
fn rotate<T: Clone>(items: &[T], start: usize, max: usize) -> (Vec<T>, usize) {
    if items.is_empty() {
        return (Vec::new(), 0);
    }
    let start = start % items.len();
    let take = max.min(items.len());
    let window = items
        .iter()
        .cycle()
        .skip(start)
        .take(take)
        .cloned()
        .collect();
    (window, (start + take) % items.len())
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
        let seed_sources: BTreeSet<String> = plan
            .kademlia
            .as_ref()
            .map(|(k, _)| k.seed_sources.iter().cloned().collect())
            .unwrap_or_default();
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
            reconnect_cursor: 0,
            seed_sources,
            bootstrapped: false,
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
        let mut seeds = Vec::new();
        for provider in providers.into_iter().flatten() {
            let source = provider.descriptor().name;
            let seeding = self.seed_sources.contains(&source);
            for event in provider.drain_events(now_ms, DRAIN_PER_ROUND) {
                if seeding && let DiscoveryEvent::CandidateObserved { candidate } = &event {
                    seeds.push(candidate.clone());
                }
                let _ = self.manager.on_event(&source, event, now_ms, &self.trust);
            }
        }
        // SEED SOURCES FEED KADEMLIA (`kademlia-integration.md` §8): the
        // candidates of the providers `seed_sources` names reach the
        // provider as hints, which applies §8's own rules -- a
        // kademlia-sourced candidate is not re-offered, a lapsed one is
        // ignored, protocol evidence is kept (#137 review F4).
        if let Some(kademlia) = self.kademlia.as_mut() {
            for candidate in seeds {
                let _ = kademlia.add_hint(PeerHint::CandidateHint(candidate), now_ms);
            }
        }
    }

    /// Advance Kademlia's own schedule and take what it asks the driver
    /// to do.
    pub(crate) fn kademlia_commands(&mut self, now_ms: u64) -> Vec<KademliaCommand> {
        let Some(kademlia) = self.kademlia.as_mut() else {
            return Vec::new();
        };
        // §9.1's bootstrap: once routing is non-empty (at start, or on
        // recovering from empty) and then on the refresh interval. The
        // driver's own periodic bootstrap is off because this schedule
        // owns it; the provider spaces requests by
        // `bootstrap_min_interval` (#137 review F4).
        if kademlia.routing_view().routing_peers == 0 {
            self.bootstrapped = false;
        } else if (!self.bootstrapped || kademlia.bootstrap_refresh_due(now_ms))
            && kademlia.request_bootstrap(now_ms).is_ok()
        {
            self.bootstrapped = true;
        }
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
            // THE CACHE LEARNS WHAT THIS NODE REACHED (`providers/
            // peer-cache.md` §Ownership): a route a dial of ours
            // established -- whatever first suggested the address, the
            // proof is our own dial (`confirms_route`). Without it
            // the composed cache was loaded and flushed and never
            // written (#137 review F3).
            SwarmEvent::RouteConfirmed { peer, address } => {
                if let Some(cache) = self.cache.as_mut() {
                    let _ = cache.add_hint(
                        PeerHint::ObservedReachable {
                            peer_id: peer.clone(),
                            address: address.clone(),
                            observed_at: now_ms,
                        },
                        now_ms,
                    );
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
    /// since they were last handed to the book, or were last handed to it
    /// [`RELEARN_INTERVAL_MS`] ago or more
    /// (`an_unchanged_candidate_is_offered_again_after_the_interval`).
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
            let due = self
                .learned
                .get(&candidate.peer_id)
                .is_none_or(|(offered, at)| {
                    offered != &addresses || now_ms.saturating_sub(*at) >= RELEARN_INTERVAL_MS
                });
            if due {
                self.learned
                    .insert(candidate.peer_id.clone(), (addresses.clone(), now_ms));
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
    ///
    /// ROTATED ACROSS ROUNDS: the manager lists candidates in PeerId
    /// order, and taking the first sixteen every round starved every
    /// peer after them for as long as those sixteen stayed unconnected
    /// -- a relay-only peer the discovery door cannot route holds its
    /// place forever (#137 review F2). Each round starts where the last
    /// stopped, so every eligible peer is asked within
    /// `ceil(eligible / MAX_RECONNECTS_PER_ROUND)` rounds.
    pub(crate) fn reconnect_targets(
        &mut self,
        now_ms: u64,
        connected: impl Fn(&TransportIdentity) -> bool,
    ) -> Vec<TransportIdentity> {
        let eligible: Vec<TransportIdentity> = self
            .manager
            .candidates(now_ms)
            .into_iter()
            .map(|c| c.peer_id)
            .filter(|peer| self.trust.decide(peer) == TrustDecision::Allowed && !connected(peer))
            .collect();
        let (window, next) = rotate(&eligible, self.reconnect_cursor, MAX_RECONNECTS_PER_ROUND);
        self.reconnect_cursor = next;
        window
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
        // WRITTEN WHATEVER THE DEBOUNCE SAYS: a route confirmed inside the
        // last write interval would otherwise be lost with the process,
        // which is the restart the cache exists to survive.
        if let Some(cache) = self.cache.as_mut() {
            let _ = cache.cache_mut().flush(now_ms);
        }
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::{Discovery, MAX_RECONNECTS_PER_ROUND, RELEARN_INTERVAL_MS, rotate};
    use crate::translate::DiscoveryPlan;
    use interweave_discovery_static::StaticEntry;
    use interweave_kademlia_control_api::{KademliaCommand, KademliaEvent, QueryClass};
    use interweave_profile_config::DiscoveryProviderSettings;
    use interweave_profile_identity::ProfileIdentity;
    use interweave_transport_api::TransportIdentity;
    use interweave_transport_libp2p::SwarmEvent;
    use interweave_trust_api::PeerTrustPolicy;
    use std::collections::BTreeSet;

    fn peer() -> TransportIdentity {
        ProfileIdentity::generate()
            .transport_identity()
            .expect("peer id")
    }

    /// A static seed and a kademlia entry naming `seed_sources`.
    fn discovery(seed_sources: &str) -> (Discovery, TransportIdentity) {
        let seed = peer();
        let local = peer();
        let settings: DiscoveryProviderSettings = serde_norway::from_str(&format!(
            "network_id: interweave-test\nseed_sources: {seed_sources}\n"
        ))
        .expect("parses");
        let plan = DiscoveryPlan {
            static_bootstrap: Some((
                vec![StaticEntry::new(seed.clone(), "/ip4/8.8.8.8/tcp/4001").expect("valid")],
                10,
            )),
            kademlia: Some((settings.kademlia_profile().expect("resolves"), 40)),
            ..DiscoveryPlan::default()
        };
        let trusted = BTreeSet::from([seed.clone()]);
        let discovery = Discovery::new(
            plan,
            None,
            PeerTrustPolicy::new(trusted.clone()).expect("one peer"),
            trusted,
            &local,
            0,
        )
        .expect("composes");
        (discovery, seed)
    }

    fn offered(commands: &[KademliaCommand], seed: &TransportIdentity) -> bool {
        commands
            .iter()
            .any(|c| matches!(c, KademliaCommand::OfferRoutingPeer { peer, .. } if peer == seed))
    }

    #[test]
    fn a_seed_source_named_in_the_profile_seeds_kademlia_and_an_unnamed_one_does_not() {
        let (mut seeded, seed) = discovery("[static-bootstrap]");
        seeded.pump(0);
        assert!(
            offered(&seeded.kademlia_commands(0), &seed),
            "the static seed became an offer"
        );
        let (mut unseeded, seed) = discovery("[]");
        unseeded.pump(0);
        assert!(
            !offered(&unseeded.kademlia_commands(0), &seed),
            "the control: with no seed source nothing is offered"
        );
    }

    /// The window's wiring, not only `rotate`: twenty trusted, unconnected
    /// candidates are all asked across two rounds, where taking the
    /// first sixteen each round asks sixteen (#137 re-review N4).
    #[test]
    fn reconnect_rounds_reach_every_candidate_past_the_first_window() {
        let peers: Vec<TransportIdentity> =
            (0..MAX_RECONNECTS_PER_ROUND + 4).map(|_| peer()).collect();
        let entries = peers
            .iter()
            .map(|p| StaticEntry::new(p.clone(), "/ip4/8.8.8.8/tcp/4001").expect("valid"))
            .collect();
        let trusted: BTreeSet<TransportIdentity> = peers.iter().cloned().collect();
        let mut d = Discovery::new(
            DiscoveryPlan {
                static_bootstrap: Some((entries, 10)),
                ..DiscoveryPlan::default()
            },
            None,
            PeerTrustPolicy::new(trusted.clone()).expect("a small set"),
            trusted.clone(),
            &peer(),
            0,
        )
        .expect("composes");
        d.pump(0);
        let mut asked = BTreeSet::new();
        for _ in 0..2 {
            let round = d.reconnect_targets(0, |_| false);
            assert!(round.len() <= MAX_RECONNECTS_PER_ROUND);
            asked.extend(round);
        }
        assert_eq!(asked, trusted, "every candidate asked within two rounds");
    }

    /// Emptied and refilled, the routing table is bootstrapped again
    /// (§9.1's recovery trigger) once the minimum interval has passed.
    #[test]
    fn a_routing_table_refilled_after_emptying_is_bootstrapped_again() {
        let (mut d, seed) = discovery("[]");
        let bootstraps = |commands: Vec<KademliaCommand>| {
            commands
                .iter()
                .filter(|c| {
                    matches!(
                        c,
                        KademliaCommand::StartQuery {
                            class: QueryClass::Bootstrap,
                            ..
                        }
                    )
                })
                .count()
        };
        let event = |e| SwarmEvent::Kademlia { event: e };
        assert!(d.on_swarm_event(
            &event(KademliaEvent::RoutingPeerAdded { peer: seed.clone() }),
            1_000
        ));
        assert_eq!(bootstraps(d.kademlia_commands(1_000)), 1);
        assert!(d.on_swarm_event(
            &event(KademliaEvent::RoutingPeerRemoved { peer: seed.clone() }),
            2_000
        ));
        assert_eq!(
            bootstraps(d.kademlia_commands(2_000)),
            0,
            "nothing to bootstrap from"
        );
        // Past the five-minute minimum, before the fifteen-minute refresh.
        let later = 1_000 + 6 * 60 * 1_000;
        assert!(d.on_swarm_event(
            &event(KademliaEvent::RoutingPeerAdded { peer: seed }),
            later
        ));
        assert_eq!(
            bootstraps(d.kademlia_commands(later)),
            1,
            "recovery bootstraps again"
        );
    }

    #[test]
    fn a_non_empty_routing_table_is_bootstrapped_and_an_empty_one_is_not() {
        let (mut d, seed) = discovery("[]");
        let bootstraps = |commands: Vec<KademliaCommand>| {
            commands
                .iter()
                .filter(|c| {
                    matches!(
                        c,
                        KademliaCommand::StartQuery {
                            class: QueryClass::Bootstrap,
                            ..
                        }
                    )
                })
                .count()
        };
        assert_eq!(
            bootstraps(d.kademlia_commands(0)),
            0,
            "nothing to bootstrap from"
        );
        let added = SwarmEvent::Kademlia {
            event: KademliaEvent::RoutingPeerAdded { peer: seed },
        };
        assert!(d.on_swarm_event(&added, 1_000));
        assert_eq!(
            bootstraps(d.kademlia_commands(1_000)),
            1,
            "bootstrapped once routing filled"
        );
        assert_eq!(
            bootstraps(d.kademlia_commands(2_000)),
            0,
            "and not again until the refresh interval"
        );
    }

    #[test]
    fn every_eligible_peer_is_asked_within_the_bound_on_rounds() {
        let items: Vec<usize> = (0..MAX_RECONNECTS_PER_ROUND + 4).collect();
        let mut cursor = 0;
        let mut seen = BTreeSet::new();
        let rounds = items.len().div_ceil(MAX_RECONNECTS_PER_ROUND);
        for _ in 0..rounds {
            let (window, next) = rotate(&items, cursor, MAX_RECONNECTS_PER_ROUND);
            assert!(
                window.len() <= MAX_RECONNECTS_PER_ROUND,
                "at most the bound"
            );
            seen.extend(window);
            cursor = next;
        }
        assert_eq!(seen.len(), items.len(), "the 17th and later are reached");
    }

    #[test]
    fn an_unchanged_candidate_is_offered_again_after_the_interval() {
        // The book may have refused or since evicted what it was offered;
        // an unchanged candidate is offered again once the interval has
        // passed, and not before (#137 carried R2).
        let (mut d, seed) = discovery("[]");
        d.pump(0);
        let named =
            |v: Vec<(TransportIdentity, Vec<String>)>| v.into_iter().any(|(p, _)| p == seed);
        assert!(named(d.changed_candidates(0)), "offered first");
        assert!(
            !named(d.changed_candidates(RELEARN_INTERVAL_MS - 1)),
            "unchanged, and not yet due"
        );
        assert!(
            named(d.changed_candidates(RELEARN_INTERVAL_MS)),
            "offered again once due"
        );
    }

    #[test]
    fn a_short_list_is_taken_whole_and_an_empty_one_is_nothing() {
        assert_eq!(rotate(&[1, 2, 3], 5, MAX_RECONNECTS_PER_ROUND).0.len(), 3);
        assert!(rotate::<u8>(&[], 7, MAX_RECONNECTS_PER_ROUND).0.is_empty());
    }
}
