// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Candidate aggregation across discovery providers (ADR-0006, ADR-0007).
//!
//! `DiscoveryManager` is the only consumer of a `DiscoveryProvider`
//! (`PROVIDER-CONTRACT.md`), and this is its state: providers report
//! observations, this merges them, and what comes out is a bounded set of
//! candidate peers a CONSUMER may consider dialling.
//!
//! # What this deliberately cannot do
//!
//! It does not dial, does not own a Swarm, and **never mutates trust**
//! (ADR-0011, ADR-0012). A [`PeerTrustPolicy`] appears in exactly one
//! place — eviction ORDER — because `architecture/discovery/DESIGN.md`
//! says overflow evicts "least-recently-observed untrusted candidates",
//! and answering "is this one untrusted" is a read. Nothing here writes
//! it, and no method returns a trust decision to a caller.
//!
//! # Merge model (`COMPOSITION.md`)
//!
//! Candidates are keyed by PeerId. Each address carries one provenance
//! record PER SOURCE, so two providers reporting the same address are two
//! records with independent lifetimes: an address disappears only when no
//! live source still supports it, and a peer disappears when no addresses
//! remain. Protocol observations are merged separately by `(peer,
//! protocol, source)` and **never keep an otherwise expired peer alive** —
//! they are facts about a peer, not evidence it is still there.
//!
//! # Time is a parameter
//!
//! Every method that can expire something takes `now_ms`. No clock is read
//! here, so expiry is tested by enumeration rather than by sleeping — the
//! same shape as `dedup.rs` and `directory.rs`.

use std::collections::{BTreeMap, BTreeSet};

use interweave_discovery_api::{
    CandidatePeer, DiscoveryEvent, ProtocolId, ProviderDescriptor, ProviderHealth, ProviderScope,
};
use interweave_transport_api::TransportIdentity;
use interweave_trust_api::PeerTrustPolicy;

/// Aggregate candidate PeerIds (`DESIGN.md`).
pub const MAX_CANDIDATES: usize = 4096;
/// Addresses retained per candidate peer.
pub const MAX_ADDRESSES_PER_PEER: usize = 16;
/// Provenance records retained per address — one per source.
pub const MAX_PROVENANCE_PER_ADDRESS: usize = 8;
/// Protocol observations retained per peer.
pub const MAX_OBSERVATIONS_PER_PEER: usize = 16;

/// Registered providers, which also bounds how many can be composed.
pub const MAX_PROVIDERS: usize = 16;

/// Configured candidates retained against overflow eviction.
///
/// `DESIGN.md` says configured static entries are retained "within their
/// own explicit cap"; this is that cap, and it matches the 64-entry
/// ceiling `static-bootstrap` itself accepts, so a fully-loaded static
/// provider is exactly covered and no more.
pub const MAX_CONFIGURED_RETAINED: usize = 64;

/// The default lifetime applied to an observation from a provider that
/// CAN express expiry but did not for this candidate.
///
/// `CandidatePeer::expires_at` of `None` means "no stated expiry", not
/// "permanent", and the manager is where the bound gets applied. Ten
/// minutes: short enough that a stale LAN address does not outlive the
/// laptop that left.
///
/// It does NOT apply to a provider whose descriptor says
/// `supports_expiry: false` — see [`DiscoveryManager::on_event`]. Such a
/// provider is not declining to state a lifetime for one candidate; it is
/// saying its observations do not lapse on their own, and ageing them out
/// on a timer deletes configured bootstrap entries from a long-running
/// node that is still configured with them.
pub const DEFAULT_OBSERVATION_TTL_MS: u64 = 600_000;

/// Why an event was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectedEvent {
    /// The event's `source` is not the registered name of the provider
    /// that emitted it.
    ///
    /// Provenance is the twelfth conformance guarantee, and it is only
    /// checkable because a provider's descriptor name is the source it
    /// stamps. A provider claiming another's name would launder a
    /// candidate's origin, so the manager refuses rather than rewrites.
    SourceMismatch {
        /// The provider that emitted it.
        expected: String,
        /// The name the event carried.
        got: String,
    },
    /// The candidate failed its own contract validation.
    InvalidCandidate,
    /// No provider is registered under that name.
    UnknownProvider,
}

/// One source's support for one address.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Provenance {
    source: String,
    observed_at: u64,
    expires_at: u64,
    /// The source is a configured provider that declares NO EXPIRY, so
    /// it will not emit this entry again — which is what the retention
    /// rule in `evict_one` keys on. Carried per record rather than looked
    /// up at eviction time because the registry can change between the
    /// observation and the pressure.
    ///
    /// Scope alone is not the test: `PeerCacheDiscovery` is also
    /// `Configured`, and a cache record ages out and is re-emitted from
    /// disk, so it needs no protection from eviction.
    pinned: bool,
}

/// One peer's aggregated reachability.
#[derive(Debug, Clone, Default)]
struct Entry {
    /// address -> the sources supporting it, each with its own lifetime.
    addresses: BTreeMap<String, Vec<Provenance>>,
    /// `(protocol, source)` -> (supported, observed_at, expires_at).
    observations: BTreeMap<(ProtocolId, String), (bool, u64, u64)>,
    /// The most recent observation of this peer from any source, for the
    /// least-recently-observed half of the eviction rule.
    ///
    /// DERIVED, never accumulated. Held as a monotonic maximum it only
    /// ever rose, so a peer whose most recent source retracted or expired
    /// kept that source's recency forever — and `evict_one` then
    /// preserved it over a peer whose live reachability had actually been
    /// observed more recently. `Entry::recency` computes it, and nothing
    /// stores it.
    _recency_is_derived: (),
}

impl Entry {
    /// The newest observation across provenance still LIVE at `now_ms`.
    ///
    /// Addresses only: a protocol observation never keeps a peer alive
    /// (COMPOSITION.md), so it must not make one look recently reachable
    /// either.
    ///
    /// The time argument is the point. Reading every record regardless of
    /// expiry made a peer with an old live source and a newer LAPSED one
    /// report the lapsed timestamp — and eviction then preserved it over
    /// a candidate whose live reachability was genuinely more recent.
    /// Records are only removed by `sweep`, which runs on a pump the
    /// caller controls, so "expired" and "gone" are not the same state
    /// here and this must judge the first.
    fn recency(&self, now_ms: u64) -> u64 {
        self.addresses
            .values()
            .flat_map(|records| records.iter())
            .filter(|r| now_ms < r.expires_at)
            .map(|r| r.observed_at)
            .max()
            .unwrap_or(0)
    }

    /// Any live provenance from a pinned provider.
    fn is_pinned(&self) -> bool {
        self.addresses
            .values()
            .any(|rs| rs.iter().any(|r| r.pinned))
    }
}

/// One address, and which providers currently vouch for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressProvenance {
    /// The opaque address.
    pub address: String,
    /// Every source with a live observation of it, best priority first.
    ///
    /// PER ADDRESS, not per candidate: ADR-0007 makes priority guidance
    /// for choosing among a peer's addresses, and a consumer cannot apply
    /// it without knowing which provider supports which address. A single
    /// merged source set could not answer that.
    pub sources: Vec<String>,
    /// The best (lowest) configured priority among those sources.
    pub best_priority: i32,
    /// The most recent observation of this address.
    pub observed_at_ms: u64,
}

/// A candidate as a consumer sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregatedCandidate {
    /// Which peer.
    pub peer_id: TransportIdentity,
    /// Every address at least one live source still supports, ordered by
    /// the best provider priority supporting each.
    ///
    /// An ORDER, never an authorization: a lower priority means "try this
    /// one first", and every address here still passes dial admission
    /// exactly as any other would (ADR-0007, ADR-0011).
    pub addresses: Vec<AddressProvenance>,
    /// Every source currently supporting at least one of those addresses.
    pub sources: BTreeSet<String>,
    /// The most recent observation across sources.
    pub last_observed_ms: u64,
    /// Protocol facts still live for this peer, merged across sources.
    ///
    /// Advisory like everything else here: a protocol observation never
    /// keeps a peer alive (COMPOSITION.md), so this list can be empty for
    /// a peer with addresses and is never the reason one is present. It
    /// is peer-asserted evidence a consumer may weigh, not authorization.
    ///
    /// Exposed because the manager MERGES these and nothing could read
    /// them: bounded, expired on schedule, retracted with their source,
    /// and unreachable — which is a store rather than a contribution, and
    /// leaves the Kademlia seed flow that is meant to consume them with
    /// no way to.
    pub protocol_observations: Vec<MergedObservation>,
}

/// One protocol fact about a peer, as merged from the providers
/// asserting it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedObservation {
    /// Which protocol.
    pub protocol: ProtocolId,
    /// Whether it is asserted supported.
    pub supported: bool,
    /// Which sources currently assert it, in name order.
    pub sources: Vec<String>,
    /// The most recent assertion across those sources.
    pub observed_at_ms: u64,
}

impl AggregatedCandidate {
    /// Just the addresses, in priority order.
    #[must_use]
    pub fn address_list(&self) -> Vec<&str> {
        self.addresses.iter().map(|a| a.address.as_str()).collect()
    }
}

/// The merged candidate set.
///
/// Bounded in three dimensions, because a set fed by a LAN multicast any
/// host can send to is a map an unauthorized party grows.
#[derive(Debug, Clone, Default)]
pub struct CandidateSet {
    peers: BTreeMap<TransportIdentity, Entry>,
}

