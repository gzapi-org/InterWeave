// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Endpoint-directory validation, freshness, and the advisory cache
//! (ADR-0031).
//!
//! The requester's half of `/interweave/endpoints/1.0.0`. What arrives is
//! hostile metadata from an authenticated peer: it says which routes that
//! peer CLAIMS to advertise, and this module decides what a cache or a UI
//! may be shown of it. Three rules, each with the test that holds it:
//!
//! - **More than 32 entries or a duplicate is a protocol violation**, not
//!   something to trim — `thirty_three_entries_is_a_violation`,
//!   `a_duplicate_is_a_violation`. Grammar is refused one layer down, by
//!   the codec, because an [`EndpointId`] cannot be constructed without
//!   it; the codec's own tests cover that.
//! - **A valid but unsorted unique list is sorted locally and flagged**,
//!   never rejected — `an_unsorted_unique_list_is_sorted_and_flagged`.
//! - **Freshness starts at local receipt and is clamped**: `ttl_ms` becomes
//!   `min(remote, local, MAX_DIRECTORY_TTL_MS)`, and `generated_at_ms` is
//!   never an input — `the_ttl_is_clamped_three_ways` and
//!   `freshness_starts_at_receipt_not_generation`.
//!
//! The cache is **in memory only**, bounded by peer count, and advisory:
//! it gates no send, grants no trust, and a stale entry followed by
//! `no_route` is expected rather than an error. There is no save or load
//! here on purpose.
//!
//! Time is a parameter. No clock is read here, so TTL behaviour is tested
//! by enumeration rather than by sleeping.

use std::collections::BTreeMap;

use interweave_transport_api::{
    EndpointDirectoryV1, EndpointId, MAX_DIRECTORY_ENTRIES, MAX_DIRECTORY_TTL_MS, TransportIdentity,
};

use crate::ingress::{IngressDenial, IngressLimiter};

/// Default local cache TTL, in milliseconds (`contracts/ENDPOINTS.md`).
pub const DEFAULT_CACHE_TTL_MS: u32 = 60_000;
/// Default bound on cached peers.
///
/// A runtime bound rather than a contract number: the contract bounds the
/// TTL and the entry count, and this is the third dimension a map needs
/// to stay a map. One entry per remote peer, evicting the oldest receipt.
pub const DEFAULT_CACHE_PEERS: usize = 64;

/// Default directory queries per minute per remote PeerId.
pub const DEFAULT_QUERIES_PER_PEER_PER_MINUTE: u32 = 12;
/// Ceiling on `queries_per_peer_per_minute`.
pub const MAX_QUERIES_PER_PEER_PER_MINUTE: u32 = 60;
/// Default concurrent directory exchanges per profile.
pub const DEFAULT_MAX_INFLIGHT: usize = 16;
/// Ceiling on `max_inflight`.
pub const MAX_INFLIGHT_CEILING: usize = 64;

/// Why the budget refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetDenial {
    /// This peer has asked too often this minute.
    PeerExhausted,
    /// Too many exchanges are in flight for the whole profile.
    InFlightExhausted,
}

/// Why a budget could not be configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetConfigError {
    /// Above [`MAX_QUERIES_PER_PEER_PER_MINUTE`], or zero.
    QueriesPerPeer(u32),
    /// Above [`MAX_INFLIGHT_CEILING`], or zero.
    MaxInflight(usize),
}

/// The directory's own bounded budget (ADR-0031), separate from direct
/// ingress so a directory flood cannot spend a message's allowance and a
/// message flood cannot spend the directory's.
///
/// Two bounds, two shapes: queries are rate-limited PER PEER, and
/// exchanges are counted IN FLIGHT per profile. There is deliberately no
/// global per-minute bucket — `IngressLimiter::per_peer_only` — because
/// the contract names none and a second aggregate would be an invented
/// limit. `the_thirteenth_query_in_a_minute_is_refused`,
/// `the_seventeenth_exchange_in_flight_is_refused`, and
/// `ending_an_exchange_admits_the_next` hold the three halves.
#[derive(Debug)]
pub struct DirectoryBudget {
    per_peer: IngressLimiter,
    inflight: usize,
    max_inflight: usize,
}

