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
    /// The source is a CONFIGURED provider (`ProviderScope::Configured`),
    /// which is what the retention rule in `evict_one` keys on. Carried
    /// per record rather than looked up at eviction time because the
    /// registry can change between the observation and the pressure.
    configured: bool,
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
    last_observed_ms: u64,
}

impl Entry {
    /// Any live provenance from a configured provider.
    fn is_configured(&self) -> bool {
        self.addresses
            .values()
            .any(|rs| rs.iter().any(|r| r.configured))
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
                Some(AggregatedCandidate {
                    peer_id: peer_id.clone(),
                    addresses,
                    sources,
                    last_observed_ms: entry.last_observed_ms,
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
        provider_configured: bool,
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
        entry.last_observed_ms = entry.last_observed_ms.max(candidate.observed_at);

        for address in &candidate.addresses {
            if !entry.addresses.contains_key(address)
                && entry.addresses.len() >= MAX_ADDRESSES_PER_PEER
            {
                continue;
            }
            let records = entry.addresses.entry(address.clone()).or_default();
            // ONE RECORD PER SOURCE. Re-observing refreshes that source's
            // lifetime and leaves every other source's alone, which is
            // what makes "the address dies when no source supports it"
            // mean something.
            if let Some(existing) = records.iter_mut().find(|r| r.source == candidate.source) {
                existing.observed_at = candidate.observed_at;
                existing.expires_at = expires_at;
            } else if records.len() < MAX_PROVENANCE_PER_ADDRESS {
                records.push(Provenance {
                    source: candidate.source.clone(),
                    observed_at: candidate.observed_at,
                    expires_at,
                    configured: provider_configured,
                });
            }
        }

        for observation in &candidate.protocol_observations {
            let key = (observation.protocol_id.clone(), candidate.source.clone());
            if !entry.observations.contains_key(&key)
                && entry.observations.len() >= MAX_OBSERVATIONS_PER_PEER
            {
                continue;
            }
            entry.observations.insert(
                key,
                (observation.supported, observation.observed_at, expires_at),
            );
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
        if addresses.is_empty() {
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
            .filter(|(_, e)| e.is_configured())
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
                std::cmp::Reverse(self.peers.get(*p).map_or(0, |e| e.last_observed_ms))
            });
            ranked.truncate(MAX_CONFIGURED_RETAINED);
            ranked.into_iter().collect()
        };

        let victim = self
            .peers
            .iter()
            .filter(|(peer, _)| !trust.decide(peer).is_allowed())
            .filter(|(peer, _)| !protect.contains(peer))
            .min_by_key(|(_, e)| e.last_observed_ms)
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
                let provider_configured = self
                    .providers
                    .get(source)
                    .is_some_and(|p| p.descriptor.scope == ProviderScope::Configured);
                self.candidates.observe(
                    &candidate,
                    now_ms,
                    trust,
                    provider_expires,
                    provider_configured,
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
}