impl CandidateSet {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self {
            peers: BTreeMap::new(),
        }
    }

    /// Candidates currently supported by at least one live source.
    ///
    /// `priority_of` supplies each source's configured priority so the
    /// addresses come back ordered; a source the caller does not know is
    /// treated as the lowest preference rather than dropped, because a
    /// candidate is still a candidate when its provider has been
    /// deregistered mid-flight.
    #[must_use]
    pub fn candidates(
        &self,
        now_ms: u64,
        priority_of: &dyn Fn(&str) -> Option<i32>,
    ) -> Vec<AggregatedCandidate> {
        self.peers
            .iter()
            .filter_map(|(peer_id, entry)| {
                let mut addresses: Vec<AddressProvenance> = Vec::new();
                let mut sources = BTreeSet::new();
                for (address, records) in &entry.addresses {
                    let live: Vec<&Provenance> =
                        records.iter().filter(|r| now_ms < r.expires_at).collect();
                    if live.is_empty() {
                        continue;
                    }
                    let mut per_address: Vec<(i32, &Provenance)> = live
                        .iter()
                        .map(|r| (priority_of(&r.source).unwrap_or(i32::MAX), *r))
                        .collect();
                    per_address.sort_by_key(|(priority, r)| (*priority, r.source.clone()));
                    for (_, record) in &per_address {
                        sources.insert(record.source.clone());
                    }
                    addresses.push(AddressProvenance {
                        address: address.clone(),
                        best_priority: per_address.first().map_or(i32::MAX, |(p, _)| *p),
                        observed_at_ms: per_address
                            .iter()
                            .map(|(_, r)| r.observed_at)
                            .max()
                            .unwrap_or(0),
                        sources: per_address
                            .into_iter()
                            .map(|(_, r)| r.source.clone())
                            .collect(),
                    });
                }
                // A PEER WITH NO LIVE ADDRESS IS NOT A CANDIDATE. Protocol
                // observations are deliberately not consulted here: they
                // never keep a peer alive (`COMPOSITION.md`).
                if addresses.is_empty() {
                    return None;
                }
                // Best priority first; ties broken by the address itself
                // so the order is deterministic rather than map-dependent.
                addresses.sort_by(|a, b| {
                    a.best_priority
                        .cmp(&b.best_priority)
                        .then_with(|| a.address.cmp(&b.address))
                });
                // Merged by protocol: one entry per (protocol, supported)
                // claim, naming every source still asserting it. Expired
                // observations are dropped by the same `now_ms` the
                // addresses are judged against, so a consumer never reads
                // a fact this manager would no longer stand behind.
                let mut merged: BTreeMap<(ProtocolId, bool), MergedObservation> = BTreeMap::new();
                for ((protocol, source), (supported, at, exp)) in &entry.observations {
                    if now_ms >= *exp {
                        continue;
                    }
                    let slot = merged
                        .entry((protocol.clone(), *supported))
                        .or_insert_with(|| MergedObservation {
                            protocol: protocol.clone(),
                            supported: *supported,
                            sources: Vec::new(),
                            observed_at_ms: 0,
                        });
                    slot.sources.push(source.clone());
                    slot.observed_at_ms = slot.observed_at_ms.max(*at);
                }
                let protocol_observations: Vec<MergedObservation> = merged.into_values().collect();

                Some(AggregatedCandidate {
                    peer_id: peer_id.clone(),
                    addresses,
                    sources,
                    last_observed_ms: entry.recency(now_ms),
                    protocol_observations,
                })
            })
            .collect()
    }

    /// Peers held, live or not.
    #[must_use]
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    /// Whether nothing is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Drop every provenance record and observation that has expired, and
    /// every peer left with no addresses.
    pub fn sweep(&mut self, now_ms: u64) {
        for entry in self.peers.values_mut() {
            for records in entry.addresses.values_mut() {
                records.retain(|r| now_ms < r.expires_at);
            }
            entry.addresses.retain(|_, records| !records.is_empty());
            entry.observations.retain(|_, (_, _, exp)| now_ms < *exp);
        }
        self.peers.retain(|_, e| !e.addresses.is_empty());
    }

    /// Record an observation from `source`.
    ///
    /// The observation's lifetime is its own `expires_at` when the
    /// provider expresses one, else `now_ms + DEFAULT_OBSERVATION_TTL_MS`.
    fn observe(
        &mut self,
        candidate: &CandidatePeer,
        now_ms: u64,
        trust: &PeerTrustPolicy,
        provider_expires: bool,
        provider_pinned: bool,
    ) {
        // A PROVIDER THAT DECLARES NO EXPIRY IS RETRACTED, NEVER AGED OUT.
        // Static bootstrap emits its entries once, at start and on
        // reload; a default TTL here would delete them from a node that
        // is still configured with them and would never re-learn, because
        // nothing changed and so nothing is re-emitted. Such a provider
        // says what it means with `CandidateExpired`.
        let expires_at = match candidate.expires_at {
            Some(stated) => stated,
            None if provider_expires => now_ms.saturating_add(DEFAULT_OBSERVATION_TTL_MS),
            None => u64::MAX,
        };

        // AN ADDRESSLESS OBSERVATION BUYS NOTHING, so it may not spend a
        // slot. `CandidatePeer::validate` permits an empty address set,
        // and protocol observations are explicitly not allowed to keep a
        // peer alive (COMPOSITION.md) — so such an entry never appears in
        // `candidates()`. Admitting one under pressure evicted a live
        // untrusted candidate and replaced its reachability with nothing,
        // which is a way to clear the set using events that are
        // individually valid.
        //
        // A peer ALREADY known still takes the event: its observations
        // merge, and it is holding its slot on addresses it already has.
        // NOTHING LIVE BUYS NOTHING, so it may not spend a slot. Two
        // shapes reach this with the same effect: no addresses at all,
        // and addresses whose stated lifetime has already passed. A cache
        // event can legitimately sit in a queue past its own TTL, so the
        // second is ordinary rather than hostile — and either way the
        // entry is invisible in `candidates()` while having cost a live
        // candidate its slot.
        //
        // A peer ALREADY known still takes the event: its observations
        // merge, its recency advances, and it is holding its
        // slot on addresses it already has.
        let contributes_nothing = candidate.addresses.is_empty() || expires_at <= now_ms;
        if contributes_nothing && !self.peers.contains_key(&candidate.peer_id) {
            return;
        }

        if !self.peers.contains_key(&candidate.peer_id) && self.peers.len() >= MAX_CANDIDATES {
            self.evict_one(now_ms, trust);
            if self.peers.len() >= MAX_CANDIDATES {
                // Nothing could be evicted — every slot is a live trusted
                // candidate. Refusing the new one is correct: the bound is
                // the bound, and dropping a trusted peer for an unknown
                // one is what an attacker would want.
                return;
            }
        }

        let entry = self.peers.entry(candidate.peer_id.clone()).or_default();

        // A DEAD SLOT IS NOT AN OCCUPIED ONE. The cap counts address
        // KEYS, and a key whose every provenance record has expired is
        // still a key until `sweep` runs — so under pump-then-sweep a
        // peer could hold sixteen dead addresses and reject a live one.
        // The sweep afterwards frees the slot but cannot recover the
        // rejected address, and a provider that already considers its
        // snapshot emitted will not offer it again.
        //
        // Pruning here rather than relying on sweep ordering makes the
        // cap mean "sixteen addresses a source still vouches for", which
        // is what it was always meant to say.
        entry.addresses.retain(|_, records| {
            records.retain(|r| now_ms < r.expires_at);
            !records.is_empty()
        });

        for address in &candidate.addresses {
            if !entry.addresses.contains_key(address)
                && entry.addresses.len() >= MAX_ADDRESSES_PER_PEER
            {
                // INSERTION ORDER IS NOT A QUALITY RANKING. Dropping the
                // incoming address because the slots happen to be full
                // silently loses an operator's deterministic route when
                // LAN or cache observations reached the peer first — and
                // static bootstrap will not offer it again without a
                // reload, so "later" became "never".
                //
                // A pinned address therefore displaces an unpinned one,
                // oldest first. Nothing else displaces anything: between
                // two addresses of the same standing the earlier one has
                // at least been seen, so the bound holds as before.
                if !provider_pinned {
                    continue;
                }
                let victim = entry
                    .addresses
                    .iter()
                    .filter(|(_, rs)| !rs.iter().any(|r| r.pinned))
                    .min_by_key(|(addr, rs)| {
                        (
                            rs.iter().map(|r| r.observed_at).max().unwrap_or(0),
                            (*addr).clone(),
                        )
                    })
                    .map(|(addr, _)| addr.clone());
                match victim {
                    Some(addr) => {
                        entry.addresses.remove(&addr);
                    }
                    // Every slot is already pinned: the bound is the
                    // bound, and configuration cannot grow it.
                    None => continue,
                }
            }
            let records = entry.addresses.entry(address.clone()).or_default();
            // ONE RECORD PER SOURCE. Re-observing refreshes that source's
            // lifetime and leaves every other source's alone, which is
            // what makes "the address dies when no source supports it"
            // mean something.
            if let Some(existing) = records.iter_mut().find(|r| r.source == candidate.source) {
                // ONLY FORWARD. An older event delivered after a newer one
                // would otherwise roll this source's timestamp and expiry
                // BACKWARD — and since a known peer deliberately accepts
                // an already-expired candidate (so its observations still
                // merge), a delayed stale event could retire a live
                // address, or the whole peer when it was the only source
                // supporting it. That path exists because of the
                // addressless/expired guard added earlier in this branch,
                // so the guard owes this check.
                //
                // Same rule as the protocol observations below, for the
                // same reason: evidence is ordered by when it was
                // observed, not by when it arrived.
                if candidate.observed_at >= existing.observed_at {
                    existing.observed_at = candidate.observed_at;
                    existing.expires_at = expires_at;
                }
            } else {
                // THE SAME ASYMMETRY AS THE ADDRESS SLOTS, one level
                // down. At the cap a ninth source was dropped by arrival
                // order — so a configured non-expiring provider reporting
                // an address eight others already claim lost its record,
                // and when those eight expired the address went with them
                // even though it is still configured. Static will not
                // re-emit it without a reload.
                if records.len() >= MAX_PROVENANCE_PER_ADDRESS {
                    if !provider_pinned {
                        continue;
                    }
                    // Displace an unpinned record, oldest first. If every
                    // record is pinned the cap stands: configuration does
                    // not grow a bound.
                    let victim = records
                        .iter()
                        .enumerate()
                        .filter(|(_, r)| !r.pinned)
                        .min_by_key(|(_, r)| r.observed_at)
                        .map(|(i, _)| i);
                    match victim {
                        Some(i) => {
                            records.remove(i);
                        }
                        None => continue,
                    }
                }
                records.push(Provenance {
                    source: candidate.source.clone(),
                    observed_at: candidate.observed_at,
                    expires_at,
                    pinned: provider_pinned,
                });
            }
        }

        // A DEAD SLOT IS NOT AN OCCUPIED ONE — the same rule the address
        // cap follows, for the same reason. An expired observation is
        // still a map entry until `sweep` runs, so under pump-then-sweep
        // a peer with sixteen lapsed facts rejected a live one, and the
        // sweep afterwards frees the slot but cannot recover what was
        // refused.
        entry.observations.retain(|_, (_, _, exp)| now_ms < *exp);

        for observation in &candidate.protocol_observations {
            let key = (observation.protocol_id.clone(), candidate.source.clone());
            if !entry.observations.contains_key(&key)
                && entry.observations.len() >= MAX_OBSERVATIONS_PER_PEER
            {
                continue;
            }
            // THE NEWEST EVIDENCE WINS, not the last one iterated.
            // `protocol_observations` is a `BTreeSet`, so a candidate may
            // legally carry both `supported: true` and `supported: false`
            // for one protocol and the derived ordering decides which is
            // applied last — and an event delivered late can overwrite
            // fresher evidence with stale. Either way a consumer could be
            // told a peer supports something a newer observation had
            // already withdrawn.
            match entry.observations.get(&key) {
                Some((_, held_at, _)) if *held_at > observation.observed_at => {}
                _ => {
                    entry.observations.insert(
                        key,
                        (observation.supported, observation.observed_at, expires_at),
                    );
                }
            }
        }
    }

    /// Retract `source`'s support: the named addresses, or all of them.
    fn retract(&mut self, peer_id: &TransportIdentity, source: &str, addresses: &BTreeSet<String>) {
        let Some(entry) = self.peers.get_mut(peer_id) else {
            return;
        };
        for (address, records) in &mut entry.addresses {
            if addresses.is_empty() || addresses.contains(address) {
                records.retain(|r| r.source != source);
            }
        }
        entry.addresses.retain(|_, records| !records.is_empty());

        // A SOURCE THAT SUPPORTS NOTHING KEEPS NO FACTS. Dropping
        // observations only on a whole-peer retraction was too narrow: a
        // source with one address that selectively retracts it has no
        // remaining provenance either, and if another provider still
        // supplies an address the peer survives — carrying the departed
        // source's protocol claims until their original TTL, asserted by
        // something that no longer vouches for a single way to reach the
        // peer.
        //
        // Asking whether the source has any provenance left covers both
        // shapes, so the whole-peer case is no longer special.
        let still_supports = entry
            .addresses
            .values()
            .any(|records| records.iter().any(|r| r.source == source));
        if !still_supports {
            entry.observations.retain(|(_, s), _| s != source);
        }

        if entry.addresses.is_empty() {
            self.peers.remove(peer_id);
        }
    }

    /// Make room: an expired peer first, then the least recently observed
    /// UNTRUSTED one.
    ///
    /// Trust is read for ORDER and nothing else (ADR-0012). A trusted peer
    /// is preferred over an untrusted one under pressure — it is not
    /// granted anything, and an untrusted candidate that survives is not
    /// thereby endorsed.
    fn evict_one(&mut self, now_ms: u64, trust: &PeerTrustPolicy) {
        let expired: Option<TransportIdentity> = self
            .peers
            .iter()
            .find(|(_, e)| {
                e.addresses
                    .values()
                    .all(|rs| rs.iter().all(|r| now_ms >= r.expires_at))
            })
            .map(|(p, _)| p.clone());
        if let Some(peer) = expired {
            self.peers.remove(&peer);
            return;
        }
        // CONFIGURED ENTRIES ARE RETAINED WITHIN THEIR OWN CAP
        // (`architecture/discovery/DESIGN.md`). Static bootstrap is the
        // case that makes this load-bearing: its provider declares no
        // expiry and emits only at start and on reload, so an evicted
        // configured entry is not re-learned — it is gone until something
        // reloads the provider. Left in the general pool it would also be
        // evicted FIRST under churn, because a peer observed once at start
        // is by definition the least recently observed.
        //
        // Beyond the cap the protection stops: retention is bounded like
        // everything else here, so configuration cannot be used to pin the
        // whole set out of reach of eviction.
        let configured: BTreeSet<TransportIdentity> = self
            .peers
            .iter()
            .filter(|(_, e)| e.is_pinned())
            .map(|(p, _)| p.clone())
            .collect();
        let protect: BTreeSet<&TransportIdentity> = if configured.len() <= MAX_CONFIGURED_RETAINED {
            configured.iter().collect()
        } else {
            // Over the cap: protect the most recently observed within it,
            // so the excess is evictable in the same order as anything
            // else.
            let mut ranked: Vec<&TransportIdentity> = configured.iter().collect();
            ranked.sort_by_key(|p| {
                std::cmp::Reverse(self.peers.get(*p).map_or(0, |e| e.recency(now_ms)))
            });
            ranked.truncate(MAX_CONFIGURED_RETAINED);
            ranked.into_iter().collect()
        };

        let victim = self
            .peers
            .iter()
            .filter(|(peer, _)| !trust.decide(peer).is_allowed())
            .filter(|(peer, _)| !protect.contains(peer))
            .min_by_key(|(_, e)| e.recency(now_ms))
            .map(|(p, _)| p.clone());
        if let Some(peer) = victim {
            self.peers.remove(&peer);
        }
    }
}

