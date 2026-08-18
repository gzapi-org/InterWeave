// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Bounded ephemeral duplicate suppression (ADR-0019).
//!
//! A runtime-local cache: 10,000 entries, 5-minute TTL, and **persistence
//! is prohibited**. There is deliberately no save, load, or export here —
//! a durable dedup ledger would turn at-most-once-within-a-window into a
//! promise the transport does not make.
//!
//! # The key is what the SENDER addressed, not where it landed
//!
//! [`DestinationSelector`] records `Explicit(id)` or `Default` — the
//! sender's own addressing. The *resolved* endpoint is stored in the
//! entry as an outcome, never in the key. That single choice is what makes
//! a retry stable across a configuration change: if the key held the
//! resolved endpoint, an operator changing `default_direct_endpoint`
//! between a message and its retry would produce a different key, the
//! retry would look like a new message, and it would be delivered a
//! second time to a different local client.
//!
//! # Time is a parameter
//!
//! Every method that can expire an entry takes `now_ms`. No clock is read
//! here, so TTL behaviour is testable by enumeration rather than by
//! sleeping.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use interweave_transport_api::{ChannelId, EndpointId, MessageId, TransportIdentity};

use crate::fingerprint::ContentFingerprint;

/// Default entry ceiling.
pub const DEFAULT_MAX_ENTRIES: usize = 10_000;
/// Default time-to-live, in milliseconds.
pub const DEFAULT_TTL_MS: u64 = 5 * 60 * 1000;
/// Default global in-flight reservations.
pub const DEFAULT_MAX_RESERVATIONS: usize = 128;
/// Default in-flight reservations per source peer.
pub const DEFAULT_MAX_RESERVATIONS_PER_PEER: usize = 8;
/// Ceiling on the global reservation limit.
pub const MAX_RESERVATIONS_CEILING: usize = 512;
/// Ceiling on the per-peer reservation limit.
pub const MAX_RESERVATIONS_PER_PEER_CEILING: usize = 32;

/// How the sender addressed the destination.
///
/// Not the resolved endpoint. See the module note — this distinction is
/// the whole reason a retry survives a default-endpoint change.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DestinationSelector {
    /// The sender named an endpoint.
    Explicit(EndpointId),
    /// The sender omitted it, meaning the receiver's configured default.
    Default,
}

/// The normalized deduplication key.
///
/// Broadcast and direct are separate variants rather than one struct with
/// optional fields: they have genuinely different identity, and an
/// `Option<EndpointId>` shared between them would let a broadcast key
/// accidentally carry endpoint data that ADR-0030 keeps out of broadcast.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DedupKey {
    /// Broadcast identity: publisher, channel, message.
    Broadcast {
        /// The authenticated publisher.
        source_peer: TransportIdentity,
        /// The logical channel.
        channel: ChannelId,
        /// The application message identifier.
        message_id: MessageId,
    },
    /// Direct identity: publisher, source endpoint, selector, message.
    ///
    /// `source_endpoint` is part of the key because two endpoints under
    /// one PeerId may independently choose the same 128-bit id, and
    /// collapsing them would silently drop the second message.
    Direct {
        /// The authenticated sender.
        source_peer: TransportIdentity,
        /// The sender's leased endpoint.
        source_endpoint: EndpointId,
        /// How the sender addressed the destination.
        destination_selector: DestinationSelector,
        /// The application message identifier.
        message_id: MessageId,
    },
}

impl DedupKey {
    /// The peer this key belongs to, for per-peer accounting.
    #[must_use]
    pub const fn source_peer(&self) -> &TransportIdentity {
        match self {
            Self::Broadcast { source_peer, .. } | Self::Direct { source_peer, .. } => source_peer,
        }
    }
}

/// What a positive direct entry remembers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedRecord {
    /// The endpoint that actually accepted it, resolved at first admission.
    ///
    /// An outcome, not part of the key. A retry replays this stored route
    /// rather than re-resolving, which is what "the default changing does
    /// not reroute an accepted retry" means concretely.
    pub resolved_endpoint: EndpointId,
    /// The content identity, so a conflicting body is detectable.
    pub fingerprint: ContentFingerprint,
    /// When the entry was created.
    pub stored_at_ms: u64,
}

