// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! [`PeerCacheDiscovery`]: the cache as a `DiscoveryProvider`.
//!
//! Stage 3 built the bounded persistence; this is the provider face
//! ADR-0027 describes — the ONE provider that persists observations, fed
//! by hints travelling `ConnectionManager -> TransportRuntime -> here`.
//! Every other provider observes the network; this one remembers what the
//! runtime already learned.
//!
//! A cached peer is never trusted because it was cached, and emitting a
//! candidate is not a claim that the peer is reachable now — only that it
//! was, within the cache's TTL.

use interweave_discovery_api::{
    DiscoveryEvent, DiscoveryProvider, HintDisposition, PeerHint, ProviderDescriptor,
    ProviderError, ProviderHealth, ProviderMode, ProviderScope,
};
use std::collections::BTreeMap;

use interweave_transport_api::TransportIdentity;

use crate::cache::{CacheHealth, PeerCache, SOURCE};

/// The provider-interface version this implements.
const INTERFACE_VERSION: &str = "1.0";

/// The cache, presented as a discovery provider.
#[derive(Debug)]
pub struct PeerCacheDiscovery {
    cache: PeerCache,
    started: bool,
    stopped: bool,
    /// Events waiting to be drained, oldest first.
    pending: Vec<DiscoveryEvent>,
    /// Peers emitted as candidates, with the freshness last reported for
    /// each, so ageing out is an expiry rather than silence AND a record
    /// whose life was extended is re-emitted rather than stranded.
    ///
    /// The value is the record's own `expires_at` at the time it was
    /// emitted. Tracking only the peer id was a liveness bug: a peer that
    /// kept succeeding had its cache expiry extended while the consumer
    /// still held the FIRST one, and the manager — which learns lifetimes
    /// only from an observation event — expired a peer the cache
    /// considered fresh.
    emitted: BTreeMap<TransportIdentity, Option<u64>>,
}

impl PeerCacheDiscovery {
    /// Wrap a loaded cache.
    #[must_use]
    pub fn new(cache: PeerCache) -> Self {
        Self {
            cache,
            started: false,
            stopped: false,
            pending: Vec::new(),
            emitted: BTreeMap::new(),
        }
    }

    /// The cache underneath, for a caller that also persists it.
    #[must_use]
    pub const fn cache(&self) -> &PeerCache {
        &self.cache
    }

    /// Mutable access, for the flush the owner schedules.
    pub const fn cache_mut(&mut self) -> &mut PeerCache {
        &mut self.cache
    }

    /// Re-read the cache and queue the difference since the last look.
    ///
    /// A peer that is fresh is (re-)observed; one that has aged out of the
    /// cache since it was emitted is retracted. Called from
    /// `drain_events`, so a caller that never drains queues nothing —
    /// which is what keeps this provider's state bounded by the cache's
    /// own bounds rather than by how long nobody looked.
    fn refresh(&mut self, now_ms: u64) {
        if !self.started || self.stopped {
            return;
        }
        let fresh = self.cache.candidates(now_ms);
        let mut live: BTreeMap<TransportIdentity, Option<u64>> = BTreeMap::new();

        // WHAT CHANGED, where a longer life counts as a change. Re-emitting
        // the whole cache on every drain would be duplicate-tolerant (the
        // manager dedups) but unbounded in traffic. Emitting only on FIRST
        // sight was the other error: a record whose expiry moved forward
        // never reached the consumer, which holds a lifetime it can only
        // learn from an observation event, so a peer that kept succeeding
        // lapsed there while staying fresh here.
        for candidate in fresh {
            let previously = self.emitted.get(&candidate.peer_id).copied();
            let changed = previously != Some(candidate.expires_at);
            live.insert(candidate.peer_id.clone(), candidate.expires_at);
            if changed {
                self.pending.push(DiscoveryEvent::CandidateObserved {
                    candidate: Box::new(candidate),
                });
            }
        }
        // GONE FROM THE CACHE IS AN EXPIRY, not silence. A consumer that
        // heard a candidate must hear that it lapsed, or the manager keeps
        // it until its own default TTL for a peer this provider no longer
        // vouches for.
        let expired: Vec<TransportIdentity> = self
            .emitted
            .keys()
            .filter(|p| !live.contains_key(*p))
            .cloned()
            .collect();
        for peer_id in expired {
            self.pending.push(DiscoveryEvent::CandidateExpired {
                peer_id,
                source: SOURCE.to_owned(),
                // Empty: this provider's lifetime is per PEER (the record
                // ages as a whole), not per address.
                addresses: std::collections::BTreeSet::new(),
            });
        }
        self.emitted = live;
    }
}

