// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Bounds on work done for a peer that has not authenticated yet.
//!
//! # The numbers here are not this crate's to choose
//!
//! `architecture/transport/libp2p/SECURITY.md` specifies the listener
//! policy: 64 pending handshakes globally and 8 per source bucket, a
//! 10-second handshake timeout, and 30 starts per minute per bucket.
//! Those are the defaults below, and a limit that disagrees with that
//! document is a bug in this file rather than a tuning preference.
//!
//! # The window this covers
//!
//! Between accepting a TCP connection and completing Noise, the remote
//! side has proved nothing. It has no PeerId, so no trust decision can
//! apply to it, and every byte of state allocated on its behalf is state
//! an anonymous party chose to make this process hold. CLAUDE.md §5
//! requires that window to be bounded; this is where the bound lives.
//!
//! # Why the source is not an identity
//!
//! Accounting is keyed by the transport-level source — an address the
//! socket layer reports. That is NOT an identity and must never be
//! treated as one: it is unauthenticated, it is shared by everything
//! behind one NAT, and an attacker with address space can pick a new one
//! per attempt. Keying on it buys exactly one thing, which is that a
//! single source cannot consume the whole global budget. It buys no
//! trust, no authorization, and no reputation, and nothing here reads it
//! for any other purpose.
//!
//! # Why the accounting table is itself bounded
//!
//! A per-source table is a map an unauthenticated party can grow, so
//! tracking sources is the denial of service unless the tracking is
//! bounded too — the same lesson the address table learned. When the
//! table is full, an entry with nothing in flight and no live rate
//! window is forgotten, because it is carrying no information. An entry
//! that IS carrying something is never evicted to make room: dropping it
//! would let an attacker clear their own accounting by making noise from
//! other addresses, which is the laundering pattern the connection
//! policy already refuses.

use std::collections::BTreeMap;

/// Longest source label the gate will account for, in bytes.
///
/// The label comes from the local socket layer rather than from the
/// wire, so this is not expected to bind. It exists because a map key an
/// unauthenticated party influences should have a stated size.
pub const MAX_SOURCE_BYTES: usize = 128;

/// Default ceiling on handshakes in flight across all sources.
pub const DEFAULT_MAX_PENDING_TOTAL: usize = 64;

/// Default ceiling on handshakes in flight from one source bucket.
///
/// Well under the global cap on purpose: the point of per-source
/// accounting is that one bucket cannot spend the whole budget, and a
/// per-source limit equal to the total would make the split decorative.
///
/// Eight rather than a tighter number this crate might prefer, because a
/// bucket is not a peer — everything behind one NAT shares it, so a
/// stingy default refuses legitimate users at no attacker's cost.
pub const DEFAULT_MAX_PENDING_PER_SOURCE: usize = 8;

/// Default time a handshake may take before its slot is reclaimed.
///
/// A handshake that never completes is indistinguishable from one that
/// is merely slow, and the difference does not matter: either way the
/// slot must come back, or an attacker who opens connections and then
/// says nothing holds the budget for free.
pub const DEFAULT_HANDSHAKE_TIMEOUT_MS: u64 = 10_000;

/// Default number of sources tracked at once.
pub const DEFAULT_MAX_SOURCES: usize = 256;

/// Default rate-accounting window: one minute.
///
/// The window length and the count are one statement, not two tunables —
/// "30 starts per minute" says nothing on its own about how many are
/// allowed in ten seconds, and picking a shorter window with a
/// proportional count permits a burst the specification does not.
pub const DEFAULT_RATE_WINDOW_MS: u64 = 60_000;

/// Default starts one source bucket may make within a window.
pub const DEFAULT_MAX_ATTEMPTS_PER_WINDOW: u32 = 30;

/// Default starts ALL sources together may make within a window.
///
/// Per-source accounting alone does not bound the total. A bucket whose
/// handshakes complete quickly never reaches the pending ceiling and
/// never exhausts its own rate, so an attacker with twenty buckets pays
/// nothing for the twenty-first — and Noise handshakes are the CPU cost
/// this layer exists to bound, whoever asked for them.
pub const DEFAULT_MAX_GLOBAL_ATTEMPTS_PER_WINDOW: u32 = 600;