/// The outcome of presenting a message to the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// Not seen before; the caller should proceed and then record it.
    Fresh,
    /// Seen before with matching content: replay the stored route.
    ///
    /// The caller must NOT re-enqueue. The message was already delivered.
    DuplicateAccepted {
        /// The endpoint the first attempt resolved to.
        resolved_endpoint: EndpointId,
    },
    /// Same key, different content.
    ///
    /// A duplicate-ID conflict, not a retry: two different messages are
    /// claiming one identity, and accepting either would make the pair
    /// indistinguishable afterwards.
    Conflict,
}

/// A bounded LRU/TTL duplicate cache.
///
/// Deliberately not `Clone` and with no serialization: ADR-0019 prohibits
/// persistence, and a type that could be written down invites it.
#[derive(Debug)]
pub struct DedupCache {
    entries: BTreeMap<DedupKey, AcceptedRecord>,
    /// Insertion order, for eviction. A key appears once.
    order: VecDeque<DedupKey>,
    max_entries: usize,
    ttl_ms: u64,
}

impl Default for DedupCache {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_ENTRIES, DEFAULT_TTL_MS)
    }
}

impl DedupCache {
    /// Build a cache with explicit bounds.
    #[must_use]
    pub fn new(max_entries: usize, ttl_ms: u64) -> Self {
        Self {
            entries: BTreeMap::new(),
            order: VecDeque::new(),
            max_entries: max_entries.max(1),
            ttl_ms,
        }
    }

    /// Entries currently held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Present a message and learn whether it is new, a retry, or a conflict.
    ///
    /// Read-only apart from expiry: a caller that is going to reject the
    /// message must not have created an entry by asking. Recording happens
    /// in [`Self::record_accepted`], after admission actually succeeded.
    pub fn admit(
        &mut self,
        key: &DedupKey,
        fingerprint: ContentFingerprint,
        now_ms: u64,
    ) -> Admission {
        self.expire(now_ms);
        let Some(record) = self.entries.get(key) else {
            return Admission::Fresh;
        };
        if record.fingerprint != fingerprint {
            return Admission::Conflict;
        }
        let resolved_endpoint = record.resolved_endpoint.clone();
        // LRU, so a HIT is a use. Without this a frequently retried entry
        // is evicted before a newer one nobody has touched, and the next
        // retry of the hot key reads as fresh — delivering it a second
        // time inside the TTL, which is the duplicate this cache exists
        // to suppress.
        self.touch(key);
        Admission::DuplicateAccepted { resolved_endpoint }
    }

    /// Move a key to the most-recently-used end.
    fn touch(&mut self, key: &DedupKey) {
        if let Some(pos) = self.order.iter().position(|o| o == key) {
            let k = self.order.remove(pos);
            if let Some(k) = k {
                self.order.push_back(k);
            }
        }
    }

    /// Record a successful admission.
    ///
    /// Only positive outcomes are stored. ADR-0019 says rejected requests
    /// need not be cached, and caching them would be worse than useless:
    /// a message rejected because its endpoint was briefly offline would
    /// keep being rejected from cache after the route recovered.
    pub fn record_accepted(
        &mut self,
        key: DedupKey,
        resolved_endpoint: EndpointId,
        fingerprint: ContentFingerprint,
        now_ms: u64,
    ) {
        self.expire(now_ms);
        if self.entries.contains_key(&key) {
            return;
        }
        while self.entries.len() >= self.max_entries {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
        self.order.push_back(key.clone());
        self.entries.insert(
            key,
            AcceptedRecord {
                resolved_endpoint,
                fingerprint,
                stored_at_ms: now_ms,
            },
        );
    }

    /// Drop entries older than the TTL.
    pub fn expire(&mut self, now_ms: u64) {
        let ttl = self.ttl_ms;
        let expired: Vec<DedupKey> = self
            .entries
            .iter()
            .filter(|(_, r)| now_ms.saturating_sub(r.stored_at_ms) >= ttl)
            .map(|(k, _)| k.clone())
            .collect();
        for k in expired {
            self.entries.remove(&k);
            if let Some(pos) = self.order.iter().position(|o| o == &k) {
                self.order.remove(pos);
            }
        }
    }

    /// The stored record for a key, if one is live.
    #[must_use]
    pub fn get(&self, key: &DedupKey) -> Option<&AcceptedRecord> {
        self.entries.get(key)
    }
}

/// Why a reservation could not be taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationFailure {
    /// The global or per-peer reservation budget is exhausted.
    ///
    /// Surfaces as `Overloaded` locally and `overloaded` on the wire — the
    /// honest answer, and one that does not reveal whether the key was
    /// already in flight.
    Overloaded,
    /// A concurrent request holds this key with different content.
    Conflict,
}