impl DiscoveryProvider for PeerCacheDiscovery {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            name: SOURCE.to_owned(),
            interface_version: INTERFACE_VERSION.to_owned(),
            config_version: None,
            // Operator-configured location, holding what this node itself
            // observed — not the local link, and not the wider network.
            scope: ProviderScope::Configured,
            // It never queries anything; it reports what it was told.
            mode: ProviderMode::Passive,
            // Records age out on the cache's own TTL.
            supports_expiry: true,
            // The reachability and protocol hint classes are exactly what
            // this provider is for (ADR-0027).
            supports_hints: true,
        }
    }

    fn start(&mut self, now_ms: u64) -> Result<(), ProviderError> {
        if self.started {
            return Err(ProviderError::AlreadyStarted);
        }
        self.started = true;
        // The cold-start emission: everything still fresh on disk.
        self.refresh(now_ms);
        Ok(())
    }

    fn drain_events(&mut self, now_ms: u64, max: usize) -> Vec<DiscoveryEvent> {
        if !self.started || self.stopped {
            return Vec::new();
        }
        self.refresh(now_ms);
        let take = max.min(self.pending.len());
        self.pending.drain(..take).collect()
    }

    fn add_hint(&mut self, hint: PeerHint, now_ms: u64) -> HintDisposition {
        if !self.started || self.stopped {
            // Not an error: a hint offered outside the provider's life is
            // simply not taken, and saying so is the explicit answer the
            // contract asks for.
            return HintDisposition::Unsupported;
        }
        match hint {
            PeerHint::ObservedReachable {
                peer_id, address, ..
            } => match self.cache.record_success(&peer_id, &address, now_ms) {
                Ok(()) => HintDisposition::Accepted,
                // A bound refused it. The cache is unchanged and the
                // provider is fine — this is the hint being too big, not
                // the provider failing.
                Err(_) => HintDisposition::Rejected(
                    interweave_discovery_api::DiscoveryError::InvalidLength {
                        field: "address",
                        got: address.len(),
                        max: crate::limits::MAX_ADDRESS_BYTES,
                    },
                ),
            },
            // NOT ACCEPTED, and the reason is a deferral rather than a
            // policy: the cache's capability record carries a protocol
            // FAMILY, wire major, network hash and role, and a
            // `ProtocolId` is one opaque string. Inventing a mapping here
            // would decide, silently, what Stage 10 is required to decide
            // in the architecture first — the plan's §13 prerequisite
            // ("decide the capability-observation mapping in the
            // architecture before writing code"). Until then this class is
            // honestly unsupported rather than quietly mis-stored.
            PeerHint::ObservedProtocol { .. } => HintDisposition::Unsupported,
            // The cache persists what THIS node observed. A third party's
            // candidate is someone else's observation, and storing it here
            // would make the cache a relay for claims it cannot check.
            PeerHint::CandidateHint(_) => HintDisposition::Unsupported,
        }
    }

    fn health(&self) -> ProviderHealth {
        if !self.started || self.stopped {
            return ProviderHealth::Unavailable;
        }
        match self.cache.health() {
            CacheHealth::Healthy => ProviderHealth::Healthy,
            // A quarantined file is a cold start, not a dead provider: the
            // cache continues empty, so discovery is degraded and the node
            // still runs (`providers/peer-cache.md`).
            CacheHealth::Quarantined { .. } => ProviderHealth::Degraded,
        }
    }

    fn shutdown(&mut self, _now_ms: u64) {
        self.stopped = true;
        self.pending.clear();
        self.emitted.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::CacheLimits;

    const P1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    const P2: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";

    fn peer(s: &str) -> TransportIdentity {
        TransportIdentity::parse(s).expect("valid identity")
    }

    /// A provider over a cache in a fresh temporary directory.
    fn provider(dir: &tempfile::TempDir) -> PeerCacheDiscovery {
        let cache = PeerCache::load(&dir.path().join("peers.json"), CacheLimits::default())
            .expect("an absent file is an empty cache, not an error");
        PeerCacheDiscovery::new(cache)
    }

    fn observed_peers(events: &[DiscoveryEvent]) -> Vec<TransportIdentity> {
        events
            .iter()
            .filter_map(|e| match e {
                DiscoveryEvent::CandidateObserved { candidate } => Some(candidate.peer_id.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_descriptor_names_the_cache_source() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = provider(&dir);
        let d = p.descriptor();
        assert_eq!(d.name, SOURCE, "the name is the source it stamps");
        assert!(d.supports_expiry, "records age out on the cache TTL");
        assert!(d.supports_hints, "this is the provider hints are for");
    }

    #[test]
    fn a_cold_start_emits_what_was_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut p = provider(&dir);
        p.cache_mut()
            .record_success(&peer(P1), "/ip4/10.0.0.1/tcp/4001", 1_000)
            .expect("within bounds");

        // NOTHING BEFORE START. The rule is guarded twice — in the drain
        // and in `refresh` — so removing EITHER alone is unobservable and
        // this test does not go red for it; removing BOTH does, which was
        // checked. Redundant guards are the point rather than an
        // oversight: `refresh` is also reachable from `start`, where the
        // drain's guard does not apply.
        assert!(
            p.drain_events(1_000, 8).is_empty(),
            "nothing before start — the contract's first rule"
        );
        assert_eq!(
            p.health(),
            ProviderHealth::Unavailable,
            "and an unstarted provider is unavailable, not healthy"
        );
        p.start(1_000).expect("starts");
        let events = p.drain_events(1_000, 8);
        assert_eq!(observed_peers(&events), vec![peer(P1)]);
        assert_eq!(p.health(), ProviderHealth::Healthy);
    }

    #[test]
    fn a_reachability_hint_is_recorded_and_becomes_a_candidate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut p = provider(&dir);
        p.start(1_000).expect("starts");
        let _ = p.drain_events(1_000, 8);

        assert_eq!(
            p.add_hint(
                PeerHint::ObservedReachable {
                    peer_id: peer(P2),
                    address: "/ip4/10.0.0.2/tcp/4001".to_owned(),
                    observed_at: 2_000,
                },
                2_000
            ),
            HintDisposition::Accepted
        );
        let events = p.drain_events(2_000, 8);
        assert_eq!(
            observed_peers(&events),
            vec![peer(P2)],
            "the hint reached the cache and came back out as a candidate"
        );
    }

    #[test]
    fn the_protocol_and_candidate_hint_classes_are_refused_explicitly() {
        // Not silence: DISCOVERY.md requires an explicit refusal, because
        // a provider that quietly accepts is one taking ownership of
        // something it does not handle.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut p = provider(&dir);
        p.start(1_000).expect("starts");

        assert_eq!(
            p.add_hint(
                PeerHint::ObservedProtocol {
                    peer_id: peer(P1),
                    protocol_id: interweave_discovery_api::ProtocolId::parse(
                        "/interweave/kad/1.0.0"
                    )
                    .expect("valid"),
                    supported: true,
                    observed_at: 1_000,
                },
                1_000
            ),
            HintDisposition::Unsupported,
            "the capability mapping is Stage 10's to decide in the architecture"
        );
        assert_eq!(
            p.add_hint(
                PeerHint::CandidateHint(Box::new(interweave_discovery_api::CandidatePeer {
                    peer_id: peer(P2),
                    addresses: ["/ip4/10.0.0.9/tcp/1".to_owned()].into_iter().collect(),
                    source: "somebody-else".to_owned(),
                    observed_at: 1_000,
                    expires_at: None,
                    protocol_observations: std::collections::BTreeSet::new(),
                })),
                1_000
            ),
            HintDisposition::Unsupported,
            "the cache persists what THIS node observed, not third-party claims"
        );
        // And neither was stored.
        assert!(p.drain_events(1_000, 8).is_empty());
    }

    #[test]
    fn a_refreshed_record_is_re_emitted_so_the_consumer_learns_the_new_life() {
        // The consumer holds a lifetime it can only learn from an
        // observation event. A hint that extends the cache record must
        // therefore produce one, or the peer lapses there while staying
        // fresh here.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut p = provider(&dir);
        p.cache_mut()
            .record_success(&peer(P1), "/ip4/10.0.0.1/tcp/4001", 0)
            .expect("within bounds");
        p.start(0).expect("starts");
        let first = p.drain_events(0, 8);
        assert_eq!(observed_peers(&first), vec![peer(P1)]);
        let first_expiry = match &first[0] {
            DiscoveryEvent::CandidateObserved { candidate } => candidate.expires_at,
            _ => panic!("an observation"),
        };

        // Still succeeding an hour later: the record's life moves forward.
        let later = 3_600_000;
        assert_eq!(
            p.add_hint(
                PeerHint::ObservedReachable {
                    peer_id: peer(P1),
                    address: "/ip4/10.0.0.1/tcp/4001".to_owned(),
                    observed_at: later,
                },
                later
            ),
            HintDisposition::Accepted
        );
        let again = p.drain_events(later, 8);
        assert_eq!(
            observed_peers(&again),
            vec![peer(P1)],
            "the extended life is forwarded, not stranded in the cache"
        );
        let second_expiry = match &again[0] {
            DiscoveryEvent::CandidateObserved { candidate } => candidate.expires_at,
            _ => panic!("an observation"),
        };
        assert!(
            second_expiry > first_expiry,
            "and it really is later: {second_expiry:?} vs {first_expiry:?}"
        );
    }

    #[test]
    fn an_unchanged_record_is_not_re_emitted() {
        // The other half: draining in a loop must not replay the cache.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut p = provider(&dir);
        p.cache_mut()
            .record_success(&peer(P1), "/ip4/10.0.0.1/tcp/4001", 0)
            .expect("within bounds");
        p.start(0).expect("starts");
        assert_eq!(p.drain_events(0, 8).len(), 1);
        for t in 1..20 {
            assert!(
                p.drain_events(t, 8).is_empty(),
                "nothing changed, so nothing is emitted"
            );
        }
    }

    #[test]
    fn a_peer_that_ages_out_is_retracted_rather_than_going_quiet() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut p = provider(&dir);
        p.cache_mut()
            .record_success(&peer(P1), "/ip4/10.0.0.1/tcp/4001", 0)
            .expect("within bounds");
        p.start(0).expect("starts");
        assert_eq!(observed_peers(&p.drain_events(0, 8)), vec![peer(P1)]);

        // Past the cache TTL: the record is no longer fresh.
        let past_ttl = crate::limits::DEFAULT_TTL_MS + 1;
        let events = p.drain_events(past_ttl, 8);
        assert!(
            events.iter().any(|e| matches!(
                e,
                DiscoveryEvent::CandidateExpired { peer_id, .. } if peer_id == &peer(P1)
            )),
            "a consumer that heard the candidate must hear that it lapsed"
        );
    }

    #[test]
    fn the_drain_respects_the_callers_bound() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut p = provider(&dir);
        for (i, id) in [P1, P2].iter().enumerate() {
            p.cache_mut()
                .record_success(&peer(id), &format!("/ip4/10.0.0.{i}/tcp/1"), 0)
                .expect("within bounds");
        }
        p.start(0).expect("starts");
        assert_eq!(p.drain_events(0, 1).len(), 1, "the caller sizes the batch");
    }

    #[test]
    fn shutdown_is_idempotent_and_closes_the_stream() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut p = provider(&dir);
        p.cache_mut()
            .record_success(&peer(P1), "/ip4/10.0.0.1/tcp/1", 0)
            .expect("within bounds");
        p.start(0).expect("starts");
        p.shutdown(1);
        p.shutdown(2);
        assert!(p.drain_events(3, 8).is_empty());
        assert_eq!(p.health(), ProviderHealth::Unavailable);
        assert_eq!(
            p.add_hint(
                PeerHint::ObservedReachable {
                    peer_id: peer(P2),
                    address: "/ip4/10.0.0.2/tcp/1".to_owned(),
                    observed_at: 3,
                },
                3
            ),
            HintDisposition::Unsupported,
            "a hint after shutdown is not taken"
        );
    }

    #[test]
    fn a_second_start_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut p = provider(&dir);
        p.start(0).expect("starts");
        assert_eq!(p.start(1), Err(ProviderError::AlreadyStarted));
    }

    #[test]
    fn a_quarantined_cache_is_degraded_not_dead() {
        // A corrupt advisory cache costs a cold start, never a failed
        // startup (`providers/peer-cache.md`).
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("peers.json");
        std::fs::write(&path, b"{ this is not json").expect("writes");
        let cache = PeerCache::load(&path, CacheLimits::default())
            .expect("a corrupt file quarantines rather than failing the load");
        let mut p = PeerCacheDiscovery::new(cache);
        p.start(0).expect("starts");
        assert_eq!(p.health(), ProviderHealth::Degraded);
        assert!(
            p.drain_events(0, 8).is_empty(),
            "and it continues, empty, rather than serving garbage"
        );
    }
}