/// What the gate enforces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreAuthLimits {
    /// Handshakes in flight across all sources.
    pub max_pending_total: usize,
    /// Handshakes in flight from one source.
    pub max_pending_per_source: usize,
    /// How long a handshake may take before its slot is reclaimed.
    pub handshake_timeout_ms: u64,
    /// Sources tracked at once.
    pub max_sources: usize,
    /// Length of the rate-accounting window.
    pub rate_window_ms: u64,
    /// Starts one source bucket may make within a window.
    pub max_attempts_per_window: u32,
    /// Starts all sources together may make within a window.
    pub max_global_attempts_per_window: u32,
}

impl Default for PreAuthLimits {
    fn default() -> Self {
        Self {
            max_pending_total: DEFAULT_MAX_PENDING_TOTAL,
            max_pending_per_source: DEFAULT_MAX_PENDING_PER_SOURCE,
            handshake_timeout_ms: DEFAULT_HANDSHAKE_TIMEOUT_MS,
            max_sources: DEFAULT_MAX_SOURCES,
            rate_window_ms: DEFAULT_RATE_WINDOW_MS,
            max_attempts_per_window: DEFAULT_MAX_ATTEMPTS_PER_WINDOW,
            max_global_attempts_per_window: DEFAULT_MAX_GLOBAL_ATTEMPTS_PER_WINDOW,
        }
    }
}

/// Why a pre-authentication attempt was refused.
///
/// These are LOCAL diagnostics and must not be reported to the peer with
/// this granularity. Telling an anonymous party which specific budget it
/// exhausted describes the gate's shape to whoever is probing it; the
/// wire answer is a closed connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreAuthDenial {
    /// The global in-flight ceiling is reached.
    TooManyPending,
    /// This source already holds its share of in-flight handshakes.
    TooManyFromSource,
    /// This source bucket has made too many starts in the current window.
    RateLimited,
    /// All sources together have made too many starts in the window.
    ///
    /// Distinct from [`Self::RateLimited`] because the two say different
    /// things to an operator: one bucket is misbehaving, or the listener
    /// as a whole is past the rate it will start handshakes at. The
    /// second is the only bound that holds against an attacker with many
    /// source addresses.
    GloballyRateLimited,
    /// The source label is longer than the gate will account for.
    ///
    /// Fails CLOSED. An unaccountable source is one that could bypass
    /// per-source limits entirely, so it is refused rather than admitted
    /// untracked.
    SourceNotAccountable,
    /// Every tracked source is live, so a new one cannot be accounted for.
    ///
    /// Also fails closed, and for the same reason: admitting what cannot
    /// be counted is how an attacker escapes the count.
    NoAccountingCapacity,
    /// The runtime is draining.
    ShuttingDown,
}

/// A handshake this gate is holding a slot for.
///
/// Returned by [`PreAuthGate::admit`] and consumed by
/// [`PreAuthGate::completed`]. A token rather than a count, because
/// handshakes finish out of order: decrementing a counter would let a
/// fast handshake release the slot of a slow one, and the timeout sweep
/// would then reclaim whichever was left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandshakeSlot {
    source: String,
    id: u64,
    started_at_ms: u64,
}

impl HandshakeSlot {
    /// When the handshake started.
    #[must_use]
    pub const fn started_at_ms(&self) -> u64 {
        self.started_at_ms
    }

    /// The unauthenticated source this handshake came from.
    ///
    /// Diagnostics only. It is not an identity (see the module docs).
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }
}

#[derive(Debug, Default, Clone)]
struct SourceState {
    /// Handshake id -> start time.
    pending: BTreeMap<u64, u64>,
    window_start_ms: u64,
    attempts_in_window: u32,
    last_touched_ms: u64,
}