/// What taking a reservation produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reservation {
    /// This caller owns the key and must produce the result.
    Owner,
    /// Another caller owns it; wait for and share their outcome.
    ///
    /// Never a second enqueue path. Two concurrent copies of one message
    /// must produce one delivery, which is the race this map closes.
    Waiter,
}

/// The bounded in-flight reservation map.
#[derive(Debug)]
pub struct ReservationMap {
    in_flight: BTreeMap<DedupKey, ContentFingerprint>,
    per_peer: BTreeMap<TransportIdentity, BTreeSet<DedupKey>>,
    max_global: usize,
    max_per_peer: usize,
}

impl Default for ReservationMap {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_RESERVATIONS, DEFAULT_MAX_RESERVATIONS_PER_PEER)
    }
}

impl ReservationMap {
    /// Build a map, clamped to the architecture ceilings.
    ///
    /// Clamped rather than trusted: these bound how much state one peer
    /// can make the daemon hold, so a misconfiguration must not raise them.
    #[must_use]
    pub fn new(max_global: usize, max_per_peer: usize) -> Self {
        Self {
            in_flight: BTreeMap::new(),
            per_peer: BTreeMap::new(),
            max_global: max_global.min(MAX_RESERVATIONS_CEILING),
            max_per_peer: max_per_peer.min(MAX_RESERVATIONS_PER_PEER_CEILING),
        }
    }