impl DirectoryBudget {
    /// Build with the contract defaults.
    #[must_use]
    pub fn with_defaults(now_ms: u64) -> Self {
        Self {
            per_peer: IngressLimiter::per_peer_only(
                DEFAULT_QUERIES_PER_PEER_PER_MINUTE,
                DEFAULT_QUERIES_PER_PEER_PER_MINUTE,
                now_ms,
            ),
            inflight: 0,
            max_inflight: DEFAULT_MAX_INFLIGHT,
        }
    }

    /// Build with explicit bounds.
    ///
    /// # Errors
    /// Returns [`BudgetConfigError`] for a zero or above-ceiling value;
    /// zero is refused because a budget that admits nothing is a disabled
    /// directory wearing the wrong error.
    pub fn new(
        queries_per_peer_per_minute: u32,
        max_inflight: usize,
        now_ms: u64,
    ) -> Result<Self, BudgetConfigError> {
        if queries_per_peer_per_minute == 0
            || queries_per_peer_per_minute > MAX_QUERIES_PER_PEER_PER_MINUTE
        {
            return Err(BudgetConfigError::QueriesPerPeer(
                queries_per_peer_per_minute,
            ));
        }
        if max_inflight == 0 || max_inflight > MAX_INFLIGHT_CEILING {
            return Err(BudgetConfigError::MaxInflight(max_inflight));
        }
        Ok(Self {
            per_peer: IngressLimiter::per_peer_only(
                queries_per_peer_per_minute,
                queries_per_peer_per_minute,
                now_ms,
            ),
            inflight: 0,
            max_inflight,
        })
    }

    /// Charge one query from `peer` and reserve one in-flight slot.
    ///
    /// The peer's rate is charged FIRST, so a peer over its own limit does
    /// not hold an in-flight slot on the way to being refused —
    /// `a_peer_over_its_rate_does_not_hold_a_slot`. On success the caller
    /// owes an [`end_exchange`](Self::end_exchange).
    ///
    /// # Errors
    /// Returns [`BudgetDenial`]; both are coarse `overloaded` on the wire.
    pub fn begin_exchange(
        &mut self,
        peer: &TransportIdentity,
        now_ms: u64,
    ) -> Result<(), BudgetDenial> {
        match self.per_peer.admit(peer, now_ms) {
            Ok(()) => {}
            Err(IngressDenial::PerPeerExhausted | IngressDenial::GlobalExhausted) => {
                return Err(BudgetDenial::PeerExhausted);
            }
        }
        if self.inflight >= self.max_inflight {
            return Err(BudgetDenial::InFlightExhausted);
        }
        self.inflight += 1;
        Ok(())
    }

    /// Release one in-flight slot.
    ///
    /// Saturating rather than panicking: an unmatched release is a caller
    /// bug, but a runtime task that dies on it is a worse one —
    /// `an_unmatched_end_does_not_underflow`.
    pub fn end_exchange(&mut self) {
        self.inflight = self.inflight.saturating_sub(1);
    }

    /// Exchanges currently in flight.
    #[must_use]
    pub const fn inflight(&self) -> usize {
        self.inflight
    }
}

/// Why a directory response was refused as a protocol violation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectoryViolation {
    /// More entries than the wire allows.
    TooManyEntries {
        /// How many arrived.
        got: usize,
    },
    /// The same endpoint listed twice.
    Duplicate(EndpointId),
}

/// A response that passed validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedDirectory {
    /// Sorted and unique.
    pub endpoints: Vec<EndpointId>,
    /// The remote's `ttl_ms`, as sent. Clamped on the way into the cache.
    pub ttl_ms: u32,
    /// The remote's `generated_at_ms`. Diagnostic only; carried so a
    /// diagnostic can show it, and so that a test can prove it is not
    /// used for anything else.
    pub generated_at_ms: u64,
    /// The list arrived unsorted and was sorted here. A bounded diagnostic
    /// signal about the remote, not a reason to distrust the entries.
    pub noncanonical: bool,
}

/// Validate a raw response.
///
/// # Errors
/// Returns [`DirectoryViolation`] for a list the wire does not permit; the
/// caller treats it as `ProtocolViolation` and caches nothing.
pub fn validate_response(
    raw: &EndpointDirectoryV1,
) -> Result<ValidatedDirectory, DirectoryViolation> {
    if raw.endpoints.len() > MAX_DIRECTORY_ENTRIES {
        return Err(DirectoryViolation::TooManyEntries {
            got: raw.endpoints.len(),
        });
    }
    let mut endpoints = raw.endpoints.clone();
    let noncanonical = !endpoints.windows(2).all(|w| w[0] <= w[1]);
    endpoints.sort();
    if let Some(dup) = endpoints.windows(2).find(|w| w[0] == w[1]) {
        return Err(DirectoryViolation::Duplicate(dup[0].clone()));
    }
    Ok(ValidatedDirectory {
        endpoints,
        ttl_ms: raw.ttl_ms,
        generated_at_ms: raw.generated_at_ms,
        noncanonical,
    })
}

