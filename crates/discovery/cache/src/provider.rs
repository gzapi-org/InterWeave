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
use std::collections::{BTreeMap, BTreeSet};

use interweave_transport_api::TransportIdentity;

use crate::cache::{CacheHealth, PeerCache, SOURCE};

/// The provider-interface version this implements.
const INTERFACE_VERSION: &str = "1.0";

/// The ceiling on events waiting to be drained.
///
/// Two per peer the cache can hold — one observation, one retraction —
/// which is what a consumer draining normally ever sees at once.
/// Per-peer coalescing bounds what one identity queues; this bounds the
/// queue, which is the part that survives peers ageing out and being
/// replaced by different ones.
pub const MAX_PENDING_EVENTS: usize = 2 * crate::limits::MAX_PEERS;

/// A queued event, with the bookkeeping needed to undo it.
///
/// `before` is what `emitted` held for this peer immediately BEFORE the
/// event was queued. That is the only thing that reliably reverses a
/// drop: an event says what CHANGED, and for a whole-peer retraction it
/// deliberately says nothing about the addresses being withdrawn — so a
/// rollback reconstructing state from the event, or from the cache as it
/// stands now, cannot recover what the consumer was actually holding.
#[derive(Debug, Clone)]
struct Queued {
    event: DiscoveryEvent,
    before: Option<EmittedRecord>,
}

/// What a consumer was last told about one peer.
///
/// Compared as a whole to decide re-emission: any field that a
/// `CandidateObserved` carries belongs here, or a change to it is
/// invisible and the consumer keeps stale content.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct EmittedRecord {
    expires_at: Option<u64>,
    addresses: BTreeSet<String>,
}

/// The cache, presented as a discovery provider.
#[derive(Debug)]
pub struct PeerCacheDiscovery {
    cache: PeerCache,
    started: bool,
    stopped: bool,
    /// Events waiting to be drained, oldest first.
    pending: Vec<Queued>,
    /// Peers emitted as candidates, with the freshness last reported for
    /// each, so ageing out is an expiry rather than silence AND a record
    /// whose life was extended is re-emitted rather than stranded.
    ///
    /// The value is what the consumer was last told — the record's
    /// `expires_at` AND its address set, which is the whole of what a
    /// `CandidateObserved` carries. Tracking only the peer id was a liveness bug: a peer that
    /// kept succeeding had its cache expiry extended while the consumer
    /// still held the FIRST one, and the manager — which learns lifetimes
    /// only from an observation event — expired a peer the cache
    /// considered fresh.
    emitted: BTreeMap<TransportIdentity, EmittedRecord>,
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