    /// Reservations currently held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.in_flight.len()
    }

    /// Whether nothing is in flight.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.in_flight.is_empty()
    }

    /// Take or join a reservation for a key.
    ///
    /// # Errors
    /// Returns [`ReservationFailure::Conflict`] when a concurrent request
    /// holds the key with different content, or
    /// [`ReservationFailure::Overloaded`] when a budget is exhausted.
    pub fn acquire(
        &mut self,
        key: &DedupKey,
        fingerprint: ContentFingerprint,
    ) -> Result<Reservation, ReservationFailure> {
        if let Some(existing) = self.in_flight.get(key) {
            return if *existing == fingerprint {
                Ok(Reservation::Waiter)
            } else {
                // Immediate: two different bodies claiming one identity
                // cannot both be right, and waiting would not help.
                Err(ReservationFailure::Conflict)
            };
        }
        if self.in_flight.len() >= self.max_global {
            return Err(ReservationFailure::Overloaded);
        }
        let peer = key.source_peer().clone();
        let held = self.per_peer.get(&peer).map_or(0, BTreeSet::len);
        if held >= self.max_per_peer {
            // Per-peer before global, so one noisy peer cannot consume the
            // whole budget and refuse everyone else.
            return Err(ReservationFailure::Overloaded);
        }
        self.in_flight.insert(key.clone(), fingerprint);
        self.per_peer.entry(peer).or_default().insert(key.clone());
        Ok(Reservation::Owner)
    }

    /// Release a reservation once its outcome is known.
    ///
    /// Called on rejection as well as acceptance. A rejected owner leaves
    /// **no** positive cache entry, so a later retry can succeed after the
    /// route recovers rather than being refused from cache forever.
    pub fn release(&mut self, key: &DedupKey) {
        if self.in_flight.remove(key).is_some() {
            let peer = key.source_peer().clone();
            if let Some(set) = self.per_peer.get_mut(&peer) {
                set.remove(key);
                if set.is_empty() {
                    self.per_peer.remove(&peer);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::direct_content_fingerprint_v1;

    const P1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    const P2: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";

    fn peer(s: &str) -> TransportIdentity {
        TransportIdentity::parse(s).expect("valid identity")
    }
    fn ep(name: &str) -> EndpointId {
        EndpointId::parse(name).expect("valid endpoint")
    }
    fn mid(b: u8) -> MessageId {
        MessageId::from_bytes([b; 16])
    }
    fn fp(body: &[u8]) -> ContentFingerprint {
        direct_content_fingerprint_v1(Some("text/plain"), body).expect("valid")
    }
    fn direct(selector: DestinationSelector, id: u8) -> DedupKey {
        DedupKey::Direct {
            source_peer: peer(P1),
            source_endpoint: ep("claude"),
            destination_selector: selector,
            message_id: mid(id),
        }
    }

    #[test]
    fn a_fresh_message_is_admitted_and_a_matching_retry_replays_its_route() {
        let mut c = DedupCache::default();
        let key = direct(DestinationSelector::Default, 1);
        assert_eq!(c.admit(&key, fp(b"hello"), 0), Admission::Fresh);
        c.record_accepted(key.clone(), ep("human"), fp(b"hello"), 0);
        assert_eq!(
            c.admit(&key, fp(b"hello"), 1_000),
            Admission::DuplicateAccepted {
                resolved_endpoint: ep("human")
            }
        );
    }

    #[test]
    fn a_default_endpoint_change_does_not_reroute_an_accepted_retry() {
        // The exit-gate rule, and the reason the SELECTOR is in the key
        // while the RESOLVED endpoint is only in the record. The operator
        // has repointed the default at `claude` between the two attempts;
        // the retry must still replay `human`, and must not be delivered
        // a second time to a different client.
        let mut c = DedupCache::default();
        let key = direct(DestinationSelector::Default, 7);
        c.record_accepted(key.clone(), ep("human"), fp(b"body"), 0);

        // The key is unchanged because the sender addressed it the same
        // way; only the receiver's configuration moved.
        let retry = direct(DestinationSelector::Default, 7);
        assert_eq!(retry, key);
        assert_eq!(
            c.admit(&retry, fp(b"body"), 10),
            Admission::DuplicateAccepted {
                resolved_endpoint: ep("human")
            }
        );
    }

    #[test]
    fn an_explicit_selector_is_a_different_key_from_the_default() {
        // Addressing `human` explicitly is a different request from
        // addressing the default that happens to be `human`.
        let mut c = DedupCache::default();
        let explicit = direct(DestinationSelector::Explicit(ep("human")), 3);
        let by_default = direct(DestinationSelector::Default, 3);
        assert_ne!(explicit, by_default);
        c.record_accepted(explicit, ep("human"), fp(b"x"), 0);
        assert_eq!(c.admit(&by_default, fp(b"x"), 0), Admission::Fresh);
    }

    #[test]
    fn the_same_id_with_a_different_body_is_a_conflict() {
        let mut c = DedupCache::default();
        let key = direct(DestinationSelector::Default, 2);
        c.record_accepted(key.clone(), ep("human"), fp(b"first"), 0);
        assert_eq!(c.admit(&key, fp(b"second"), 0), Admission::Conflict);
    }

    #[test]
    fn two_source_endpoints_may_use_the_same_message_id() {
        // One PeerId, two endpoints, same 128 bits. Collapsing them would
        // silently drop the second message.
        let mut c = DedupCache::default();
        let from_claude = direct(DestinationSelector::Default, 9);
        let from_human = DedupKey::Direct {
            source_peer: peer(P1),
            source_endpoint: ep("human"),
            destination_selector: DestinationSelector::Default,
            message_id: mid(9),
        };
        assert_ne!(from_claude, from_human);
        c.record_accepted(from_claude, ep("human"), fp(b"a"), 0);
        assert_eq!(c.admit(&from_human, fp(b"a"), 0), Admission::Fresh);
    }

    #[test]
    fn broadcast_and_direct_identities_are_separate_shapes() {
        let b = DedupKey::Broadcast {
            source_peer: peer(P1),
            channel: ChannelId::parse("general").expect("valid"),
            message_id: mid(1),
        };
        assert_eq!(b.source_peer(), &peer(P1));
        // A broadcast key cannot carry an endpoint at all, which is what
        // keeps ADR-0030's rule out of the type rather than in a comment.
        assert!(!matches!(b, DedupKey::Direct { .. }));
    }

    #[test]
    fn entries_expire_after_the_ttl() {
        let mut c = DedupCache::new(10, 1_000);
        let key = direct(DestinationSelector::Default, 4);
        c.record_accepted(key.clone(), ep("human"), fp(b"x"), 0);
        assert!(matches!(
            c.admit(&key, fp(b"x"), 999),
            Admission::DuplicateAccepted { .. }
        ));
        // At the TTL exactly, the entry is gone: a very late replay may
        // present again, which the ADR accepts as the cost of a bound.
        assert_eq!(c.admit(&key, fp(b"x"), 1_000), Admission::Fresh);
        assert!(c.is_empty());
    }

    #[test]
    fn the_entry_bound_evicts_oldest_first() {
        let mut c = DedupCache::new(2, DEFAULT_TTL_MS);
        for i in 0..3u8 {
            c.record_accepted(
                direct(DestinationSelector::Default, i),
                ep("human"),
                fp(b"x"),
                u64::from(i),
            );
        }
        assert_eq!(c.len(), 2);
        // The first is gone and presents as fresh again.
        assert_eq!(
            c.admit(&direct(DestinationSelector::Default, 0), fp(b"x"), 3),
            Admission::Fresh
        );
        assert!(matches!(
            c.admit(&direct(DestinationSelector::Default, 2), fp(b"x"), 3),
            Admission::DuplicateAccepted { .. }
        ));
    }

    #[test]
    fn a_hit_refreshes_recency_so_a_hot_entry_is_not_evicted_first() {
        // Insert A and B, use A, then insert C. A cold-insertion-order
        // cache would evict A — the entry actually in use — and the next
        // retry of A would read as fresh and be delivered again inside
        // the TTL.
        let mut c = DedupCache::new(2, DEFAULT_TTL_MS);
        let a = direct(DestinationSelector::Default, 1);
        let b = direct(DestinationSelector::Default, 2);
        let d = direct(DestinationSelector::Default, 3);
        c.record_accepted(a.clone(), ep("human"), fp(b"x"), 0);
        c.record_accepted(b.clone(), ep("human"), fp(b"x"), 1);

        assert!(matches!(
            c.admit(&a, fp(b"x"), 2),
            Admission::DuplicateAccepted { .. }
        ));

        c.record_accepted(d, ep("human"), fp(b"x"), 3);
        // A survives because it was used; B, untouched, is the eviction.
        assert!(matches!(
            c.admit(&a, fp(b"x"), 4),
            Admission::DuplicateAccepted { .. }
        ));
        assert_eq!(c.admit(&b, fp(b"x"), 4), Admission::Fresh);
    }

    #[test]
    fn asking_does_not_create_an_entry() {
        // A caller about to reject the message must not have cached it by
        // enquiring.
        let mut c = DedupCache::default();
        let key = direct(DestinationSelector::Default, 5);
        assert_eq!(c.admit(&key, fp(b"x"), 0), Admission::Fresh);
        assert!(c.is_empty());
    }

    #[test]
    fn the_first_concurrent_request_owns_and_the_second_waits() {
        let mut m = ReservationMap::default();
        let key = direct(DestinationSelector::Default, 1);
        assert_eq!(m.acquire(&key, fp(b"x")), Ok(Reservation::Owner));
        assert_eq!(m.acquire(&key, fp(b"x")), Ok(Reservation::Waiter));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn a_concurrent_request_with_different_content_conflicts_immediately() {
        let mut m = ReservationMap::default();
        let key = direct(DestinationSelector::Default, 1);
        m.acquire(&key, fp(b"first")).expect("owner");
        assert_eq!(
            m.acquire(&key, fp(b"second")),
            Err(ReservationFailure::Conflict)
        );
    }

    #[test]
    fn a_released_rejection_leaves_no_entry_so_a_later_retry_can_succeed() {
        // The owner was rejected because its endpoint was briefly offline.
        // Nothing positive was cached, so the retry gets a real attempt.
        let mut m = ReservationMap::default();
        let mut c = DedupCache::default();
        let key = direct(DestinationSelector::Default, 6);
        m.acquire(&key, fp(b"x")).expect("owner");
        m.release(&key);
        assert!(m.is_empty());
        assert_eq!(c.admit(&key, fp(b"x"), 0), Admission::Fresh);
    }

    #[test]
    fn one_peer_cannot_consume_the_whole_reservation_budget() {
        let mut m = ReservationMap::new(64, 2);
        for i in 0..2u8 {
            m.acquire(&direct(DestinationSelector::Default, i), fp(b"x"))
                .expect("within per-peer budget");
        }
        assert_eq!(
            m.acquire(&direct(DestinationSelector::Default, 9), fp(b"x")),
            Err(ReservationFailure::Overloaded)
        );
        // A different peer is unaffected, which is the point of the
        // per-peer bound existing alongside the global one.
        let other = DedupKey::Direct {
            source_peer: peer(P2),
            source_endpoint: ep("claude"),
            destination_selector: DestinationSelector::Default,
            message_id: mid(9),
        };
        assert_eq!(m.acquire(&other, fp(b"x")), Ok(Reservation::Owner));
    }

    #[test]
    fn reservation_limits_are_clamped_to_the_ceilings() {
        let m = ReservationMap::new(usize::MAX, usize::MAX);
        assert_eq!(m.max_global, MAX_RESERVATIONS_CEILING);
        assert_eq!(m.max_per_peer, MAX_RESERVATIONS_PER_PEER_CEILING);
    }
}