/// The effective freshness of a response: the smallest of what the
/// remote offered, what this profile caches for, and the hard ceiling.
#[must_use]
pub const fn clamp_ttl(remote_ttl_ms: u32, local_cache_ttl_ms: u32) -> u32 {
    let local = if local_cache_ttl_ms < MAX_DIRECTORY_TTL_MS {
        local_cache_ttl_ms
    } else {
        MAX_DIRECTORY_TTL_MS
    };
    if remote_ttl_ms < local {
        remote_ttl_ms
    } else {
        local
    }
}

/// One cached directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEntry {
    /// Sorted, unique, validated.
    pub endpoints: Vec<EndpointId>,
    /// When this node received it — the only freshness origin.
    pub received_at_ms: u64,
    /// `received_at_ms + clamped ttl`. Exclusive.
    pub fresh_until_ms: u64,
    /// The remote's own timestamp, for diagnostics.
    pub generated_at_ms: u64,
    /// Whether the remote sent it unsorted.
    pub noncanonical: bool,
}

/// The bounded in-memory remote-directory cache.
#[derive(Debug, Clone)]
pub struct DirectoryCache {
    entries: BTreeMap<TransportIdentity, CacheEntry>,
    max_peers: usize,
    local_ttl_ms: u32,
}

impl DirectoryCache {
    /// Build a cache holding at most `max_peers` (at least one) entries,
    /// each fresh for at most `local_ttl_ms` after receipt.
    #[must_use]
    pub fn new(max_peers: usize, local_ttl_ms: u32) -> Self {
        Self {
            entries: BTreeMap::new(),
            max_peers: max_peers.max(1),
            local_ttl_ms,
        }
    }