impl SourceState {
    /// Whether this entry is carrying something worth keeping.
    ///
    /// Nothing in flight and no live rate window means forgetting it
    /// loses no information — which is what makes eviction safe here and
    /// unsafe for an entry that is still shaping decisions.
    fn is_live_at(&self, now_ms: u64, window_ms: u64) -> bool {
        !self.pending.is_empty() || now_ms < self.window_start_ms.saturating_add(window_ms)
    }
}

/// Bounds on unauthenticated work, as a pure state machine.
///
/// No sockets, no clock, no async. Time arrives as a parameter so every
/// bound can be tested by enumeration rather than by waiting.
#[derive(Debug)]
pub struct PreAuthGate {
    limits: PreAuthLimits,
    sources: BTreeMap<String, SourceState>,
    pending_total: usize,
    next_id: u64,
    global_window_start_ms: u64,
    global_attempts_in_window: u32,
    /// Whether the runtime is draining.
    pub shutting_down: bool,
}

impl Default for PreAuthGate {
    fn default() -> Self {
        Self::new(PreAuthLimits::default())
    }
}

impl PreAuthGate {
    /// Build a gate with explicit limits.
    #[must_use]
    pub const fn new(limits: PreAuthLimits) -> Self {
        Self {
            limits,
            sources: BTreeMap::new(),
            pending_total: 0,
            next_id: 0,
            global_window_start_ms: 0,
            global_attempts_in_window: 0,
            shutting_down: false,
        }
    }

    /// Handshakes currently in flight.
    #[must_use]
    pub const fn pending(&self) -> usize {
        self.pending_total
    }

    /// Sources currently accounted for.
    #[must_use]
    pub fn tracked_sources(&self) -> usize {
        self.sources.len()
    }

    /// Admit one unauthenticated handshake, or say why not.
    ///
    /// # Errors
    /// Returns the [`PreAuthDenial`] that applied. Every refusal is
    /// local: see the type's documentation for why the reason must not
    /// reach the peer.
    pub fn admit(&mut self, source: &str, now_ms: u64) -> Result<HandshakeSlot, PreAuthDenial> {
        if self.shutting_down {
            return Err(PreAuthDenial::ShuttingDown);
        }
        if source.is_empty() || source.len() > MAX_SOURCE_BYTES {
            return Err(PreAuthDenial::SourceNotAccountable);
        }

        // BEFORE the caps are read, not after. A slot whose handshake
        // timed out is not in flight, and counting it would let one
        // stalled peer shrink the budget permanently.
        self.expire(now_ms);

        if self.pending_total >= self.limits.max_pending_total {
            return Err(PreAuthDenial::TooManyPending);
        }

        // THE GLOBAL START RATE, which per-source accounting does not
        // imply. A bucket whose handshakes complete quickly never reaches
        // the pending ceiling and never exhausts its own rate, so without
        // this an attacker with enough source addresses starts unlimited
        // Noise handshakes — and the CPU they cost is the thing this
        // layer exists to bound.
        let window_ms = self.limits.rate_window_ms;
        if now_ms >= self.global_window_start_ms.saturating_add(window_ms) {
            self.global_window_start_ms = now_ms;
            self.global_attempts_in_window = 0;
        }
        if self.global_attempts_in_window >= self.limits.max_global_attempts_per_window {
            return Err(PreAuthDenial::GloballyRateLimited);
        }

        if !self.sources.contains_key(source) && !self.make_room_for_source(now_ms) {
            return Err(PreAuthDenial::NoAccountingCapacity);
        }

        let max_attempts = self.limits.max_attempts_per_window;
        let max_per_source = self.limits.max_pending_per_source;
        let state = self.sources.entry(source.to_owned()).or_default();

        if state.pending.len() >= max_per_source {
            return Err(PreAuthDenial::TooManyFromSource);
        }

        // A window that has elapsed starts again; one that has not is
        // counted into. Sliding the start forward on every attempt would
        // let a steady stream stay under the limit forever.
        if now_ms >= state.window_start_ms.saturating_add(window_ms) {
            state.window_start_ms = now_ms;
            state.attempts_in_window = 0;
        }
        if state.attempts_in_window >= max_attempts {
            return Err(PreAuthDenial::RateLimited);
        }
        state.attempts_in_window = state.attempts_in_window.saturating_add(1);
        state.last_touched_ms = now_ms;
        // Charged only once every other check has passed, so a refusal
        // does not spend budget on a handshake that never started.
        self.global_attempts_in_window = self.global_attempts_in_window.saturating_add(1);

        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        state.pending.insert(id, now_ms);
        self.pending_total = self.pending_total.saturating_add(1);

        Ok(HandshakeSlot {
            source: source.to_owned(),
            id,
            started_at_ms: now_ms,
        })
    }