/// One registered provider's identity and composition settings.
#[derive(Debug, Clone)]
struct Registered {
    descriptor: ProviderDescriptor,
    /// Guidance for address selection, never trust (ADR-0007).
    priority: i32,
    health: ProviderHealth,
}

/// Composes providers and owns the merged candidate set.
///
/// The manager never holds the providers themselves: it is a pure state
/// machine, and whoever owns the provider objects drains them and hands
/// the events here. That keeps every I/O-shaped concern — tasks, sockets,
/// files — outside a module whose rules are worth testing by enumeration.
#[derive(Debug, Default)]
pub struct DiscoveryManager {
    providers: BTreeMap<String, Registered>,
    candidates: CandidateSet,
}

impl DiscoveryManager {
    /// An empty manager.
    #[must_use]
    pub fn new() -> Self {
        Self {
            providers: BTreeMap::new(),
            candidates: CandidateSet::new(),
        }
    }

    /// Register a provider under its descriptor's name.
    ///
    /// The descriptor is validated here: a provider whose name breaks the
    /// contract's bounds cannot be composed, and the name is what every
    /// later provenance check compares against.
    ///
    /// # Errors
    /// The descriptor's own validation error, or `None` when the registry
    /// is full — `MAX_PROVIDERS` mirrors the config schema's
    /// `providers: list[ProviderConfig, max=16]`.
    pub fn register(
        &mut self,
        descriptor: ProviderDescriptor,
        priority: i32,
    ) -> Result<(), interweave_discovery_api::DiscoveryError> {
        descriptor.validate()?;
        if !self.providers.contains_key(&descriptor.name) && self.providers.len() >= MAX_PROVIDERS {
            return Err(interweave_discovery_api::DiscoveryError::TooManyItems {
                field: "providers",
                got: self.providers.len() + 1,
                max: MAX_PROVIDERS,
            });
        }
        self.providers.insert(
            descriptor.name.clone(),
            Registered {
                descriptor,
                priority,
                // A registered provider has not started yet, and
                // `DISCOVERY.md` gives a provider no events before start.
                health: ProviderHealth::Unavailable,
            },
        );
        Ok(())
    }