    /// Build with the contract default TTL and the runtime default bound.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_CACHE_PEERS, DEFAULT_CACHE_TTL_MS)
    }

    /// Record a validated response received at `now_ms`, replacing any
    /// entry for the same peer.
    ///
    /// At the bound, expired entries go first and then the oldest receipt
    /// — `the_cache_evicts_at_its_bound`. Never grows past `max_peers`.
    pub fn insert(
        &mut self,
        peer: TransportIdentity,
        validated: ValidatedDirectory,
        now_ms: u64,
    ) -> &CacheEntry {
        if !self.entries.contains_key(&peer) && self.entries.len() >= self.max_peers {
            self.expire(now_ms);
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.received_at_ms)
                .map(|(p, _)| p.clone());
            if let Some(oldest) = oldest.filter(|_| self.entries.len() >= self.max_peers) {
                self.entries.remove(&oldest);
            }
        }
        let ttl = u64::from(clamp_ttl(validated.ttl_ms, self.local_ttl_ms));
        let entry = CacheEntry {
            endpoints: validated.endpoints,
            received_at_ms: now_ms,
            fresh_until_ms: now_ms.saturating_add(ttl),
            generated_at_ms: validated.generated_at_ms,
            noncanonical: validated.noncanonical,
        };
        self.entries.entry(peer).insert_entry(entry).into_mut()
    }

    /// The fresh entry for `peer` at `now_ms`, if any.
    #[must_use]
    pub fn get(&self, peer: &TransportIdentity, now_ms: u64) -> Option<&CacheEntry> {
        self.entries.get(peer).filter(|e| now_ms < e.fresh_until_ms)
    }

    /// Drop every entry that is no longer fresh at `now_ms`.
    pub fn expire(&mut self, now_ms: u64) {
        self.entries.retain(|_, e| now_ms < e.fresh_until_ms);
    }

    /// Forget one peer's entry, fresh or not.
    pub fn forget(&mut self, peer: &TransportIdentity) -> bool {
        self.entries.remove(peer).is_some()
    }

    /// Entries held, fresh or not.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    const P2: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";

    fn ep(name: &str) -> EndpointId {
        EndpointId::parse(name).expect("valid endpoint")
    }
    fn peer(s: &str) -> TransportIdentity {
        TransportIdentity::parse(s).expect("valid identity")
    }
    fn raw(names: &[&str]) -> EndpointDirectoryV1 {
        EndpointDirectoryV1 {
            generated_at_ms: 1_786_600_000_000,
            ttl_ms: 60_000,
            endpoints: names.iter().map(|n| ep(n)).collect(),
        }
    }
    fn names(list: &[EndpointId]) -> Vec<&str> {
        list.iter().map(EndpointId::as_str).collect()
    }

    #[test]
    fn a_sorted_unique_list_is_canonical() {
        let v = validate_response(&raw(&["alpha", "beta"])).expect("valid");
        assert_eq!(names(&v.endpoints), ["alpha", "beta"]);
        assert!(!v.noncanonical);
    }

    #[test]
    fn an_unsorted_unique_list_is_sorted_and_flagged() {
        let v = validate_response(&raw(&["beta", "alpha"])).expect("valid, not refused");
        assert_eq!(names(&v.endpoints), ["alpha", "beta"]);
        assert!(
            v.noncanonical,
            "the remote sent it unsorted and that is recorded"
        );
    }

    #[test]
    fn thirty_three_entries_is_a_violation() {
        let many: Vec<String> = (0..33).map(|i| format!("e{i:02}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(
            validate_response(&raw(&many)),
            Err(DirectoryViolation::TooManyEntries { got: 33 })
        );
        // And exactly 32 is fine: the bound is inclusive.
        assert!(validate_response(&raw(&many[..32])).is_ok());
    }

    #[test]
    fn a_duplicate_is_a_violation() {
        // Even when it is not adjacent as sent.
        assert_eq!(
            validate_response(&raw(&["alpha", "beta", "alpha"])),
            Err(DirectoryViolation::Duplicate(ep("alpha")))
        );
    }

    #[test]
    fn an_empty_list_is_valid() {
        let v = validate_response(&raw(&[])).expect("empty is a legal answer");
        assert!(v.endpoints.is_empty());
        assert!(!v.noncanonical);
    }

    #[test]
    fn the_ttl_is_clamped_three_ways() {
        // remote is the smallest
        assert_eq!(clamp_ttl(10_000, 60_000), 10_000);
        // local is the smallest
        assert_eq!(clamp_ttl(120_000, 60_000), 60_000);
        // both exceed the ceiling
        assert_eq!(clamp_ttl(u32::MAX, u32::MAX), MAX_DIRECTORY_TTL_MS);
        // zero is honoured: the remote said do not cache
        assert_eq!(clamp_ttl(0, 60_000), 0);
    }

    #[test]
    fn freshness_starts_at_receipt_not_generation() {
        let mut cache = DirectoryCache::new(8, 60_000);
        // The remote claims a generation time far in the FUTURE with a
        // long ttl; if either extended freshness, the entry would outlive
        // the local window.
        let mut r = raw(&["human"]);
        r.generated_at_ms = u64::MAX / 2;
        r.ttl_ms = u32::MAX;
        let v = validate_response(&r).expect("valid");
        let received = 1_000;
        cache.insert(peer(P1), v, received);
        assert!(cache.get(&peer(P1), received).is_some());
        assert!(cache.get(&peer(P1), received + 59_999).is_some());
        assert!(
            cache.get(&peer(P1), received + 60_000).is_none(),
            "fresh_until is received + clamped ttl, exclusive"
        );
    }

    #[test]
    fn expire_removes_what_get_no_longer_returns() {
        let mut cache = DirectoryCache::new(8, 1_000);
        cache.insert(peer(P1), validate_response(&raw(&["a"])).expect("valid"), 0);
        cache.insert(
            peer(P2),
            validate_response(&raw(&["b"])).expect("valid"),
            500,
        );
        cache.expire(1_000);
        assert_eq!(cache.len(), 1);
        assert!(cache.get(&peer(P2), 1_000).is_some());
    }

    #[test]
    fn the_cache_evicts_at_its_bound() {
        let mut cache = DirectoryCache::new(2, 60_000);
        cache.insert(peer(P1), validate_response(&raw(&["a"])).expect("valid"), 0);
        cache.insert(
            peer(P2),
            validate_response(&raw(&["b"])).expect("valid"),
            10,
        );
        let p3 = peer("12D3KooWQYV9dGMFoRzNStwpXztXaBUjtPqi6aU76ZgUriHhKust");
        cache.insert(
            p3.clone(),
            validate_response(&raw(&["c"])).expect("valid"),
            20,
        );
        assert_eq!(cache.len(), 2, "never past the bound");
        assert!(
            cache.get(&peer(P1), 20).is_none(),
            "the oldest receipt went"
        );
        assert!(cache.get(&peer(P2), 20).is_some());
        assert!(cache.get(&p3, 20).is_some());
    }

    #[test]
    fn reinserting_a_peer_replaces_without_growing() {
        let mut cache = DirectoryCache::new(1, 60_000);
        cache.insert(peer(P1), validate_response(&raw(&["a"])).expect("valid"), 0);
        cache.insert(peer(P1), validate_response(&raw(&["b"])).expect("valid"), 5);
        assert_eq!(cache.len(), 1);
        assert_eq!(
            names(&cache.get(&peer(P1), 5).expect("fresh").endpoints),
            ["b"]
        );
    }

    #[test]
    fn a_zero_bound_still_holds_one() {
        let mut cache = DirectoryCache::new(0, 60_000);
        cache.insert(peer(P1), validate_response(&raw(&["a"])).expect("valid"), 0);
        assert_eq!(cache.len(), 1);
    }

    // --- the budget --------------------------------------------------------

    #[test]
    fn the_thirteenth_query_in_a_minute_is_refused() {
        let mut b = DirectoryBudget::with_defaults(0);
        for _ in 0..12 {
            b.begin_exchange(&peer(P1), 0).expect("within budget");
            b.end_exchange();
        }
        assert_eq!(
            b.begin_exchange(&peer(P1), 0),
            Err(BudgetDenial::PeerExhausted)
        );
        // Another peer is unaffected: the bound is per PeerId.
        assert!(b.begin_exchange(&peer(P2), 0).is_ok());
        b.end_exchange();
        // And a minute later the first peer is admitted again.
        assert!(b.begin_exchange(&peer(P1), 60_000).is_ok());
    }

    #[test]
    fn the_seventeenth_exchange_in_flight_is_refused() {
        // One peer with rate to spare holds sixteen exchanges open; the
        // in-flight bound, not the rate, refuses the seventeenth.
        let mut b = DirectoryBudget::new(60, 16, 0).expect("valid");
        for _ in 0..16 {
            b.begin_exchange(&peer(P1), 0)
                .expect("within the in-flight bound");
        }
        assert_eq!(b.inflight(), 16);
        assert_eq!(
            b.begin_exchange(&peer(P2), 0),
            Err(BudgetDenial::InFlightExhausted),
            "the bound is per PROFILE: a different peer is refused too"
        );
    }

    #[test]
    fn ending_an_exchange_admits_the_next() {
        let mut b = DirectoryBudget::new(60, 1, 0).expect("valid");
        b.begin_exchange(&peer(P1), 0).expect("first");
        assert_eq!(
            b.begin_exchange(&peer(P2), 0),
            Err(BudgetDenial::InFlightExhausted)
        );
        b.end_exchange();
        assert!(b.begin_exchange(&peer(P2), 0).is_ok());
    }

    #[test]
    fn a_peer_over_its_rate_does_not_hold_a_slot() {
        let mut b = DirectoryBudget::new(1, 1, 0).expect("valid");
        b.begin_exchange(&peer(P1), 0).expect("first");
        b.end_exchange();
        assert_eq!(
            b.begin_exchange(&peer(P1), 0),
            Err(BudgetDenial::PeerExhausted)
        );
        assert_eq!(b.inflight(), 0, "a refused query reserves nothing");
    }

    #[test]
    fn the_budget_refuses_zero_and_above_ceiling_bounds() {
        assert_eq!(
            DirectoryBudget::new(0, 16, 0).err(),
            Some(BudgetConfigError::QueriesPerPeer(0))
        );
        assert_eq!(
            DirectoryBudget::new(61, 16, 0).err(),
            Some(BudgetConfigError::QueriesPerPeer(61))
        );
        assert_eq!(
            DirectoryBudget::new(12, 0, 0).err(),
            Some(BudgetConfigError::MaxInflight(0))
        );
        assert_eq!(
            DirectoryBudget::new(12, 65, 0).err(),
            Some(BudgetConfigError::MaxInflight(65))
        );
        assert!(DirectoryBudget::new(60, 64, 0).is_ok());
    }

    #[test]
    fn an_unmatched_end_does_not_underflow() {
        let mut b = DirectoryBudget::with_defaults(0);
        b.end_exchange();
        assert_eq!(b.inflight(), 0);
    }
}