    /// Hold `pending` to [`MAX_PENDING_EVENTS`], oldest first.
    ///
    /// Oldest because a consumer this far behind has already missed more
    /// recent news, and because refusing NEW events instead would let the
    /// provider's view freeze at whatever filled the queue.
    ///
    /// DROPPING AN EVENT ROLLS BACK THE BOOKKEEPING THAT PRODUCED IT.
    /// `refresh` emits the difference between `emitted` and the cache and
    /// then advances `emitted`, so a dropped event was otherwise gone for
    /// good — the next drain saw no difference and never recreated it. A
    /// discarded selective retraction was the expensive case: the manager
    /// keeps dialling an address the cache no longer holds, until its own
    /// peer-wide TTL, which is seven days.
    ///
    /// So the rollback is per event, and it is the inverse of what each
    /// one reported: forget an observation so the peer looks new again,
    /// and restore a retracted peer so it is re-detected as gone.
    fn enforce_pending_bound(&mut self) {
        if self.pending.len() <= MAX_PENDING_EVENTS {
            return;
        }
        // HEALTH IS NEVER THE THING DROPPED: the manager learns health
        // only from that event, so trimming the sole transition leaves
        // this provider reported unavailable indefinitely while it is
        // plainly working. Every other event is recoverable — that is
        // what the rollback below is for — and this one is not, because
        // nothing recomputes a transition that already happened.
        let mut excess = self.pending.len() - MAX_PENDING_EVENTS;
        let mut kept: Vec<Queued> = Vec::with_capacity(MAX_PENDING_EVENTS);
        let mut dropped: Vec<Queued> = Vec::new();
        for queued in self.pending.drain(..) {
            if excess > 0 && !matches!(queued.event, DiscoveryEvent::HealthChanged { .. }) {
                excess -= 1;
                dropped.push(queued);
            } else {
                kept.push(queued);
            }
        }
        self.pending = kept;

        // UNDONE IN REVERSE, restoring each event's own `before`. Reverse
        // order matters because a peer may have several dropped events —
        // a retraction and the observation that followed it — and the
        // EARLIEST one's snapshot is the state to end at. Restoring a
        // recorded snapshot rather than deriving one is what makes a
        // whole-peer retraction recoverable at all: it carries no
        // addresses, and the cache no longer holds them either.
        for queued in dropped.into_iter().rev() {
            let peer_id = match &queued.event {
                DiscoveryEvent::CandidateObserved { candidate } => candidate.peer_id.clone(),
                DiscoveryEvent::CandidateExpired { peer_id, .. } => peer_id.clone(),
                DiscoveryEvent::HealthChanged { .. } => continue,
            };
            match queued.before {
                Some(record) => {
                    self.emitted.insert(peer_id, record);
                }
                None => {
                    self.emitted.remove(&peer_id);
                }
            }
        }
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
        let mut live: BTreeMap<TransportIdentity, EmittedRecord> = BTreeMap::new();

        // WHAT CHANGED, where a longer life counts as a change. Re-emitting
        // the whole cache on every drain would be duplicate-tolerant (the
        // manager dedups) but unbounded in traffic. Emitting only on FIRST
        // sight was the other error: a record whose expiry moved forward
        // never reached the consumer, which holds a lifetime it can only
        // learn from an observation event, so a peer that kept succeeding
        // lapsed there while staying fresh here.
        for candidate in fresh {
            // WHAT THE CONSUMER WAS LAST TOLD, in full. Comparing expiry
            // alone missed a change the expiry cannot express: two
            // addresses recorded for one peer inside the same millisecond
            // share a record expiry, so the second address was suppressed
            // and never reached the manager until some later success
            // moved the timestamp. Address set and expiry together are the
            // whole of what a `CandidateObserved` carries, so comparing
            // both is comparing the message rather than a proxy for it.
            let record = EmittedRecord {
                expires_at: candidate.expires_at,
                addresses: candidate.addresses.iter().cloned().collect(),
            };
            let previously = self.emitted.get(&candidate.peer_id).cloned();
            let changed = previously.as_ref() != Some(&record);

            // AN ADDRESS THE CACHE DROPPED MUST BE RETRACTED, not merely
            // omitted. `record_success` truncates the least-recently-used
            // address once a peer is at its per-peer cap, and the
            // manager's `observe` is ADDITIVE — a fresh snapshot refreshes
            // what it names and says nothing about what it does not. The
            // dropped address would stay dialable there until the peer's
            // whole entry aged out, and under churn a peer's 16 manager
            // slots fill with addresses this cache no longer holds,
            // crowding out the ones it does.
            //
            // Emitted BEFORE the observation so the consumer never sees a
            // window with neither, and named selectively so a peer keeps
            // the addresses that survived.
            if let Some(prev) = previously.as_ref() {
                let dropped: BTreeSet<String> = prev
                    .addresses
                    .difference(&record.addresses)
                    .cloned()
                    .collect();
                if !dropped.is_empty() {
                    self.pending.push(Queued {
                        event: DiscoveryEvent::CandidateExpired {
                            peer_id: candidate.peer_id.clone(),
                            source: SOURCE.to_owned(),
                            addresses: dropped,
                        },
                        before: previously.clone(),
                    });
                }
            }

            live.insert(candidate.peer_id.clone(), record);
            if changed {
                // SUPERSEDE THE PENDING OBSERVATION FOR THIS PEER. Every
                // reachability hint moves the record's expiry, so a
                // stalled consumer collected one event per hint for the
                // same peer — the cache is bounded and the queue was not.
                // The newest snapshot strictly supersedes an undrained
                // older one, so replacing loses nothing.
                let peer_id = candidate.peer_id.clone();
                // The superseded event's `before` is INHERITED, not
                // discarded: it is older, so it is the state a rollback
                // has to reach. Keeping the newer one would undo only
                // half of what the pair did.
                let mut before = previously.clone();
                let mut inherited = false;
                self.pending.retain(|queued| {
                    let is_mine = matches!(
                        &queued.event,
                        DiscoveryEvent::CandidateObserved { candidate }
                            if candidate.peer_id == peer_id
                    );
                    if is_mine && !inherited {
                        before = queued.before.clone();
                        inherited = true;
                    }
                    !is_mine
                });
                self.pending.push(Queued {
                    event: DiscoveryEvent::CandidateObserved {
                        candidate: Box::new(candidate),
                    },
                    before,
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
            let before = self.emitted.get(&peer_id).cloned();
            self.pending.push(Queued {
                event: DiscoveryEvent::CandidateExpired {
                    peer_id,
                    source: SOURCE.to_owned(),
                    // Empty: this provider's lifetime is per PEER (the
                    // record ages as a whole), not per address.
                    addresses: BTreeSet::new(),
                },
                before,
            });
        }
        self.emitted = live;
        self.enforce_pending_bound();
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
        // THE INITIAL TRANSITION IS AN EVENT, and it carries the real
        // answer: a quarantined cache starts Degraded, which is exactly
        // the state a consumer needs to hear about at start rather than
        // never. The manager learns health only from `HealthChanged`.
        let health = if matches!(self.cache.health(), CacheHealth::Healthy) {
            ProviderHealth::Healthy
        } else {
            ProviderHealth::Degraded
        };
        self.pending.push(Queued {
            event: DiscoveryEvent::HealthChanged {
                source: SOURCE.to_owned(),
                health,
            },
            before: None,
        });
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
        self.pending.drain(..take).map(|q| q.event).collect()
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
                peer_id,
                address,
                observed_at,
            } => {
                // WHEN REACHABILITY WAS ESTABLISHED, not when the hint
                // arrived. The cache's TTL and its eviction ordering are
                // both keyed on this timestamp, so crediting `now_ms`
                // would let delivery delay buy a peer freshness it did not
                // earn — a queued hint would outrank a peer contacted more
                // recently. A hint dated in the future is clamped: a
                // caller cannot mint freshness beyond the present.
                let observed_at = observed_at.min(now_ms);
                match self.cache.record_success(&peer_id, &address, observed_at) {
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
                }
            }
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

    /// A synthetic identity that DECODES, not merely one that matches the
    /// pattern.
    ///
    /// `TransportIdentity::parse` decodes the base58btc and checks the
    /// multihash, so the 44 tail characters are not free: an id is built
    /// from its bytes — the identity-multihash envelope of a libp2p
    /// Ed25519 public-key protobuf — and only the 32 key bytes vary.
    /// Spelling a tail by hand produces strings no libp2p parser accepts,
    /// which is a test population that could never arrive over a wire.
    fn synthetic(cycle: u64, n: usize) -> TransportIdentity {
        let mut bytes = [0_u8; 38];
        bytes[..6].copy_from_slice(&[0x00, 0x24, 0x08, 0x01, 0x12, 0x20]);
        bytes[6..14].copy_from_slice(&(cycle * 100_000 + n as u64).to_be_bytes());
        TransportIdentity::parse(bs58::encode(bytes).into_string())
            .expect("a decodable synthetic identity")
    }

    fn peer(s: &str) -> TransportIdentity {
        TransportIdentity::parse(s).expect("valid identity")
    }

    /// A provider over a cache in a fresh temporary directory.
    fn provider(dir: &tempfile::TempDir) -> PeerCacheDiscovery {
        let cache = PeerCache::load(&dir.path().join("peers.json"), CacheLimits::default())
            .expect("an absent file is an empty cache, not an error");
        PeerCacheDiscovery::new(cache)
    }

    /// Events other than the initial health transition every provider
    /// queues at start so the manager learns it.
    fn candidate_events(events: &[DiscoveryEvent]) -> Vec<&DiscoveryEvent> {
        events
            .iter()
            .filter(|e| !matches!(e, DiscoveryEvent::HealthChanged { .. }))
            .collect()
    }

    /// The expiry carried by the first observation in a batch, which is
    /// not necessarily the first EVENT — a start also queues health.
    fn first_observed_expiry(events: &[DiscoveryEvent]) -> Option<u64> {
        events.iter().find_map(|e| match e {
            DiscoveryEvent::CandidateObserved { candidate } => Some(candidate.expires_at),
            _ => None,
        })?
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
        assert!(candidate_events(&p.drain_events(1_000, 8)).is_empty());
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
        let first_expiry = first_observed_expiry(&first);

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
        let second_expiry = first_observed_expiry(&again);
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
        // Health plus the one observation.
        assert_eq!(candidate_events(&p.drain_events(0, 8)).len(), 1);
        for t in 1..20 {
            assert!(
                candidate_events(&p.drain_events(t, 8)).is_empty(),
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
        // health + two observations were queued; the rest stay.
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
        let events = p.drain_events(0, 8);
        assert!(
            events.iter().any(|e| matches!(
                e,
                DiscoveryEvent::HealthChanged {
                    health: ProviderHealth::Degraded,
                    ..
                }
            )),
            "the degraded state is reported at start, not merely readable"
        );
        assert!(
            candidate_events(&events).is_empty(),
            "and it continues, empty, rather than serving garbage"
        );
    }
    #[test]
    fn a_delayed_hint_is_credited_to_when_reachability_was_observed() {
        // The hint is delivered a full day after the peer answered. If the
        // provider credits delivery time, the record's freshness — which
        // drives both TTL and eviction ordering — is inflated by the delay.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut p = provider(&dir);
        p.start(0).expect("start");

        const OBSERVED: u64 = 1_000;
        const DELIVERED: u64 = OBSERVED + 86_400_000;
        assert_eq!(
            p.add_hint(
                PeerHint::ObservedReachable {
                    peer_id: peer(P1),
                    address: "/ip4/127.0.0.1/tcp/1".to_owned(),
                    observed_at: OBSERVED,
                },
                DELIVERED,
            ),
            HintDisposition::Accepted
        );

        // Read the record back through the cache's own view at the instant
        // the peer should lapse. Credited to OBSERVED it is gone; credited
        // to DELIVERED it is still live, which is the bug.
        let lapse = OBSERVED + crate::limits::DEFAULT_TTL_MS;
        assert!(
            p.cache_mut().candidates(lapse).is_empty(),
            "a hint observed at {OBSERVED} must lapse at {lapse}; crediting \
             delivery time would keep it alive for another day"
        );
    }

    #[test]
    fn a_hint_dated_in_the_future_cannot_mint_freshness() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut p = provider(&dir);
        p.start(0).expect("start");
        p.add_hint(
            PeerHint::ObservedReachable {
                peer_id: peer(P1),
                address: "/ip4/127.0.0.1/tcp/1".to_owned(),
                observed_at: u64::MAX,
            },
            1_000,
        );
        let lapse = 1_000 + crate::limits::DEFAULT_TTL_MS;
        assert!(
            p.cache_mut().candidates(lapse).is_empty(),
            "a future-dated hint is clamped to now, so it lapses on schedule"
        );
    }

    #[test]
    fn a_second_address_in_the_same_millisecond_still_reaches_the_consumer() {
        // Both successes land at the same instant, so the record's expiry
        // is identical across them. A change detector keyed on expiry
        // alone calls that "unchanged" and strands the second address at
        // the provider — the consumer holds one address for a peer the
        // cache knows at two.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut p = provider(&dir);
        p.start(0).expect("start");

        const T: u64 = 5_000;
        p.cache_mut()
            .record_success(&peer(P1), "/ip4/10.0.0.1/tcp/1", T)
            .expect("recorded");
        let first = p.drain_events(T, 16);
        let first = candidate_events(&first);
        assert_eq!(first.len(), 1, "the peer is observed once");

        p.cache_mut()
            .record_success(&peer(P1), "/ip4/10.0.0.2/tcp/2", T)
            .expect("recorded");
        let second = p.drain_events(T, 16);
        let second = candidate_events(&second);

        assert_eq!(
            second.len(),
            1,
            "the peer is re-observed because its address set grew, even \
             though its expiry did not move"
        );
        match second[0] {
            DiscoveryEvent::CandidateObserved { candidate } => assert_eq!(
                candidate.addresses.len(),
                2,
                "and it carries BOTH addresses"
            ),
            other => panic!("expected an observation, got {other:?}"),
        }
    }
    #[test]
    fn an_address_the_cache_truncated_is_retracted_not_merely_omitted() {
        // At the per-peer cap, `record_success` drops the
        // least-recently-used address. The manager's view is additive, so
        // a snapshot that simply omits the dropped address leaves it
        // dialable there — this provider has to say it went.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut p = provider(&dir);
        p.start(0).expect("start");

        let cap = crate::limits::MAX_ADDRESSES_PER_PEER;
        for i in 0..cap {
            p.cache_mut()
                .record_success(&peer(P1), &format!("/ip4/10.0.0.1/tcp/{i}"), i as u64)
                .expect("within bounds");
        }
        let _ = p.drain_events(cap as u64, 64);

        // One more pushes the oldest ("/tcp/0") out of the record.
        let evicted = "/ip4/10.0.0.1/tcp/0";
        p.cache_mut()
            .record_success(&peer(P1), "/ip4/10.0.0.1/tcp/999", 1_000)
            .expect("within bounds");

        let events = p.drain_events(1_000, 64);
        let retracted = events.iter().any(|e| {
            matches!(
                e,
                DiscoveryEvent::CandidateExpired { addresses, .. }
                    if addresses.contains(evicted)
            )
        });
        assert!(
            retracted,
            "the truncated address is retracted by name: {events:?}"
        );

        // And the peer itself is still present — a selective retraction,
        // not a removal.
        assert!(
            events.iter().any(|e| matches!(
                e,
                DiscoveryEvent::CandidateObserved { candidate }
                    if candidate.addresses.contains("/ip4/10.0.0.1/tcp/999")
            )),
            "the surviving addresses are still observed: {events:?}"
        );
    }

    #[test]
    fn a_snapshot_that_drops_nothing_retracts_nothing() {
        // The positive control: adding an address below the cap must not
        // manufacture a retraction.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut p = provider(&dir);
        p.start(0).expect("start");

        p.cache_mut()
            .record_success(&peer(P1), "/ip4/10.0.0.1/tcp/1", 0)
            .expect("within bounds");
        let _ = p.drain_events(0, 64);

        p.cache_mut()
            .record_success(&peer(P1), "/ip4/10.0.0.2/tcp/2", 10)
            .expect("within bounds");
        let events = p.drain_events(10, 64);

        assert!(
            !events
                .iter()
                .any(|e| matches!(e, DiscoveryEvent::CandidateExpired { .. })),
            "nothing was dropped, so nothing is retracted: {events:?}"
        );
    }
    #[test]
    fn repeated_hints_for_one_peer_do_not_grow_the_queue() {
        // Every reachability hint moves the record's expiry, so each one
        // was a change and each change appended an event. The cache is
        // bounded; the queue was not.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut p = provider(&dir);
        p.start(0).expect("start");
        let _ = p.drain_events(0, 64);

        for i in 1..=2_000u64 {
            p.add_hint(
                PeerHint::ObservedReachable {
                    peer_id: peer(P1),
                    address: "/ip4/10.0.0.1/tcp/1".to_owned(),
                    observed_at: i,
                },
                i,
            );
            // Drain nothing: the stalled consumer.
            let _ = p.drain_events(i, 0);
        }

        let queued = p.drain_events(2_000, usize::MAX);
        let observations = queued
            .iter()
            .filter(|e| matches!(e, DiscoveryEvent::CandidateObserved { .. }))
            .count();
        assert_eq!(
            observations, 1,
            "one peer holds one pending observation however many hints \
             arrived, got {observations}"
        );
    }

    #[test]
    fn the_pending_queue_has_a_total_bound_across_peers() {
        // Per-peer coalescing bounds what one identity queues. Peers age
        // out and are replaced by different ones, which is what the total
        // bound is for.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut p = provider(&dir);
        p.start(0).expect("start");
        let _ = p.drain_events(0, 64);

        let mut now = 1u64;
        for cycle in 0..8u64 {
            for i in 0..crate::limits::MAX_PEERS {
                p.cache_mut()
                    .record_success(&synthetic(cycle, i), "/ip4/10.0.0.1/tcp/1", now)
                    .expect("within the cache's own bounds");
            }
            // Observe them WHILE THEY ARE FRESH — draining after the TTL
            // had already passed made an earlier version of this test
            // vacuous: every peer aged out before `refresh` looked, so it
            // asserted a bound on an empty queue and passed with the
            // bound removed.
            let _ = p.drain_events(now, 0);
            now += crate::limits::DEFAULT_TTL_MS + 1;
        }

        let queued = p.drain_events(now, usize::MAX);
        assert!(
            !queued.is_empty(),
            "the scenario must actually queue events, or the bound below \
             is asserted against nothing"
        );
        assert!(
            queued.len() <= MAX_PENDING_EVENTS,
            "the queue stays within its total bound, got {}",
            queued.len()
        );
    }

    #[test]
    fn a_consumer_that_drains_normally_still_sees_every_peer() {
        // The control: the cap must not be reachable in ordinary use.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut p = provider(&dir);
        p.start(0).expect("start");
        let _ = p.drain_events(0, 64);

        for i in 0..64 {
            p.cache_mut()
                .record_success(&synthetic(0, i), "/ip4/10.0.0.1/tcp/1", 10)
                .ok();
        }
        let drained = p.drain_events(10, usize::MAX);
        assert_eq!(
            candidate_events(&drained).len(),
            64,
            "every peer recorded is still reported"
        );
    }
    #[test]
    fn a_retraction_dropped_by_the_bound_is_recreated_on_the_next_drain() {
        // `refresh` advances `emitted` and then the bound discarded the
        // event it produced, so the difference was gone for good — the
        // manager would keep dialling an address this cache no longer
        // holds until its own peer-wide TTL, seven days out.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut p = provider(&dir);
        p.start(0).expect("start");
        let _ = p.drain_events(0, 64);

        // 1. The consumer LEARNS a batch of peers.
        let mut now = 10u64;
        let mut known: BTreeSet<TransportIdentity> = BTreeSet::new();
        for i in 0..crate::limits::MAX_PEERS {
            let id = synthetic(0, i);
            p.cache_mut()
                .record_success(&id, "/ip4/10.0.0.1/tcp/1", now)
                .expect("within bounds");
            known.insert(id);
        }
        for event in p.drain_events(now, usize::MAX) {
            if let DiscoveryEvent::CandidateObserved { candidate } = event {
                assert!(known.contains(&candidate.peer_id));
            }
        }

        // 2. Now it stalls. The known peers age out — owing retractions —
        // while fresh peers keep arriving, so the queue grows past the
        // bound and the trim reaches those retractions. A single batch
        // cannot get here: the cache's own 1024-peer cap holds the queue
        // at exactly the bound, which is how an earlier version of this
        // test failed to fire the trim at all.
        for cycle in 1..6u64 {
            now += crate::limits::DEFAULT_TTL_MS + 1;
            for i in 0..crate::limits::MAX_PEERS {
                p.cache_mut()
                    .record_success(&synthetic(cycle, i), "/ip4/10.0.0.1/tcp/1", now)
                    .expect("within bounds");
            }
            let _ = p.drain_events(now, 0);
        }

        // 3. Every peer the consumer was told about must still be
        // retracted. Drain repeatedly: a dropped event has to be
        // recreated, not merely survive one pass.
        let mut retracted: BTreeSet<TransportIdentity> = BTreeSet::new();
        for _ in 0..40 {
            for event in p.drain_events(now, usize::MAX) {
                if let DiscoveryEvent::CandidateExpired { peer_id, .. } = event {
                    retracted.insert(peer_id);
                }
            }
        }

        let stranded: Vec<_> = known.difference(&retracted).collect();
        assert!(
            stranded.is_empty(),
            "{} of {} peers the consumer was told about were never \
             retracted; a dropped retraction must be recreated, not lost",
            stranded.len(),
            known.len()
        );
    }
    #[test]
    fn a_trimmed_selective_retraction_is_recreated() {
        // The peer stays LIVE and only some of its addresses go, so
        // `emitted` still holds its record — `entry(..).or_default()`
        // changed nothing, the next refresh saw no address difference,
        // and the retraction was lost. The manager then keeps dialling an
        // address this cache already dropped.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut p = provider(&dir);
        p.start(0).expect("start");
        let _ = p.drain_events(0, usize::MAX);

        // One peer at its per-peer address cap, learned by the consumer.
        let subject = peer(P1);
        let cap = crate::limits::MAX_ADDRESSES_PER_PEER;
        for i in 0..cap {
            p.cache_mut()
                .record_success(&subject, &format!("/ip4/10.0.0.1/tcp/{i}"), i as u64)
                .expect("within bounds");
        }
        let _ = p.drain_events(cap as u64, usize::MAX);
        let evicted = "/ip4/10.0.0.1/tcp/0";

        // Push the oldest address out, which owes a selective retraction,
        // then bury it under enough other traffic to be trimmed.
        p.cache_mut()
            .record_success(&subject, "/ip4/10.0.0.1/tcp/999", 1_000)
            .expect("within bounds");

        // Bury it. Two things have to be true at once, and getting
        // either wrong makes the test prove nothing:
        //
        //   * the queue must actually exceed the bound, which needs
        //     churning IDENTITIES — per-peer coalescing holds a fixed
        //     peer set at roughly one event each, so re-addressing the
        //     same peers never reaches MAX_PENDING_EVENTS;
        //   * P1 must stay LIVE, or it is evicted from the cache and owes
        //     a whole-peer retraction instead, which the previous
        //     rollback already handled.
        //
        // So fresh identities arrive each cycle while P1 is touched every
        // cycle and is never the least-recently-used victim.
        let mut now = 2_000u64;
        for cycle in 1..=3u64 {
            for i in 0..(crate::limits::MAX_PEERS - 1) {
                p.cache_mut()
                    .record_success(&synthetic(cycle, i), "/ip4/10.5.0.1/tcp/1", now)
                    .expect("within bounds");
            }
            p.cache_mut()
                .record_success(&subject, "/ip4/10.0.0.1/tcp/999", now)
                .expect("within bounds");
            let _ = p.drain_events(now, 0);
            now += 1;
        }

        // Drain to settle. The retraction for the evicted address must
        // appear, however deeply it was buried.
        let mut retracted = false;
        for _ in 0..40 {
            for event in p.drain_events(now, usize::MAX) {
                if let DiscoveryEvent::CandidateExpired { addresses, .. } = event
                    && addresses.contains(evicted)
                {
                    retracted = true;
                }
            }
        }
        assert!(
            retracted,
            "the selective retraction survives the queue bound"
        );
    }
    #[test]
    fn the_health_transition_survives_a_trimmed_queue() {
        // Same rule as the other providers: the manager learns health
        // only from this event, and nothing recomputes a transition that
        // already happened.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut p = provider(&dir);
        p.start(0).expect("start");

        // Never drained since start, so the health event is the oldest
        // thing in the queue — exactly what an oldest-first trim takes.
        let mut now = 10u64;
        for cycle in 0..6u64 {
            for i in 0..crate::limits::MAX_PEERS {
                p.cache_mut()
                    .record_success(&synthetic(cycle, i), "/ip4/10.0.0.1/tcp/1", now)
                    .expect("within bounds");
            }
            let _ = p.drain_events(now, 0);
            now += crate::limits::DEFAULT_TTL_MS + 1;
        }

        let queued = p.drain_events(now, usize::MAX);
        assert!(
            queued
                .iter()
                .any(|e| matches!(e, DiscoveryEvent::HealthChanged { .. })),
            "the health transition is still delivered after {} events",
            queued.len()
        );
    }
    #[test]
    fn dropping_a_retraction_and_its_observation_together_still_recreates_both() {
        // A peer whose address set shrank queues a retraction and an
        // observation ADJACENTLY, so queue pressure takes both or
        // neither. Rolling them back in queue order restored the
        // retracted addresses and then deleted the whole record, losing
        // the retraction the first step existed to preserve.
        //
        // Driven at the bound directly: reaching this pair through the
        // public path needs 2048 later events without touching the peer,
        // and anything that produces them evicts it from the cache —
        // which turns this into the whole-peer case that already worked.
        // The state fed in is exactly what `refresh` produces.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut p = provider(&dir);
        p.start(0).expect("start");
        let _ = p.drain_events(0, usize::MAX);

        let subject = peer(P1);
        let kept = "/ip4/10.0.0.1/tcp/1";
        let removed = "/ip4/10.0.0.2/tcp/2";
        p.cache_mut()
            .record_success(&subject, kept, 10)
            .expect("within bounds");

        // The consumer was told about both addresses; the cache holds one.
        p.emitted.insert(
            subject.clone(),
            EmittedRecord {
                expires_at: None,
                addresses: [kept.to_owned(), removed.to_owned()].into_iter().collect(),
            },
        );
        p.refresh(20);
        assert!(
            p.pending.iter().any(|q| matches!(
                &q.event,
                DiscoveryEvent::CandidateExpired { addresses, .. }
                    if addresses.contains(removed)
            )),
            "the pair is queued"
        );

        // Bury the pair past the bound, then let the trim take it.
        for i in 0..MAX_PENDING_EVENTS {
            p.pending.push(Queued {
                event: DiscoveryEvent::CandidateObserved {
                    candidate: Box::new(interweave_discovery_api::CandidatePeer {
                        peer_id: synthetic(9, i),
                        addresses: [kept.to_owned()].into_iter().collect(),
                        source: SOURCE.to_owned(),
                        observed_at: 30,
                        expires_at: None,
                        protocol_observations: BTreeSet::new(),
                    }),
                },
                before: None,
            });
        }
        p.enforce_pending_bound();
        assert!(
            !p.pending.iter().any(|q| matches!(
                &q.event,
                DiscoveryEvent::CandidateExpired { addresses, .. }
                    if addresses.contains(removed)
            )),
            "the retraction was indeed trimmed, or this proves nothing"
        );

        // Draining recomputes: the retraction must come back.
        let mut recreated = false;
        for _ in 0..10 {
            for event in p.drain_events(40, usize::MAX) {
                if let DiscoveryEvent::CandidateExpired { addresses, .. } = event
                    && addresses.contains(removed)
                {
                    recreated = true;
                }
            }
        }
        assert!(
            recreated,
            "the retraction is recreated after both halves were dropped"
        );
    }
    #[test]
    fn a_dropped_whole_peer_retraction_still_retracts_the_old_address() {
        // A whole-peer `CandidateExpired` carries NO addresses by design,
        // so a rollback that reconstructs from the event, or from the
        // cache as it stands now, cannot recover what the consumer was
        // holding. If the peer is re-added with a different address
        // before the drain, the recomputed state already matches and
        // neither event is recreated — leaving the manager, whose
        // observations are additive, dialling the old address until its
        // own TTL.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut p = provider(&dir);
        p.start(0).expect("start");
        let _ = p.drain_events(0, usize::MAX);

        let subject = peer(P1);
        let old_address = "/ip4/10.0.0.1/tcp/1";
        let new_address = "/ip4/10.0.0.2/tcp/2";

        // The consumer was told about the peer at its old address.
        p.emitted.insert(
            subject.clone(),
            EmittedRecord {
                expires_at: None,
                addresses: [old_address.to_owned()].into_iter().collect(),
            },
        );
        // It is gone from the cache, then comes back at a different one.
        p.refresh(10);
        p.cache_mut()
            .record_success(&subject, new_address, 20)
            .expect("within bounds");
        p.refresh(20);

        // Bury both events past the bound.
        for i in 0..MAX_PENDING_EVENTS {
            p.pending.push(Queued {
                event: DiscoveryEvent::CandidateObserved {
                    candidate: Box::new(interweave_discovery_api::CandidatePeer {
                        peer_id: synthetic(9, i),
                        addresses: [new_address.to_owned()].into_iter().collect(),
                        source: SOURCE.to_owned(),
                        observed_at: 30,
                        expires_at: None,
                        protocol_observations: BTreeSet::new(),
                    }),
                },
                before: None,
            });
        }
        p.enforce_pending_bound();
        assert!(
            !p.pending.iter().any(|q| matches!(
                &q.event,
                DiscoveryEvent::CandidateExpired { peer_id, .. } if *peer_id == subject
            )),
            "the retraction was indeed trimmed, or this proves nothing"
        );

        let mut retracted = false;
        for _ in 0..10 {
            for event in p.drain_events(40, usize::MAX) {
                if let DiscoveryEvent::CandidateExpired { peer_id, .. } = event
                    && peer_id == subject
                {
                    retracted = true;
                }
            }
        }
        assert!(
            retracted,
            "the whole-peer retraction is recreated, so the old address is \
             withdrawn rather than left dialable"
        );
    }
}