    /// Release a slot because its handshake finished.
    ///
    /// Success or failure alike: the gate bounds work in progress and
    /// does not care how it ended. Whether the peer is then authorized is
    /// a different question, asked by a different type.
    pub fn completed(&mut self, slot: &HandshakeSlot) {
        if let Some(state) = self.sources.get_mut(&slot.source)
            && state.pending.remove(&slot.id).is_some()
        {
            self.pending_total = self.pending_total.saturating_sub(1);
        }
    }

    /// Reclaim slots whose handshakes have taken too long.
    ///
    /// Returns how many were reclaimed. Called automatically by
    /// [`Self::admit`]; exposed so a runtime can also sweep on a timer,
    /// because a gate that only expires when someone knocks stays full
    /// exactly when nobody can get in.
    pub fn expire(&mut self, now_ms: u64) -> usize {
        let deadline = self.limits.handshake_timeout_ms;
        let mut reclaimed = 0;
        for state in self.sources.values_mut() {
            let expired: Vec<u64> = state
                .pending
                .iter()
                .filter(|(_, started)| now_ms.saturating_sub(**started) >= deadline)
                .map(|(id, _)| *id)
                .collect();
            for id in expired {
                state.pending.remove(&id);
                reclaimed += 1;
            }
        }
        self.pending_total = self.pending_total.saturating_sub(reclaimed);
        reclaimed
    }