    /// Providers registered.
    #[must_use]
    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }

    /// One provider's last reported health, if it is registered.
    #[must_use]
    pub fn provider_health(&self, name: &str) -> Option<ProviderHealth> {
        self.providers.get(name).map(|p| p.health)
    }

    /// A provider's composition priority, if it is registered.
    #[must_use]
    pub fn provider_priority(&self, name: &str) -> Option<i32> {
        self.providers.get(name).map(|p| p.priority)
    }

    /// Aggregate discovery health (`DISCOVERY.md`).
    ///
    /// Healthy when any provider is healthy — the transport can be fine
    /// with one provider degraded, and reporting the worst would make a
    /// disabled-multicast laptop look broken. Unavailable only when every
    /// provider is, which is the state that actually means "no discovery".
    #[must_use]
    pub fn aggregate_health(&self) -> ProviderHealth {
        if self.providers.is_empty() {
            return ProviderHealth::Unavailable;
        }
        if self
            .providers
            .values()
            .any(|p| p.health == ProviderHealth::Healthy)
        {
            ProviderHealth::Healthy
        } else if self
            .providers
            .values()
            .any(|p| p.health == ProviderHealth::Degraded)
        {
            ProviderHealth::Degraded
        } else {
            ProviderHealth::Unavailable
        }
    }

    /// Take one event from `source`.
    ///
    /// # Errors
    /// [`RejectedEvent`] when the provider is unknown, the event's own
    /// source does not match it, or a candidate fails validation. A
    /// refusal changes nothing: a malformed event from one provider must
    /// not disturb another's state (conformance: failure isolation).
    pub fn on_event(
        &mut self,
        source: &str,
        event: DiscoveryEvent,
        now_ms: u64,
        trust: &PeerTrustPolicy,
    ) -> Result<(), RejectedEvent> {
        if !self.providers.contains_key(source) {
            return Err(RejectedEvent::UnknownProvider);
        }
        match event {
            DiscoveryEvent::CandidateObserved { candidate } => {
                if candidate.source != source {
                    return Err(RejectedEvent::SourceMismatch {
                        expected: source.to_owned(),
                        got: candidate.source.clone(),
                    });
                }
                candidate
                    .validate()
                    .map_err(|_| RejectedEvent::InvalidCandidate)?;
                let provider_expires = self
                    .providers
                    .get(source)
                    .is_some_and(|p| p.descriptor.supports_expiry);
                // PINNED, not merely configured-scope. `PeerCacheDiscovery`
                // also declares `ProviderScope::Configured`, so scope alone
                // protected cached observations too — and once more than
                // MAX_CONFIGURED_RETAINED recent cache peers coexisted with
                // older static ones, the ranking protected the cache entries
                // and made the static bootstrap ones evictable, which is the
                // opposite of the rule.
                //
                // The property retention actually rests on is that the
                // provider will not emit the entry again: `supports_expiry:
                // false` means it is RETRACTED or it stands, and static
                // bootstrap emits only at start and on reload. A cache
                // record ages out and is re-emitted from disk, so losing one
                // to eviction costs a refresh, not the entry.
                let provider_pinned = self.providers.get(source).is_some_and(|p| {
                    p.descriptor.scope == ProviderScope::Configured && !p.descriptor.supports_expiry
                });
                self.candidates.observe(
                    &candidate,
                    now_ms,
                    trust,
                    provider_expires,
                    provider_pinned,
                );
                Ok(())
            }
            DiscoveryEvent::CandidateExpired {
                peer_id,
                source: event_source,
                addresses,
            } => {
                if event_source != source {
                    return Err(RejectedEvent::SourceMismatch {
                        expected: source.to_owned(),
                        got: event_source,
                    });
                }
                self.candidates.retract(&peer_id, source, &addresses);
                Ok(())
            }
            DiscoveryEvent::HealthChanged {
                source: event_source,
                health,
            } => {
                if event_source != source {
                    return Err(RejectedEvent::SourceMismatch {
                        expected: source.to_owned(),
                        got: event_source,
                    });
                }
                if let Some(registered) = self.providers.get_mut(source) {
                    registered.health = health;
                }
                Ok(())
            }
        }
    }

    /// Drop everything that has expired at `now_ms`.
    pub fn sweep(&mut self, now_ms: u64) {
        self.candidates.sweep(now_ms);
    }

    /// The merged candidates a consumer may consider.
    ///
    /// ADVISORY. Nothing here has been dialled, trusted, or promised to be
    /// reachable; a consumer still passes every dial through admission.
    #[must_use]
    pub fn candidates(&self, now_ms: u64) -> Vec<AggregatedCandidate> {
        self.candidates.candidates(now_ms, &|source| {
            self.providers.get(source).map(|p| p.priority)
        })
    }

    /// Peers held in the merged set, live or not.
    #[must_use]
    pub fn candidate_count(&self) -> usize {
        self.candidates.len()
    }

    /// The descriptor a provider registered with.
    #[must_use]
    pub fn descriptor(&self, name: &str) -> Option<&ProviderDescriptor> {
        self.providers.get(name).map(|p| &p.descriptor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interweave_discovery_api::{ProtocolObservation, ProviderMode, ProviderScope};

    const P1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    const P2: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";
    const P3: &str = "12D3KooWQYV9dGMFoRzNStwpXztXaBUjtPqi6aU76ZgUriHhKust";

    /// A synthetic well-formed identity, for tests that need more peers
    /// than there are named constants.
    ///
    /// The PeerId grammar this crate checks is a prefix, an alphabet and a
    /// length with no checksum (`TransportIdentity::parse`), so a
    /// generated string is exactly as valid to every layer under test as a
    /// captured one. Nothing here dials, so a peer that no key backs is
    /// the right test subject rather than a shortcut.
    fn identity(n: usize) -> TransportIdentity {
        const B58: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
        let mut tail = [b'1'; 44];
        let mut v = n;
        let mut i = 43;
        loop {
            tail[i] = B58[v % B58.len()];
            v /= B58.len();
            if v == 0 || i == 0 {
                break;
            }
            i -= 1;
        }
        let s = format!("12D3KooW{}", core::str::from_utf8(&tail).expect("ascii"));
        TransportIdentity::parse(s).expect("the generated id matches the grammar")
    }

    fn peer(s: &str) -> TransportIdentity {
        TransportIdentity::parse(s).expect("valid identity")
    }
    fn trusting(peers: &[&str]) -> PeerTrustPolicy {
        PeerTrustPolicy::new(peers.iter().map(|p| peer(p)).collect::<Vec<_>>()).expect("policy")
    }
    fn nobody() -> PeerTrustPolicy {
        PeerTrustPolicy::new(Vec::new()).expect("policy")
    }
    /// A descriptor for a provider that does NOT model expiry, like
    /// static bootstrap.
    fn descriptor_without_expiry(name: &str) -> ProviderDescriptor {
        ProviderDescriptor {
            supports_expiry: false,
            ..descriptor(name)
        }
    }

    fn descriptor(name: &str) -> ProviderDescriptor {
        ProviderDescriptor {
            name: name.to_owned(),
            interface_version: "1.0".to_owned(),
            config_version: None,
            scope: ProviderScope::Configured,
            mode: ProviderMode::Passive,
            supports_expiry: true,
            supports_hints: false,
        }
    }
    fn candidate(
        p: &str,
        source: &str,
        addrs: &[&str],
        at: u64,
        exp: Option<u64>,
    ) -> CandidatePeer {
        CandidatePeer {
            peer_id: peer(p),
            addresses: addrs.iter().map(|a| (*a).to_owned()).collect(),
            source: source.to_owned(),
            observed_at: at,
            expires_at: exp,
            protocol_observations: BTreeSet::new(),
        }
    }
    /// `candidate`, for an identity that is already parsed.
    fn for_id(
        id: &TransportIdentity,
        source: &str,
        addr: &str,
        at: u64,
        exp: Option<u64>,
    ) -> CandidatePeer {
        CandidatePeer {
            peer_id: id.clone(),
            addresses: [addr.to_owned()].into_iter().collect(),
            source: source.to_owned(),
            observed_at: at,
            expires_at: exp,
            protocol_observations: BTreeSet::new(),
        }
    }

    fn observed(c: CandidatePeer) -> DiscoveryEvent {
        DiscoveryEvent::CandidateObserved {
            candidate: Box::new(c),
        }
    }
    fn manager_with(names: &[&str]) -> DiscoveryManager {
        let mut m = DiscoveryManager::new();
        for n in names {
            m.register(descriptor(n), 0).expect("registers");
        }
        m
    }

    #[test]
    fn two_providers_reporting_one_peer_merge_into_one_candidate() {
        let mut m = manager_with(&["a", "b"]);
        m.on_event(
            "a",
            observed(candidate(P1, "a", &["/addr/1"], 0, None)),
            0,
            &nobody(),
        )
        .expect("accepted");
        m.on_event(
            "b",
            observed(candidate(P1, "b", &["/addr/2"], 0, None)),
            0,
            &nobody(),
        )
        .expect("accepted");

        let c = m.candidates(0);
        assert_eq!(c.len(), 1, "one peer, not one per provider");
        assert_eq!(c[0].addresses.len(), 2, "address sets merge");
        assert_eq!(
            c[0].sources,
            ["a".to_owned(), "b".to_owned()].into_iter().collect(),
            "both sources vouch for it"
        );
    }

    #[test]
    fn an_address_survives_until_no_live_source_supports_it() {
        // The rule COMPOSITION.md states, and the reason provenance is
        // per source rather than a single lifetime per address: `a`
        // expires at 100, `b` at 200, so the shared address lives to 200.
        let mut m = manager_with(&["a", "b"]);
        m.on_event(
            "a",
            observed(candidate(P1, "a", &["/shared"], 0, Some(100))),
            0,
            &nobody(),
        )
        .expect("accepted");
        m.on_event(
            "b",
            observed(candidate(P1, "b", &["/shared"], 0, Some(200))),
            0,
            &nobody(),
        )
        .expect("accepted");

        assert_eq!(m.candidates(150).len(), 1, "b still supports it at 150");
        assert_eq!(
            m.candidates(150)[0].sources,
            ["b".to_owned()].into_iter().collect(),
            "and only b does"
        );
        assert!(m.candidates(200).is_empty(), "no live source at 200");
    }

    #[test]
    fn a_peer_disappears_when_its_last_address_does() {
        let mut m = manager_with(&["a"]);
        m.on_event(
            "a",
            observed(candidate(P1, "a", &["/one"], 0, Some(50))),
            0,
            &nobody(),
        )
        .expect("accepted");
        assert_eq!(m.candidates(0).len(), 1);
        assert!(m.candidates(50).is_empty());
        m.sweep(50);
        assert_eq!(m.candidate_count(), 0, "swept, not merely hidden");
    }

    #[test]
    fn protocol_observations_never_keep_a_peer_alive() {
        // COMPOSITION.md: observations are facts about a peer, not
        // evidence it is still reachable. A peer whose addresses all
        // expired is gone even with a live observation attached.
        let mut m = manager_with(&["a"]);
        let mut c = candidate(P1, "a", &["/one"], 0, Some(50));
        c.protocol_observations.insert(ProtocolObservation {
            protocol_id: ProtocolId::parse("/interweave/direct/2.0.0").expect("valid"),
            supported: true,
            observed_at: 0,
        });
        m.on_event("a", observed(c), 0, &nobody())
            .expect("accepted");
        assert!(
            m.candidates(50).is_empty(),
            "the observation must not resurrect the peer"
        );
    }

    #[test]
    fn re_observing_refreshes_only_that_sources_lifetime() {
        let mut m = manager_with(&["a", "b"]);
        m.on_event(
            "a",
            observed(candidate(P1, "a", &["/shared"], 0, Some(100))),
            0,
            &nobody(),
        )
        .expect("accepted");
        m.on_event(
            "b",
            observed(candidate(P1, "b", &["/shared"], 0, Some(100))),
            0,
            &nobody(),
        )
        .expect("accepted");
        // `a` re-observes at 90 with a longer life; `b` is untouched.
        m.on_event(
            "a",
            observed(candidate(P1, "a", &["/shared"], 90, Some(500))),
            90,
            &nobody(),
        )
        .expect("accepted");

        let at_200 = m.candidates(200);
        assert_eq!(at_200.len(), 1);
        assert_eq!(
            at_200[0].sources,
            ["a".to_owned()].into_iter().collect(),
            "b's record expired on its own schedule"
        );
    }

    #[test]
    fn a_selective_retraction_drops_only_the_named_address() {
        let mut m = manager_with(&["a"]);
        m.on_event(
            "a",
            observed(candidate(P1, "a", &["/one", "/two"], 0, None)),
            0,
            &nobody(),
        )
        .expect("accepted");
        m.on_event(
            "a",
            DiscoveryEvent::CandidateExpired {
                peer_id: peer(P1),
                source: "a".to_owned(),
                addresses: ["/one".to_owned()].into_iter().collect(),
            },
            1,
            &nobody(),
        )
        .expect("accepted");

        let c = m.candidates(1);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].address_list(), ["/two"], "only the named address went");
    }

    #[test]
    fn an_empty_retraction_drops_the_whole_source() {
        let mut m = manager_with(&["a", "b"]);
        m.on_event(
            "a",
            observed(candidate(P1, "a", &["/one"], 0, None)),
            0,
            &nobody(),
        )
        .expect("accepted");
        m.on_event(
            "b",
            observed(candidate(P1, "b", &["/two"], 0, None)),
            0,
            &nobody(),
        )
        .expect("accepted");
        m.on_event(
            "a",
            DiscoveryEvent::CandidateExpired {
                peer_id: peer(P1),
                source: "a".to_owned(),
                addresses: BTreeSet::new(),
            },
            1,
            &nobody(),
        )
        .expect("accepted");

        let c = m.candidates(1);
        assert_eq!(c[0].sources, ["b".to_owned()].into_iter().collect());
        assert_eq!(c[0].address_list(), ["/two"]);
    }

    #[test]
    fn an_event_naming_another_providers_source_is_refused() {
        // Provenance (conformance #12): a provider cannot launder a
        // candidate's origin by stamping someone else's name.
        let mut m = manager_with(&["a", "b"]);
        let err = m
            .on_event(
                "a",
                observed(candidate(P1, "b", &["/x"], 0, None)),
                0,
                &nobody(),
            )
            .expect_err("a cannot speak as b");
        assert_eq!(
            err,
            RejectedEvent::SourceMismatch {
                expected: "a".to_owned(),
                got: "b".to_owned()
            }
        );
        assert!(m.candidates(0).is_empty(), "and nothing was recorded");
    }

    #[test]
    fn an_unregistered_provider_is_refused() {
        let mut m = manager_with(&["a"]);
        assert_eq!(
            m.on_event(
                "ghost",
                observed(candidate(P1, "ghost", &["/x"], 0, None)),
                0,
                &nobody()
            ),
            Err(RejectedEvent::UnknownProvider)
        );
    }

    #[test]
    fn an_invalid_candidate_is_refused_and_changes_nothing() {
        let mut m = manager_with(&["a"]);
        m.on_event(
            "a",
            observed(candidate(P1, "a", &["/good"], 0, None)),
            0,
            &nobody(),
        )
        .expect("accepted");
        // expires_at before observed_at: the candidate's own validation
        // refuses it, and the good one is untouched.
        let bad = candidate(P2, "a", &["/bad"], 100, Some(50));
        assert_eq!(
            m.on_event("a", observed(bad), 100, &nobody()),
            Err(RejectedEvent::InvalidCandidate)
        );
        let c = m.candidates(0);
        assert_eq!(c.len(), 1, "the earlier candidate survives");
        assert_eq!(c[0].peer_id, peer(P1));
    }

    #[test]
    fn an_expiring_provider_that_states_no_lifetime_still_gets_a_bounded_one() {
        // A provider that CAN express expiry but did not for this
        // candidate gets the manager's bound — `None` is "no stated
        // expiry", not "permanent". The provider that declares
        // `supports_expiry: false` is the other case, covered above.
        let mut m = manager_with(&["a"]);
        m.on_event(
            "a",
            observed(candidate(P1, "a", &["/one"], 0, None)),
            0,
            &nobody(),
        )
        .expect("accepted");
        assert_eq!(m.candidates(DEFAULT_OBSERVATION_TTL_MS - 1).len(), 1);
        assert!(
            m.candidates(DEFAULT_OBSERVATION_TTL_MS).is_empty(),
            "an unexpiring provider does not produce an eternal candidate"
        );
    }

    #[test]
    fn a_provider_that_declares_no_expiry_is_retracted_not_aged_out() {
        // Static bootstrap emits its entries at start and on reload only.
        // Ageing them out on a timer would delete a configured bootstrap
        // peer from a long-running node that is still configured with it,
        // and nothing would re-emit it because nothing changed.
        let mut m = DiscoveryManager::new();
        m.register(descriptor_without_expiry("static-bootstrap"), 30)
            .expect("registers");
        m.on_event(
            "static-bootstrap",
            observed(candidate(
                P1,
                "static-bootstrap",
                &["/dns4/host.example/tcp/1"],
                0,
                None,
            )),
            0,
            &nobody(),
        )
        .expect("accepted");

        // Far past any default TTL — a week.
        let a_week = 7 * 24 * 60 * 60 * 1_000;
        assert_eq!(
            m.candidates(a_week).len(),
            1,
            "a configured entry is still configured a week later"
        );

        // It goes when the provider says so, and only then.
        m.on_event(
            "static-bootstrap",
            DiscoveryEvent::CandidateExpired {
                peer_id: peer(P1),
                source: "static-bootstrap".to_owned(),
                addresses: BTreeSet::new(),
            },
            a_week,
            &nobody(),
        )
        .expect("accepted");
        assert!(
            m.candidates(a_week).is_empty(),
            "the retraction is what ends it"
        );
    }

    #[test]
    fn addresses_per_peer_are_bounded() {
        let mut m = manager_with(&["a"]);
        let many: Vec<String> = (0..MAX_ADDRESSES_PER_PEER + 8)
            .map(|i| format!("/addr/{i}"))
            .collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        m.on_event(
            "a",
            observed(candidate(P1, "a", &refs, 0, None)),
            0,
            &nobody(),
        )
        .expect("accepted");
        assert_eq!(m.candidates(0)[0].addresses.len(), MAX_ADDRESSES_PER_PEER);
    }

    #[test]
    fn provenance_per_address_is_bounded() {
        let names: Vec<String> = (0..MAX_PROVENANCE_PER_ADDRESS + 4)
            .map(|i| format!("p{i}"))
            .collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut m = manager_with(&refs);
        for n in &refs {
            m.on_event(
                n,
                observed(candidate(P1, n, &["/shared"], 0, None)),
                0,
                &nobody(),
            )
            .expect("accepted");
        }
        // Every source vouches for the same address, but only the first
        // eight records are kept.
        assert_eq!(
            m.candidates(0)[0].sources.len(),
            MAX_PROVENANCE_PER_ADDRESS,
            "the provenance list is capped"
        );
    }

    #[test]
    fn eviction_takes_an_expired_peer_before_a_live_one() {
        let mut m = manager_with(&["a"]);
        // Fill to the cap: one expired, the rest live.
        m.on_event(
            "a",
            observed(candidate(P1, "a", &["/x"], 0, Some(10))),
            0,
            &nobody(),
        )
        .expect("accepted");
        for i in 0..MAX_CANDIDATES - 1 {
            let id = synthetic(i);
            m.on_event(
                "a",
                observed(candidate(&id, "a", &["/y"], 0, Some(1_000))),
                0,
                &nobody(),
            )
            .expect("accepted");
        }
        assert_eq!(m.candidate_count(), MAX_CANDIDATES);
        // One more, at a time when P1 has expired: P1 is the victim.
        m.on_event(
            "a",
            observed(candidate(P2, "a", &["/z"], 20, Some(1_000))),
            20,
            &nobody(),
        )
        .expect("accepted");
        assert_eq!(m.candidate_count(), MAX_CANDIDATES);
        assert!(
            m.candidates(20).iter().any(|c| c.peer_id == peer(P2)),
            "the new candidate was admitted"
        );
        assert!(
            !m.candidates(20).iter().any(|c| c.peer_id == peer(P1)),
            "the expired one made room"
        );
    }

    #[test]
    fn eviction_prefers_an_untrusted_peer_over_a_trusted_one() {
        // Trust is read for ORDER only (ADR-0012). P1 is trusted and the
        // OLDEST, so a rule that ignored trust would evict it; the rule
        // that reads trust takes the untrusted P2 instead.
        let trust = trusting(&[P1]);
        let mut m = manager_with(&["a"]);
        m.on_event(
            "a",
            observed(candidate(P1, "a", &["/x"], 1, Some(9_000))),
            1,
            &trust,
        )
        .expect("accepted");
        m.on_event(
            "a",
            observed(candidate(P2, "a", &["/y"], 2, Some(9_000))),
            2,
            &trust,
        )
        .expect("accepted");
        for i in 0..MAX_CANDIDATES - 2 {
            let id = synthetic(i);
            m.on_event(
                "a",
                observed(candidate(&id, "a", &["/z"], 5, Some(9_000))),
                5,
                &trust,
            )
            .expect("accepted");
        }
        assert_eq!(m.candidate_count(), MAX_CANDIDATES);

        m.on_event(
            "a",
            observed(candidate(P3, "a", &["/w"], 6, Some(9_000))),
            6,
            &trust,
        )
        .expect("accepted");
        let live: Vec<TransportIdentity> = m.candidates(6).into_iter().map(|c| c.peer_id).collect();
        assert!(live.contains(&peer(P1)), "the trusted peer was kept");
        assert!(!live.contains(&peer(P2)), "the untrusted one was evicted");
    }

    /// A distinct valid PeerId per index, for the bound tests.
    fn synthetic(i: usize) -> String {
        // Ed25519 identity PeerIds are `12D3KooW` + 44 base58 chars. Vary
        // the tail deterministically over the base58 alphabet.
        const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
        let mut s = String::from("12D3KooW");
        let mut n = i;
        for _ in 0..44 {
            s.push(char::from(ALPHABET[n % ALPHABET.len()]));
            n /= ALPHABET.len();
        }
        s
    }

    #[test]
    fn registration_validates_the_descriptor_and_is_bounded() {
        let mut m = DiscoveryManager::new();
        let mut bad = descriptor("x");
        bad.name = String::new(); // empty: the descriptor's own rule
        assert!(m.register(bad, 0).is_err());

        for i in 0..MAX_PROVIDERS {
            m.register(descriptor(&format!("p{i}")), 0)
                .expect("registers");
        }
        assert!(
            m.register(descriptor("one-too-many"), 0).is_err(),
            "the registry mirrors the config schema's max of 16"
        );
        assert_eq!(m.provider_count(), MAX_PROVIDERS);
    }

    #[test]
    fn aggregate_health_follows_the_best_provider() {
        let mut m = manager_with(&["a", "b"]);
        // Registered but unstarted: no events yet, so unavailable.
        assert_eq!(m.aggregate_health(), ProviderHealth::Unavailable);

        m.on_event(
            "a",
            DiscoveryEvent::HealthChanged {
                source: "a".to_owned(),
                health: ProviderHealth::Degraded,
            },
            0,
            &nobody(),
        )
        .expect("accepted");
        assert_eq!(m.aggregate_health(), ProviderHealth::Degraded);

        m.on_event(
            "b",
            DiscoveryEvent::HealthChanged {
                source: "b".to_owned(),
                health: ProviderHealth::Healthy,
            },
            0,
            &nobody(),
        )
        .expect("accepted");
        assert_eq!(
            m.aggregate_health(),
            ProviderHealth::Healthy,
            "one healthy provider is working discovery, however degraded another is"
        );
        assert_eq!(m.provider_health("a"), Some(ProviderHealth::Degraded));
        assert_eq!(m.provider_health("missing"), None);
    }

    #[test]
    fn addresses_are_ordered_by_the_priority_of_the_source_supporting_each() {
        // ADR-0007 makes priority guidance for choosing among a peer's
        // ADDRESSES. A consumer can only apply it if the candidate says
        // which provider supports which address — one merged source set
        // cannot answer that.
        let mut m = DiscoveryManager::new();
        m.register(descriptor("cheap"), 10).expect("registers");
        m.register(descriptor("costly"), 90).expect("registers");
        // THE ADDRESS NAMES SORT THE OTHER WAY. `/aaa` precedes `/zzz`
        // alphabetically, so if the order came from the address rather
        // than the priority this assertion would still pass — which the
        // first version of this test did, and the mutation caught.
        m.on_event(
            "costly",
            observed(candidate(P1, "costly", &["/aaa"], 0, None)),
            0,
            &nobody(),
        )
        .expect("accepted");
        m.on_event(
            "cheap",
            observed(candidate(P1, "cheap", &["/zzz"], 0, None)),
            0,
            &nobody(),
        )
        .expect("accepted");

        let c = &m.candidates(0)[0];
        assert_eq!(
            c.address_list(),
            ["/zzz", "/aaa"],
            "the cheaper provider's address is offered first, against alphabetical order"
        );
        assert_eq!(c.addresses[0].sources, vec!["cheap".to_owned()]);
        assert_eq!(c.addresses[0].best_priority, 10);
        assert_eq!(c.addresses[1].sources, vec!["costly".to_owned()]);
        assert_eq!(c.addresses[1].best_priority, 90);
    }

    #[test]
    fn one_address_from_two_providers_keeps_both_and_takes_the_better() {
        let mut m = DiscoveryManager::new();
        m.register(descriptor("cheap"), 10).expect("registers");
        m.register(descriptor("costly"), 90).expect("registers");
        for source in ["costly", "cheap"] {
            m.on_event(
                source,
                observed(candidate(P1, source, &["/shared"], 0, None)),
                0,
                &nobody(),
            )
            .expect("accepted");
        }
        let c = &m.candidates(0)[0];
        assert_eq!(c.addresses.len(), 1);
        assert_eq!(
            c.addresses[0].sources,
            vec!["cheap".to_owned(), "costly".to_owned()],
            "both vouch, best priority first"
        );
        assert_eq!(c.addresses[0].best_priority, 10);
    }

    #[test]
    fn priority_is_recorded_and_is_not_trust() {
        // ADR-0007: priority is guidance for address selection. It is
        // stored, and it changes nothing about what is admitted.
        let mut m = DiscoveryManager::new();
        m.register(descriptor("low"), 30).expect("registers");
        m.register(descriptor("high"), 10).expect("registers");
        assert_eq!(m.provider_priority("high"), Some(10));
        m.on_event(
            "low",
            observed(candidate(P1, "low", &["/x"], 0, None)),
            0,
            &nobody(),
        )
        .expect("accepted");
        assert_eq!(
            m.candidates(0).len(),
            1,
            "a low-priority provider's candidate is still a candidate"
        );
    }
    #[test]
    fn a_configured_entry_survives_overflow_pressure() {
        // Static bootstrap emits once, at start, and declares no expiry.
        // Left in the general pool it is evicted FIRST under churn — a
        // peer observed once at start is by definition the least recently
        // observed — and nothing re-emits it, so the bootstrap address is
        // gone until a provider reload. DESIGN.md retains it.
        let mut set = CandidateSet::new();
        let trust = nobody();
        let boot = identity(0);

        set.observe(
            &for_id(&boot, "static-bootstrap", "/ip4/10.0.0.1/tcp/1", 0, None),
            0,
            &trust,
            false, // supports_expiry: false
            true,  // scope: Configured
        );

        // Fill the set from a non-configured provider, all observed later.
        for i in 1..=MAX_CANDIDATES {
            set.observe(
                &for_id(
                    &identity(i),
                    "mdns",
                    "/ip4/10.1.0.1/tcp/1",
                    1_000 + i as u64,
                    Some(u64::MAX),
                ),
                1_000 + i as u64,
                &trust,
                true,
                false,
            );
        }

        assert!(
            set.candidates(2_000_000, &|_| None)
                .iter()
                .any(|c| c.peer_id == boot),
            "the configured bootstrap entry is retained under pressure"
        );
    }

    #[test]
    fn configured_entries_past_their_cap_are_evictable() {
        // Retention is bounded like everything else: configuration must
        // not be a way to pin the whole set out of reach of eviction.
        let mut set = CandidateSet::new();
        let trust = nobody();

        for i in 0..MAX_CONFIGURED_RETAINED + 8 {
            set.observe(
                &for_id(
                    &identity(i),
                    "static-bootstrap",
                    "/ip4/10.0.0.1/tcp/1",
                    i as u64,
                    None,
                ),
                i as u64,
                &trust,
                false,
                true,
            );
        }
        let before = set.candidates(1_000_000, &|_| None).len();

        // Now apply pressure with a non-configured provider.
        for i in 1_000..1_000 + MAX_CANDIDATES {
            set.observe(
                &for_id(
                    &identity(i),
                    "mdns",
                    "/ip4/10.1.0.1/tcp/1",
                    100_000 + i as u64,
                    Some(u64::MAX),
                ),
                100_000 + i as u64,
                &trust,
                true,
                false,
            );
        }

        let after = set
            .candidates(1_000_000, &|_| None)
            .iter()
            .filter(|c| c.sources.contains("static-bootstrap"))
            .count();
        assert!(
            after <= MAX_CONFIGURED_RETAINED,
            "at most the cap is protected, got {after} of {before} configured"
        );
        assert!(
            after >= MAX_CONFIGURED_RETAINED,
            "and the cap IS protected, not merely an upper bound: got {after}"
        );
    }
    #[test]
    fn cache_pressure_does_not_displace_a_static_entry() {
        // `PeerCacheDiscovery` also declares `ProviderScope::Configured`,
        // so keying retention on scope protected cache records too. Once
        // more than the cap of RECENT cache peers coexisted with older
        // static ones, the ranking kept the cache entries and made the
        // bootstrap entries evictable — the exact inversion of the rule.
        //
        // Driven through the MANAGER, because the scope-versus-expiry
        // decision lives in `on_event`. A `CandidateSet` test passes the
        // flag in by hand and would agree with either spelling.
        let mut m = DiscoveryManager::new();
        m.register(descriptor_without_expiry("static-bootstrap"), 0)
            .expect("registers");
        m.register(descriptor("peer-cache"), 0).expect("registers");
        m.register(descriptor("mdns"), 0).expect("registers");
        let trust = nobody();
        let boot = identity(0);

        m.on_event(
            "static-bootstrap",
            observed(for_id(
                &boot,
                "static-bootstrap",
                "/ip4/10.0.0.1/tcp/1",
                0,
                None,
            )),
            0,
            &trust,
        )
        .expect("accepted");

        // Far more than the cap of cache peers, every one observed later
        // than the static entry.
        for i in 1..=(MAX_CONFIGURED_RETAINED * 2) {
            let at = 1_000 + i as u64;
            m.on_event(
                "peer-cache",
                observed(for_id(
                    &identity(i),
                    "peer-cache",
                    "/ip4/10.2.0.1/tcp/1",
                    at,
                    Some(u64::MAX),
                )),
                at,
                &trust,
            )
            .expect("accepted");
        }

        // Now enough pressure to force eviction.
        for i in 10_000..10_000 + MAX_CANDIDATES {
            let at = 500_000 + i as u64;
            m.on_event(
                "mdns",
                observed(for_id(
                    &identity(i),
                    "mdns",
                    "/ip4/10.1.0.1/tcp/1",
                    at,
                    Some(u64::MAX),
                )),
                at,
                &trust,
            )
            .expect("accepted");
        }

        assert!(
            m.candidates(900_000).iter().any(|c| c.peer_id == boot),
            "the static entry outranks cache records, which age out and are \
             re-emitted from disk however recent they are"
        );
    }
    #[test]
    fn a_configured_route_is_not_lost_because_the_lan_filled_the_slots() {
        // The operator's deterministic route arrives after mDNS and the
        // cache have already filled the peer's 16 address slots. Dropping
        // it by insertion order loses it for good: static bootstrap does
        // not re-emit without a reload.
        let mut m = DiscoveryManager::new();
        m.register(descriptor("mdns"), 0).expect("registers");
        m.register(descriptor_without_expiry("static-bootstrap"), 0)
            .expect("registers");
        let trust = nobody();
        let subject = identity(7);

        for i in 0..MAX_ADDRESSES_PER_PEER {
            m.on_event(
                "mdns",
                observed(for_id(
                    &subject,
                    "mdns",
                    &format!("/ip4/192.168.1.{i}/tcp/4001"),
                    100 + i as u64,
                    Some(u64::MAX),
                )),
                100 + i as u64,
                &trust,
            )
            .expect("accepted");
        }

        let configured = "/dns4/bootstrap.example.net/tcp/4001";
        m.on_event(
            "static-bootstrap",
            observed(for_id(&subject, "static-bootstrap", configured, 200, None)),
            200,
            &trust,
        )
        .expect("accepted");

        let candidates = m.candidates(1_000);
        let found = candidates
            .iter()
            .find(|c| c.peer_id == subject)
            .expect("the peer is known");
        assert!(
            found.address_list().contains(&configured),
            "the configured route displaced an unpinned one: {:?}",
            found.address_list()
        );
        assert!(
            found.addresses.len() <= MAX_ADDRESSES_PER_PEER,
            "and the per-peer bound still holds: {}",
            found.addresses.len()
        );
    }

    #[test]
    fn an_unpinned_address_does_not_displace_anything() {
        // The control: only a pinned address displaces. Otherwise this
        // becomes a way for LAN traffic to churn a peer's address list.
        let mut m = DiscoveryManager::new();
        m.register(descriptor("mdns"), 0).expect("registers");
        let trust = nobody();
        let subject = identity(9);

        for i in 0..MAX_ADDRESSES_PER_PEER {
            m.on_event(
                "mdns",
                observed(for_id(
                    &subject,
                    "mdns",
                    &format!("/ip4/192.168.1.{i}/tcp/4001"),
                    100 + i as u64,
                    Some(u64::MAX),
                )),
                100 + i as u64,
                &trust,
            )
            .expect("accepted");
        }
        let late = "/ip4/10.9.9.9/tcp/4001";
        m.on_event(
            "mdns",
            observed(for_id(&subject, "mdns", late, 900, Some(u64::MAX))),
            900,
            &trust,
        )
        .expect("accepted");

        let candidates = m.candidates(1_000);
        let found = candidates
            .iter()
            .find(|c| c.peer_id == subject)
            .expect("the peer is known");
        assert!(
            !found.address_list().contains(&late),
            "an unpinned late arrival is still refused at the bound"
        );
        assert_eq!(found.addresses.len(), MAX_ADDRESSES_PER_PEER);
    }
    #[test]
    fn an_addressless_candidate_does_not_evict_a_live_peer() {
        // `validate` accepts an empty address set and observations never
        // keep a peer alive, so the entry would be invisible in
        // `candidates()` — while having cost a live candidate its slot.
        let mut set = CandidateSet::new();
        let trust = nobody();

        for i in 0..MAX_CANDIDATES {
            set.observe(
                &for_id(
                    &identity(i),
                    "mdns",
                    "/ip4/10.1.0.1/tcp/1",
                    1_000 + i as u64,
                    Some(u64::MAX),
                ),
                1_000 + i as u64,
                &trust,
                true,
                false,
            );
        }
        let before = set.candidates(2_000, &|_| None).len();
        assert_eq!(before, MAX_CANDIDATES, "the set is full of live peers");

        let ghost = identity(999_999);
        let mut empty = for_id(&ghost, "mdns", "/ip4/10.0.0.1/tcp/1", 5_000, Some(u64::MAX));
        empty.addresses.clear();
        assert!(empty.validate().is_ok(), "an empty address set is valid");
        set.observe(&empty, 5_000, &trust, true, false);

        assert_eq!(
            set.candidates(5_000, &|_| None).len(),
            before,
            "nothing was displaced for an entry that cannot be dialled"
        );
        assert!(
            !set.candidates(5_000, &|_| None)
                .iter()
                .any(|c| c.peer_id == ghost),
            "and the addressless peer did not take a slot"
        );
    }

    #[test]
    fn an_addressless_observation_still_reaches_a_peer_already_known() {
        // The control: ignoring the event entirely would drop protocol
        // observations for a peer that IS holding a slot on real
        // addresses.
        let mut set = CandidateSet::new();
        let trust = nobody();
        let known = identity(3);
        set.observe(
            &for_id(&known, "mdns", "/ip4/10.0.0.1/tcp/1", 0, Some(u64::MAX)),
            0,
            &trust,
            true,
            false,
        );

        // Asserted on the facts themselves rather than on recency, which
        // is derived from ADDRESS provenance and so does not move for an
        // observation carrying none.
        let mut empty = for_id(&known, "mdns", "/ip4/10.0.0.1/tcp/1", 100, Some(u64::MAX));
        empty.addresses.clear();
        empty.protocol_observations.insert(ProtocolObservation {
            protocol_id: ProtocolId::parse("/interweave/direct/2.0.0").expect("valid"),
            supported: true,
            observed_at: 100,
        });
        set.observe(&empty, 100, &trust, true, false);

        let candidates = set.candidates(200, &|_| None);
        let found = candidates
            .iter()
            .find(|c| c.peer_id == known)
            .expect("the known peer keeps its addresses and its slot");
        assert_eq!(
            found.protocol_observations.len(),
            1,
            "the event is APPLIED, not merely survived: its protocol fact \
             reached the consumer"
        );
    }
    #[test]
    fn merged_protocol_observations_reach_the_consumer() {
        // The manager bounded, expired and retracted these and no
        // consumer could read one — a store rather than a contribution.
        let mut set = CandidateSet::new();
        let trust = nobody();
        let subject = identity(11);

        let mut first = for_id(&subject, "mdns", "/ip4/10.0.0.1/tcp/1", 0, Some(u64::MAX));
        first.protocol_observations.insert(ProtocolObservation {
            protocol_id: ProtocolId::parse("/interweave/direct/2.0.0").expect("valid"),
            supported: true,
            observed_at: 10,
        });
        set.observe(&first, 0, &trust, true, false);

        let candidates = set.candidates(100, &|_| None);
        let found = candidates
            .iter()
            .find(|c| c.peer_id == subject)
            .expect("known");
        assert_eq!(found.protocol_observations.len(), 1, "the fact is readable");
        assert_eq!(
            found.protocol_observations[0].protocol.as_str(),
            "/interweave/direct/2.0.0"
        );
        assert_eq!(found.protocol_observations[0].sources, vec!["mdns"]);
    }

    #[test]
    fn one_protocol_asserted_by_two_sources_merges_naming_both() {
        let mut set = CandidateSet::new();
        let trust = nobody();
        let subject = identity(12);
        let protocol = ProtocolId::parse("/interweave/direct/2.0.0").expect("valid");

        for (source, at) in [("mdns", 10u64), ("peer-cache", 40u64)] {
            let mut c = for_id(&subject, source, "/ip4/10.0.0.1/tcp/1", at, Some(u64::MAX));
            c.protocol_observations.insert(ProtocolObservation {
                protocol_id: protocol.clone(),
                supported: true,
                observed_at: at,
            });
            set.observe(&c, at, &trust, true, false);
        }

        let candidates = set.candidates(100, &|_| None);
        let found = candidates
            .iter()
            .find(|c| c.peer_id == subject)
            .expect("known");
        assert_eq!(
            found.protocol_observations.len(),
            1,
            "one protocol, one entry"
        );
        assert_eq!(
            found.protocol_observations[0].sources,
            vec!["mdns", "peer-cache"],
            "naming every source still asserting it"
        );
        assert_eq!(
            found.protocol_observations[0].observed_at_ms, 40,
            "carrying the most recent assertion"
        );
    }

    #[test]
    fn an_expired_protocol_observation_is_not_exposed() {
        // Judged against the same `now_ms` as the addresses, so a
        // consumer never reads a fact the manager would not stand behind.
        let mut set = CandidateSet::new();
        let trust = nobody();
        let subject = identity(13);

        // A short-lived source carries the fact; a long-lived one keeps
        // the peer present. At `late` the first has lapsed and the second
        // has not, so the peer must remain with no facts attached.
        let mut short = for_id(&subject, "mdns", "/ip4/10.0.0.1/tcp/1", 0, Some(50));
        short.protocol_observations.insert(ProtocolObservation {
            protocol_id: ProtocolId::parse("/interweave/direct/2.0.0").expect("valid"),
            supported: true,
            observed_at: 0,
        });
        set.observe(&short, 0, &trust, true, false);
        set.observe(
            &for_id(
                &subject,
                "peer-cache",
                "/ip4/10.0.0.2/tcp/2",
                0,
                Some(u64::MAX),
            ),
            0,
            &trust,
            true,
            false,
        );

        let late = 100;
        let candidates = set.candidates(late, &|_| None);
        let found = candidates
            .iter()
            .find(|c| c.peer_id == subject)
            .expect("still known through the long-lived source");
        assert!(
            found.protocol_observations.is_empty(),
            "the lapsed fact is gone while the peer remains: {:?}",
            found.protocol_observations
        );
    }
    #[test]
    fn an_already_expired_candidate_does_not_evict_a_live_peer() {
        // A cache event can sit queued past its own TTL, so this arrives
        // in ordinary operation. It contributes no live reachability, and
        // admitting it under pressure trades a usable candidate for an
        // entry `candidates()` will never show.
        let mut set = CandidateSet::new();
        let trust = nobody();

        for i in 0..MAX_CANDIDATES {
            set.observe(
                &for_id(
                    &identity(i),
                    "mdns",
                    "/ip4/10.1.0.1/tcp/1",
                    1_000 + i as u64,
                    Some(u64::MAX),
                ),
                1_000 + i as u64,
                &trust,
                true,
                false,
            );
        }
        let before = set.candidates(2_000, &|_| None).len();
        assert_eq!(before, MAX_CANDIDATES, "the set is full of live peers");

        let stale = identity(888_888);
        let candidate = for_id(&stale, "peer-cache", "/ip4/10.0.0.1/tcp/1", 10, Some(50));
        assert!(candidate.validate().is_ok(), "a lapsed candidate is valid");
        set.observe(&candidate, 5_000, &trust, true, false);

        assert_eq!(
            set.candidates(5_000, &|_| None).len(),
            before,
            "nothing was displaced for an entry already past its lifetime"
        );
        assert!(
            !set.candidates(5_000, &|_| None)
                .iter()
                .any(|c| c.peer_id == stale),
            "and it did not take a slot"
        );
    }

    #[test]
    fn a_candidate_expiring_in_the_future_is_still_admitted() {
        // The control: the guard must test "already past", not "has an
        // expiry at all" — every mDNS and cache candidate carries one.
        let mut set = CandidateSet::new();
        let trust = nobody();
        let fresh = identity(4);
        set.observe(
            &for_id(&fresh, "peer-cache", "/ip4/10.0.0.1/tcp/1", 0, Some(10_000)),
            100,
            &trust,
            true,
            false,
        );
        assert!(
            set.candidates(200, &|_| None)
                .iter()
                .any(|c| c.peer_id == fresh),
            "a candidate with time left is admitted normally"
        );
    }

    #[test]
    fn a_pinned_source_is_recorded_even_when_the_provenance_cap_is_full() {
        // Eight sources already claim the address; the configured one
        // arrives ninth. Dropped by arrival order, its record is lost —
        // and when the eight expire the address goes with them, though it
        // is still configured and static will not re-emit it.
        let mut set = CandidateSet::new();
        let trust = nobody();
        let subject = identity(21);
        let address = "/ip4/10.0.0.1/tcp/4001";

        for i in 0..MAX_PROVENANCE_PER_ADDRESS {
            let mut c = for_id(&subject, "unpinned", address, i as u64, Some(1_000));
            c.source = format!("unpinned-{i}");
            set.observe(&c, i as u64, &trust, true, false);
        }

        let mut pinned = for_id(&subject, "static-bootstrap", address, 500, None);
        pinned.source = "static-bootstrap".to_owned();
        set.observe(&pinned, 500, &trust, false, true);

        // Past every unpinned lifetime: only a recorded pinned source can
        // keep the address alive.
        let candidates = set.candidates(5_000, &|_| None);
        let found = candidates
            .iter()
            .find(|c| c.peer_id == subject)
            .expect("the configured source still supports this peer");
        assert!(
            found.address_list().contains(&address),
            "the address outlives the expiring sources: {:?}",
            found.address_list()
        );
    }

    #[test]
    fn an_unpinned_source_past_the_provenance_cap_is_still_refused() {
        // The control: only a pinned record displaces, or the cap is not
        // a cap at all.
        let mut set = CandidateSet::new();
        let trust = nobody();
        let subject = identity(22);
        let address = "/ip4/10.0.0.1/tcp/4001";

        for i in 0..MAX_PROVENANCE_PER_ADDRESS {
            let mut c = for_id(&subject, "unpinned", address, i as u64, Some(1_000));
            c.source = format!("unpinned-{i}");
            set.observe(&c, i as u64, &trust, true, false);
        }
        let mut late = for_id(&subject, "late", address, 900, Some(u64::MAX));
        late.source = "late".to_owned();
        set.observe(&late, 900, &trust, true, false);

        // Past every recorded lifetime. If `late` had displaced one, the
        // address would survive on it.
        assert!(
            !set.candidates(5_000, &|_| None)
                .iter()
                .any(|c| c.peer_id == subject),
            "an unpinned ninth source is refused, so nothing outlives the cap"
        );
    }
    #[test]
    fn a_stale_protocol_observation_cannot_overwrite_a_newer_one() {
        // One candidate carrying both verdicts for one protocol is legal:
        // `protocol_observations` is a set, and its derived ordering puts
        // `supported: false` first, so the stale positive was applied
        // last and won.
        let mut set = CandidateSet::new();
        let trust = nobody();
        let subject = identity(31);
        let protocol = ProtocolId::parse("/interweave/direct/2.0.0").expect("valid");

        let mut c = for_id(&subject, "mdns", "/ip4/10.0.0.1/tcp/1", 100, Some(u64::MAX));
        c.protocol_observations.insert(ProtocolObservation {
            protocol_id: protocol.clone(),
            supported: true,
            observed_at: 10,
        });
        c.protocol_observations.insert(ProtocolObservation {
            protocol_id: protocol.clone(),
            supported: false,
            observed_at: 90,
        });
        set.observe(&c, 100, &trust, true, false);

        let candidates = set.candidates(200, &|_| None);
        let found = candidates
            .iter()
            .find(|c| c.peer_id == subject)
            .expect("known");
        assert_eq!(
            found.protocol_observations.len(),
            1,
            "one source, one record"
        );
        assert!(
            !found.protocol_observations[0].supported,
            "the NEWER withdrawal stands, not the older claim of support"
        );
    }

    #[test]
    fn a_late_delivered_observation_does_not_revive_stale_support() {
        // The same failure across two events rather than inside one.
        let mut set = CandidateSet::new();
        let trust = nobody();
        let subject = identity(32);
        let protocol = ProtocolId::parse("/interweave/direct/2.0.0").expect("valid");

        for (supported, observed_at) in [(false, 90u64), (true, 10u64)] {
            let mut c = for_id(&subject, "mdns", "/ip4/10.0.0.1/tcp/1", 100, Some(u64::MAX));
            c.protocol_observations.insert(ProtocolObservation {
                protocol_id: protocol.clone(),
                supported,
                observed_at,
            });
            set.observe(&c, 100, &trust, true, false);
        }

        let candidates = set.candidates(200, &|_| None);
        let found = candidates
            .iter()
            .find(|c| c.peer_id == subject)
            .expect("known");
        assert!(
            !found.protocol_observations[0].supported,
            "an event delivered late does not overwrite fresher evidence"
        );
    }

    #[test]
    fn a_genuinely_newer_observation_does_replace_the_held_one() {
        // The control: "newest wins" must not become "first wins".
        let mut set = CandidateSet::new();
        let trust = nobody();
        let subject = identity(33);
        let protocol = ProtocolId::parse("/interweave/direct/2.0.0").expect("valid");

        for (supported, observed_at) in [(true, 10u64), (false, 90u64)] {
            let mut c = for_id(&subject, "mdns", "/ip4/10.0.0.1/tcp/1", 100, Some(u64::MAX));
            c.protocol_observations.insert(ProtocolObservation {
                protocol_id: protocol.clone(),
                supported,
                observed_at,
            });
            set.observe(&c, 100, &trust, true, false);
        }

        let candidates = set.candidates(200, &|_| None);
        let found = candidates
            .iter()
            .find(|c| c.peer_id == subject)
            .expect("known");
        assert!(
            !found.protocol_observations[0].supported,
            "a newer observation replaces the held one"
        );
        assert_eq!(found.protocol_observations[0].observed_at_ms, 90);
    }
    #[test]
    fn a_stale_address_event_cannot_retire_a_live_address() {
        // A known peer deliberately accepts an already-expired candidate
        // so its observations still merge. That makes a delayed stale
        // event able to roll the provenance backward — retiring an
        // address, or the peer entirely when it was the only source.
        let mut set = CandidateSet::new();
        let trust = nobody();
        let subject = identity(41);
        let address = "/ip4/10.0.0.1/tcp/4001";

        // Live: observed at 1000, good until 100_000.
        set.observe(
            &for_id(&subject, "peer-cache", address, 1_000, Some(100_000)),
            1_000,
            &trust,
            true,
            false,
        );

        // A delayed event from the SAME source, observed long before and
        // already expired by the time it lands.
        set.observe(
            &for_id(&subject, "peer-cache", address, 10, Some(50)),
            2_000,
            &trust,
            true,
            false,
        );

        let candidates = set.candidates(3_000, &|_| None);
        let found = candidates
            .iter()
            .find(|c| c.peer_id == subject)
            .expect("the peer must survive a stale event");
        assert!(
            found.address_list().contains(&address),
            "the live address is not retired by older evidence: {:?}",
            found.address_list()
        );
    }

    #[test]
    fn a_newer_address_event_still_refreshes_the_lifetime() {
        // The control: forward-only must not become never.
        let mut set = CandidateSet::new();
        let trust = nobody();
        let subject = identity(42);
        let address = "/ip4/10.0.0.1/tcp/4001";

        set.observe(
            &for_id(&subject, "peer-cache", address, 1_000, Some(2_000)),
            1_000,
            &trust,
            true,
            false,
        );
        set.observe(
            &for_id(&subject, "peer-cache", address, 1_500, Some(100_000)),
            1_500,
            &trust,
            true,
            false,
        );

        assert!(
            set.candidates(5_000, &|_| None)
                .iter()
                .any(|c| c.peer_id == subject),
            "a newer observation extends the lifetime past the old expiry"
        );
    }
    #[test]
    fn an_expired_address_slot_does_not_reject_a_live_address() {
        // Under pump-then-sweep a peer can hold a full set of address
        // KEYS whose provenance has already expired. The cap counted
        // those, so a live address was rejected; the later sweep frees the
        // slot but cannot recover it, and a provider that considers its
        // snapshot emitted will not offer it again.
        let mut set = CandidateSet::new();
        let trust = nobody();
        let subject = identity(51);

        for i in 0..MAX_ADDRESSES_PER_PEER {
            set.observe(
                &for_id(
                    &subject,
                    "mdns",
                    &format!("/ip4/192.168.1.{i}/tcp/4001"),
                    0,
                    Some(1_000),
                ),
                0,
                &trust,
                true,
                false,
            );
        }

        // Past every one of those lifetimes, with no sweep in between.
        let fresh = "/ip4/10.0.0.9/tcp/4001";
        set.observe(
            &for_id(&subject, "peer-cache", fresh, 5_000, Some(u64::MAX)),
            5_000,
            &trust,
            true,
            false,
        );

        let candidates = set.candidates(5_000, &|_| None);
        let found = candidates
            .iter()
            .find(|c| c.peer_id == subject)
            .expect("the peer is still known");
        assert!(
            found.address_list().contains(&fresh),
            "the live address is admitted into a slot only a dead one held: {:?}",
            found.address_list()
        );
    }

    #[test]
    fn a_live_address_slot_still_refuses_an_unpinned_newcomer() {
        // The control: pruning must free only EXPIRED slots, or the cap
        // stops being a cap.
        let mut set = CandidateSet::new();
        let trust = nobody();
        let subject = identity(52);

        for i in 0..MAX_ADDRESSES_PER_PEER {
            set.observe(
                &for_id(
                    &subject,
                    "mdns",
                    &format!("/ip4/192.168.1.{i}/tcp/4001"),
                    0,
                    Some(u64::MAX),
                ),
                0,
                &trust,
                true,
                false,
            );
        }
        let late = "/ip4/10.0.0.9/tcp/4001";
        set.observe(
            &for_id(&subject, "mdns", late, 100, Some(u64::MAX)),
            100,
            &trust,
            true,
            false,
        );

        let candidates = set.candidates(200, &|_| None);
        let found = candidates
            .iter()
            .find(|c| c.peer_id == subject)
            .expect("known");
        assert!(
            !found.address_list().contains(&late),
            "every slot is live, so an unpinned newcomer is still refused"
        );
        assert_eq!(found.addresses.len(), MAX_ADDRESSES_PER_PEER);
    }
    #[test]
    fn a_selective_retraction_drops_the_facts_of_a_source_left_supporting_nothing() {
        // The source has one address and one protocol claim. It retracts
        // the address selectively, so the whole-peer branch never runs —
        // and because another provider still supplies an address, the
        // peer survives carrying the departed source's claim until its
        // original TTL.
        let mut m = DiscoveryManager::new();
        m.register(descriptor("mdns"), 0).expect("registers");
        m.register(descriptor("peer-cache"), 0).expect("registers");
        let trust = nobody();
        let subject = identity(61);
        let protocol = ProtocolId::parse("/interweave/direct/2.0.0").expect("valid");

        let leaving = "/ip4/192.168.1.5/tcp/4001";
        let mut c = for_id(&subject, "mdns", leaving, 0, Some(u64::MAX));
        c.protocol_observations.insert(ProtocolObservation {
            protocol_id: protocol.clone(),
            supported: true,
            observed_at: 0,
        });
        m.on_event("mdns", observed(c), 0, &trust)
            .expect("accepted");

        // Another provider keeps the peer alive.
        m.on_event(
            "peer-cache",
            observed(for_id(
                &subject,
                "peer-cache",
                "/ip4/10.0.0.1/tcp/1",
                0,
                Some(u64::MAX),
            )),
            0,
            &trust,
        )
        .expect("accepted");

        assert_eq!(
            m.candidates(10)
                .iter()
                .find(|c| c.peer_id == subject)
                .expect("known")
                .protocol_observations
                .len(),
            1,
            "the fact is present while mdns supports an address"
        );

        m.on_event(
            "mdns",
            DiscoveryEvent::CandidateExpired {
                peer_id: subject.clone(),
                source: "mdns".to_owned(),
                addresses: [leaving.to_owned()].into_iter().collect(),
            },
            20,
            &trust,
        )
        .expect("accepted");

        let candidates = m.candidates(30);
        let found = candidates
            .iter()
            .find(|c| c.peer_id == subject)
            .expect("the peer survives on the other provider");
        assert!(
            found.protocol_observations.is_empty(),
            "mdns supports no address now, so its claims go with it: {:?}",
            found.protocol_observations
        );
    }

    #[test]
    fn a_selective_retraction_keeps_the_facts_of_a_source_still_supporting_an_address() {
        // The control: only a source left with NO provenance loses its
        // facts, or a peer announcing several addresses forfeits its
        // protocol evidence the moment one lapses.
        let mut m = DiscoveryManager::new();
        m.register(descriptor("mdns"), 0).expect("registers");
        let trust = nobody();
        let subject = identity(62);
        let protocol = ProtocolId::parse("/interweave/direct/2.0.0").expect("valid");

        let mut c = for_id(
            &subject,
            "mdns",
            "/ip4/192.168.1.5/tcp/4001",
            0,
            Some(u64::MAX),
        );
        c.addresses.insert("/ip4/192.168.1.6/tcp/4001".to_owned());
        c.protocol_observations.insert(ProtocolObservation {
            protocol_id: protocol,
            supported: true,
            observed_at: 0,
        });
        m.on_event("mdns", observed(c), 0, &trust)
            .expect("accepted");

        m.on_event(
            "mdns",
            DiscoveryEvent::CandidateExpired {
                peer_id: subject.clone(),
                source: "mdns".to_owned(),
                addresses: ["/ip4/192.168.1.5/tcp/4001".to_owned()]
                    .into_iter()
                    .collect(),
            },
            20,
            &trust,
        )
        .expect("accepted");

        let candidates = m.candidates(30);
        let found = candidates
            .iter()
            .find(|c| c.peer_id == subject)
            .expect("known");
        assert_eq!(
            found.protocol_observations.len(),
            1,
            "mdns still supports an address, so its claim stands"
        );
    }
    #[test]
    fn an_expired_observation_slot_does_not_reject_a_live_fact() {
        // The cap counted map entries, and an expired observation is
        // still an entry until `sweep` runs — so a peer holding sixteen
        // lapsed facts rejected a live one, and the sweep afterwards
        // cannot recover it.
        let mut set = CandidateSet::new();
        let trust = nobody();
        let subject = identity(71);

        // Sixteen facts with a short lifetime, on an address that lives.
        let mut old = for_id(&subject, "mdns", "/ip4/10.0.0.1/tcp/1", 0, Some(1_000));
        for i in 0..MAX_OBSERVATIONS_PER_PEER {
            old.protocol_observations.insert(ProtocolObservation {
                protocol_id: ProtocolId::parse(format!("/interweave/p{i}/1.0.0")).expect("valid"),
                supported: true,
                observed_at: 0,
            });
        }
        set.observe(&old, 0, &trust, true, false);

        // Past those lifetimes, with no sweep, a live fact arrives from
        // another source on an address that is still good.
        let mut fresh = for_id(
            &subject,
            "peer-cache",
            "/ip4/10.0.0.2/tcp/2",
            5_000,
            Some(u64::MAX),
        );
        fresh.protocol_observations.insert(ProtocolObservation {
            protocol_id: ProtocolId::parse("/interweave/direct/2.0.0").expect("valid"),
            supported: true,
            observed_at: 5_000,
        });
        set.observe(&fresh, 5_000, &trust, true, false);

        let candidates = set.candidates(5_000, &|_| None);
        let found = candidates
            .iter()
            .find(|c| c.peer_id == subject)
            .expect("the peer is known");
        assert!(
            found
                .protocol_observations
                .iter()
                .any(|o| o.protocol.as_str() == "/interweave/direct/2.0.0"),
            "the live fact is admitted into a slot only lapsed ones held: {:?}",
            found.protocol_observations
        );
    }

    #[test]
    fn a_live_observation_slot_still_refuses_a_newcomer() {
        // The control: pruning must free only EXPIRED slots, or the cap
        // stops bounding what a peer can assert.
        let mut set = CandidateSet::new();
        let trust = nobody();
        let subject = identity(72);

        let mut full = for_id(&subject, "mdns", "/ip4/10.0.0.1/tcp/1", 0, Some(u64::MAX));
        for i in 0..MAX_OBSERVATIONS_PER_PEER {
            full.protocol_observations.insert(ProtocolObservation {
                protocol_id: ProtocolId::parse(format!("/interweave/p{i}/1.0.0")).expect("valid"),
                supported: true,
                observed_at: 0,
            });
        }
        set.observe(&full, 0, &trust, true, false);

        let mut extra = for_id(&subject, "mdns", "/ip4/10.0.0.1/tcp/1", 100, Some(u64::MAX));
        extra.protocol_observations.insert(ProtocolObservation {
            protocol_id: ProtocolId::parse("/interweave/direct/2.0.0").expect("valid"),
            supported: true,
            observed_at: 100,
        });
        set.observe(&extra, 100, &trust, true, false);

        let candidates = set.candidates(200, &|_| None);
        let found = candidates
            .iter()
            .find(|c| c.peer_id == subject)
            .expect("known");
        assert_eq!(
            found.protocol_observations.len(),
            MAX_OBSERVATIONS_PER_PEER,
            "every slot is live, so the cap still holds"
        );
    }
    #[test]
    fn recency_falls_back_when_the_newest_source_retracts() {
        // Held as a monotonic maximum, recency only ever rose — so a peer
        // whose most recent source retracted kept that source's
        // timestamp forever, and eviction preserved it over a peer whose
        // LIVE reachability was observed more recently.
        let mut set = CandidateSet::new();
        let trust = nobody();
        let subject = identity(81);

        set.observe(
            &for_id(
                &subject,
                "peer-cache",
                "/ip4/10.0.0.1/tcp/1",
                100,
                Some(u64::MAX),
            ),
            100,
            &trust,
            true,
            false,
        );
        set.observe(
            &for_id(
                &subject,
                "mdns",
                "/ip4/192.168.1.5/tcp/4001",
                9_000,
                Some(u64::MAX),
            ),
            9_000,
            &trust,
            true,
            false,
        );
        assert_eq!(
            set.candidates(10_000, &|_| None)[0].last_observed_ms,
            9_000,
            "the newest source sets recency while it is live"
        );

        set.retract(&subject, "mdns", &BTreeSet::new());

        assert_eq!(
            set.candidates(10_000, &|_| None)[0].last_observed_ms,
            100,
            "and it falls back to the newest source that REMAINS, rather \
             than keeping a departed source's recency forever"
        );
    }

    #[test]
    fn an_expired_source_does_not_hold_recency_either() {
        // The same rule through expiry rather than retraction, since a
        // sweep is the ordinary way a source leaves.
        let mut set = CandidateSet::new();
        let trust = nobody();
        let subject = identity(82);

        set.observe(
            &for_id(
                &subject,
                "peer-cache",
                "/ip4/10.0.0.1/tcp/1",
                100,
                Some(u64::MAX),
            ),
            100,
            &trust,
            true,
            false,
        );
        set.observe(
            &for_id(
                &subject,
                "mdns",
                "/ip4/192.168.1.5/tcp/4001",
                9_000,
                Some(9_500),
            ),
            9_000,
            &trust,
            true,
            false,
        );

        // NO SWEEP. An earlier version of this test called `sweep` first,
        // so it passed by REMOVING the lapsed record rather than by
        // recency judging it — and `recency` reading every record
        // regardless of expiry went undetected. Records are removed only
        // by a pump the caller controls, so "expired" and "gone" are
        // different states and this asserts the first.
        assert_eq!(
            set.candidates(10_000, &|_| None)[0].last_observed_ms,
            100,
            "a lapsed source stops counting toward recency before any sweep"
        );

        // And still so afterwards, which is the weaker claim the earlier
        // version was accidentally making.
        set.sweep(10_000);
        assert_eq!(
            set.candidates(10_000, &|_| None)[0].last_observed_ms,
            100,
            "and after the sweep removes it"
        );
    }
}