    /// Forget one source that is carrying nothing, if the table is full.
    ///
    /// Returns whether there is now room. An entry with a handshake in
    /// flight or a live rate window is never evicted: forgetting it would
    /// reset the accounting an attacker is subject to, which is the same
    /// laundering the connection policy refuses when it declines to evict
    /// a live quarantine.
    fn make_room_for_source(&mut self, now_ms: u64) -> bool {
        if self.sources.len() < self.limits.max_sources {
            return true;
        }
        let window_ms = self.limits.rate_window_ms;
        let victim = self
            .sources
            .iter()
            .filter(|(_, s)| !s.is_live_at(now_ms, window_ms))
            .min_by_key(|(_, s)| s.last_touched_ms)
            .map(|(k, _)| k.clone());
        match victim {
            Some(key) => {
                self.sources.remove(&key);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> PreAuthLimits {
        PreAuthLimits {
            max_pending_total: 6,
            max_pending_per_source: 2,
            handshake_timeout_ms: 1_000,
            max_sources: 3,
            rate_window_ms: 10_000,
            max_attempts_per_window: 4,
            // High enough that the per-source cases below are testing
            // what they say they test; the global budget has its own.
            max_global_attempts_per_window: 1_000,
        }
    }

    fn gate() -> PreAuthGate {
        PreAuthGate::new(limits())
    }

    #[test]
    fn one_source_cannot_spend_the_whole_global_budget() {
        // The entire point of per-source accounting. Without it the first
        // anonymous party to arrive holds every slot, and the global cap
        // protects memory while doing nothing for availability.
        let mut g = gate();
        let a = g.admit("10.0.0.1", 0).expect("first");
        let _b = g.admit("10.0.0.1", 0).expect("second");
        assert_eq!(
            g.admit("10.0.0.1", 0),
            Err(PreAuthDenial::TooManyFromSource),
            "a source is capped below the global budget"
        );

        // And a different source is unaffected — the budget was not spent.
        g.admit("10.0.0.2", 0)
            .expect("a different source still gets in");

        // Finishing one returns that source's slot, and only that one.
        g.completed(&a);
        g.admit("10.0.0.1", 0)
            .expect("the released slot is reusable");
        assert_eq!(g.pending(), 3);
    }

    #[test]
    fn the_global_ceiling_holds_across_sources() {
        let mut g = gate();
        for i in 0..3 {
            let who = format!("10.0.0.{i}");
            g.admit(&who, 0).expect("first from this source");
            g.admit(&who, 0).expect("second from this source");
        }
        assert_eq!(g.pending(), 6);

        // A fourth source is refused on the GLOBAL cap, not the source
        // cap: it has made no attempts of its own.
        assert_eq!(g.admit("10.0.0.9", 0), Err(PreAuthDenial::TooManyPending));
    }

    #[test]
    fn a_handshake_that_never_finishes_gives_its_slot_back() {
        // An attacker who opens connections and then says nothing would
        // otherwise hold the budget for free, and a merely slow peer is
        // indistinguishable from one — which is why the answer is a
        // timeout rather than a judgement.
        let mut g = gate();
        let _held = g.admit("10.0.0.1", 0).expect("admitted");
        assert_eq!(g.pending(), 1);

        // Still in flight just before the deadline.
        assert_eq!(g.expire(999), 0);
        assert_eq!(g.pending(), 1);

        assert_eq!(g.expire(1_000), 1, "reclaimed at the deadline");
        assert_eq!(g.pending(), 0);

        // Completing a slot that already expired must not double-count
        // it back down.
        g.completed(&_held);
        assert_eq!(g.pending(), 0);
    }

    #[test]
    fn admitting_reclaims_before_it_reads_the_caps() {
        // Without this the gate stays full exactly when it matters: a
        // party that opens the whole budget and then goes silent locks
        // everyone out until something else happens to call `expire`, and
        // the thing that would have called it is the admission that is
        // being refused.
        let mut g = gate();
        for i in 0..3 {
            g.admit(&format!("10.0.0.{i}"), 0).expect("fill");
            g.admit(&format!("10.0.0.{i}"), 0).expect("fill");
        }
        // The GLOBAL cap is what binds here: every source is at its own
        // limit, so the budget is spent before the per-source check is
        // reached.
        assert_eq!(g.admit("10.0.0.1", 0), Err(PreAuthDenial::TooManyPending));

        // Long past the handshake timeout, and nobody has swept. The
        // admission itself has to do it.
        let later = 1_000 + 1;
        g.admit("10.0.0.0", later)
            .expect("a stalled peer must not hold the budget forever");
        assert_eq!(g.pending(), 1, "the dead slots really were reclaimed");
    }

    #[test]
    fn slots_are_released_by_token_not_by_counting() {
        // Handshakes finish out of order. A counter would let the fast
        // one release the slow one's slot, and the sweep would then
        // reclaim whichever was left — freeing a live handshake and
        // keeping a dead one.
        let mut g = gate();
        let slow = g.admit("10.0.0.1", 0).expect("slow");
        let fast = g.admit("10.0.0.1", 900).expect("fast");

        g.completed(&fast);
        assert_eq!(g.pending(), 1);

        // At 1000 the slow one is over the deadline and the fast one
        // would not have been. The right one goes.
        assert_eq!(g.expire(1_000), 1);
        assert_eq!(g.pending(), 0);
        assert_eq!(slow.started_at_ms(), 0);
    }

    #[test]
    fn a_source_is_rate_limited_within_its_window() {
        let mut g = gate();
        for i in 0..4u64 {
            let slot = g.admit("10.0.0.1", i).expect("within the window");
            g.completed(&slot);
        }
        assert_eq!(
            g.admit("10.0.0.1", 5),
            Err(PreAuthDenial::RateLimited),
            "the fifth attempt in one window is refused even with nothing in flight"
        );

        // The window does not slide forward on each attempt: a steady
        // stream must not stay under the limit forever.
        assert_eq!(g.admit("10.0.0.1", 9_999), Err(PreAuthDenial::RateLimited));

        // Once it genuinely elapses, the source starts again.
        g.admit("10.0.0.1", 10_000).expect("a new window");
    }

    #[test]
    fn the_source_table_is_bounded_and_forgets_only_what_is_idle() {
        // Tracking sources is itself a map an unauthenticated party can
        // grow, so it needs the same treatment as the address table.
        let mut g = gate();
        for i in 0..3 {
            let slot = g.admit(&format!("10.0.0.{i}"), 0).expect("tracked");
            g.completed(&slot);
        }
        assert_eq!(g.tracked_sources(), 3);

        // Every entry still has a live rate window, so nothing may be
        // forgotten — and admitting an untracked source would let an
        // attacker escape the accounting by cycling addresses.
        assert_eq!(
            g.admit("10.0.0.9", 0),
            Err(PreAuthDenial::NoAccountingCapacity)
        );
        assert_eq!(g.tracked_sources(), 3, "and the table did not grow");

        // Once a window lapses the entry is carrying nothing, so it can
        // go and a new source is accounted for.
        g.admit("10.0.0.9", 10_001)
            .expect("an idle entry is forgettable");
        assert_eq!(g.tracked_sources(), 3);
    }

    #[test]
    fn a_live_entry_is_never_evicted_to_make_room() {
        // The laundering pattern: if a source with a handshake in flight
        // could be forgotten, an attacker would clear their own
        // accounting by making noise from other addresses.
        // The handshake timeout has to OUTLAST the rate window here, or
        // there is no moment at which an entry is live for the in-flight
        // reason alone — the first version of this test asked at a time
        // by which everything had already timed out, and passed for the
        // wrong reason.
        let mut g = PreAuthGate::new(PreAuthLimits {
            handshake_timeout_ms: 60_000,
            rate_window_ms: 1_000,
            ..limits()
        });
        let mut held = Vec::new();
        for i in 0..3 {
            held.push(g.admit(&format!("10.0.0.{i}"), 0).expect("tracked"));
        }

        // Past every rate window, so the only thing keeping these entries
        // alive is the handshake each still has in flight.
        assert_eq!(
            g.admit("10.0.0.9", 2_000),
            Err(PreAuthDenial::NoAccountingCapacity)
        );
        assert_eq!(g.pending(), 3, "and nothing in flight was disturbed");

        // Finish one, and its entry becomes forgettable — the eviction
        // rule is about what an entry is carrying, not about its age.
        g.completed(&held[0]);
        g.admit("10.0.0.9", 2_000)
            .expect("an entry carrying nothing may be forgotten");
        assert_eq!(g.tracked_sources(), 3);
    }

    #[test]
    fn an_unaccountable_source_is_refused_rather_than_admitted_untracked() {
        // Fails closed. A source the gate cannot key on is one that would
        // bypass per-source limits entirely.
        let mut g = gate();
        assert_eq!(
            g.admit(&"x".repeat(MAX_SOURCE_BYTES + 1), 0),
            Err(PreAuthDenial::SourceNotAccountable)
        );
        assert_eq!(g.admit("", 0), Err(PreAuthDenial::SourceNotAccountable));
        assert_eq!(g.pending(), 0);
    }

    #[test]
    fn a_draining_runtime_admits_nothing() {
        let mut g = gate();
        g.shutting_down = true;
        assert_eq!(g.admit("10.0.0.1", 0), Err(PreAuthDenial::ShuttingDown));
    }

    #[test]
    fn the_defaults_are_the_ones_the_specification_states() {
        // These numbers are not this crate's to choose. `SECURITY.md`
        // fixes the listener policy, and the first version of this module
        // invented five of them instead of reading it — which is how a
        // 10-second timeout became 15 and 30 starts/minute became 192.
        //
        // Pinned here so the next disagreement is a failing test rather
        // than a review comment.
        let d = PreAuthLimits::default();
        assert_eq!(d.max_pending_total, 64, "64 pending handshakes globally");
        assert_eq!(d.max_pending_per_source, 8, "8 per source bucket");
        assert_eq!(d.handshake_timeout_ms, 10_000, "10-second timeout");
        assert_eq!(d.rate_window_ms, 60_000, "the rate window is a minute");
        assert_eq!(d.max_attempts_per_window, 30, "30 starts/minute per bucket");
        assert_eq!(
            d.max_global_attempts_per_window, 600,
            "600 starts/minute globally"
        );
    }

    #[test]
    fn the_global_start_rate_binds_across_unrelated_sources() {
        // Per-source accounting does not imply a global bound. A bucket
        // whose handshakes complete promptly never reaches the pending
        // ceiling and never exhausts its own rate, so an attacker with
        // enough addresses starts unlimited Noise handshakes — and the
        // CPU they cost is what this layer is for.
        let mut g = PreAuthGate::new(PreAuthLimits {
            max_global_attempts_per_window: 10,
            max_sources: 64,
            ..limits()
        });

        // Ten starts spread thin: every one from a different bucket, each
        // completing at once, so nothing else can be what refuses them.
        for i in 0..10 {
            let slot = g
                .admit(&format!("10.0.0.{i}"), 0)
                .expect("within the global budget");
            g.completed(&slot);
        }
        assert_eq!(g.pending(), 0, "nothing is in flight");

        assert_eq!(
            g.admit("10.0.1.1", 0),
            Err(PreAuthDenial::GloballyRateLimited),
            "a fresh bucket with an empty pending count is still refused"
        );

        // Distinct from the per-source refusal, because the two say
        // different things to whoever is reading the logs.
        assert_ne!(
            g.admit("10.0.1.2", 0),
            Err(PreAuthDenial::RateLimited),
            "the global budget is not reported as one bucket misbehaving"
        );

        // And it is a window, not a total.
        g.admit("10.0.1.3", 10_000)
            .expect("the next window starts clean");
    }

    #[test]
    fn a_refused_attempt_does_not_spend_the_global_budget() {
        // Charging on refusal would let a source that is already over its
        // own limit burn the shared budget for everyone else, turning a
        // per-source refusal into a global outage.
        let mut g = PreAuthGate::new(PreAuthLimits {
            max_global_attempts_per_window: 10,
            max_attempts_per_window: 1,
            // Room for the nine other buckets below, so the source table
            // is not what refuses them.
            max_sources: 64,
            ..limits()
        });

        g.admit("10.0.0.1", 0).expect("its one start");
        for _ in 0..50 {
            assert_eq!(g.admit("10.0.0.1", 0), Err(PreAuthDenial::RateLimited));
        }

        // Nine of the ten global starts must still be there.
        for i in 1..10 {
            let slot = g
                .admit(&format!("10.0.{i}.1"), 0)
                .expect("the global budget was not spent on refusals");
            g.completed(&slot);
        }
    }

    #[test]
    fn the_bounds_hold_under_arbitrary_sequences() {
        // The caps are the point, so they are asserted after every step
        // rather than at the end of a scripted story.
        let mut g = gate();
        let mut live: Vec<HandshakeSlot> = Vec::new();
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        for step in 0..2_000u64 {
            let r = next();
            let now = step * 7;
            match r % 3 {
                0 => {
                    if let Ok(slot) = g.admit(&format!("10.0.0.{}", r % 5), now) {
                        live.push(slot);
                    }
                }
                1 => {
                    if !live.is_empty() {
                        let slot =
                            live.swap_remove(usize::try_from(r % live.len() as u64).unwrap_or(0));
                        g.completed(&slot);
                    }
                }
                _ => {
                    let _ = g.expire(now);
                }
            }

            assert!(
                g.pending() <= limits().max_pending_total,
                "step {step}: {} in flight, cap is {}",
                g.pending(),
                limits().max_pending_total
            );
            assert!(
                g.tracked_sources() <= limits().max_sources,
                "step {step}: {} sources tracked, cap is {}",
                g.tracked_sources(),
                limits().max_sources
            );
        }
    }
}
