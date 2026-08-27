// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Direct-ingress rate limiting and the local subscription registry.
//!
//! # The buckets apply to trusted peers
//!
//! These run *after* Noise and trust admission (ADR-0026), which is the
//! point: they bound a peer that is authorized and misbehaving. An
//! untrusted peer never reaches here, and pre-authentication resource use
//! is bounded by the connection layer before a PeerId exists at all.
//!
//! The source EndpointId is deliberately **not** a bucket dimension. It is
//! peer-asserted metadata, so keying on it would let one peer multiply its
//! own allowance by inventing endpoint names — and would make endpoint
//! names an unbounded metric label besides.

use std::collections::{BTreeMap, BTreeSet};

use interweave_transport_api::{ChannelId, TransportIdentity};

/// Default per-peer refill, tokens per minute.
pub const DEFAULT_PER_PEER_PER_MINUTE: u32 = 120;
/// Default per-peer burst.
pub const DEFAULT_PER_PEER_BURST: u32 = 32;
/// Default global refill, tokens per minute.
pub const DEFAULT_GLOBAL_PER_MINUTE: u32 = 1_200;
/// Default global burst.
pub const DEFAULT_GLOBAL_BURST: u32 = 256;

const MS_PER_MINUTE: u64 = 60_000;

/// A single token bucket.
///
/// Refills continuously from elapsed time rather than on a timer, so
/// behaviour does not depend on when a tick happened to run — and so the
/// whole thing is testable by passing timestamps.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Bucket {
    tokens: u32,
    burst: u32,
    per_minute: u32,
    last_refill_ms: u64,
}

impl Bucket {
    const fn new(per_minute: u32, burst: u32, now_ms: u64) -> Self {
        Self {
            tokens: burst,
            burst,
            per_minute,
            last_refill_ms: now_ms,
        }
    }

    fn refill(&mut self, now_ms: u64) {
        if self.per_minute == 0 {
            return;
        }
        let elapsed = now_ms.saturating_sub(self.last_refill_ms);
        if elapsed == 0 {
            return;
        }
        let gained = (elapsed.saturating_mul(u64::from(self.per_minute)) / MS_PER_MINUTE)
            .min(u64::from(u32::MAX));
        if gained == 0 {
            // Not a whole token yet. Do NOT advance the clock: the
            // fraction earned so far must survive to the next call.
            return;
        }
        self.tokens = self
            .tokens
            .saturating_add(u32::try_from(gained).unwrap_or(u32::MAX))
            .min(self.burst);
        // Advance by the time the CREDITED tokens represent, not to
        // `now_ms`. Jumping to now would discard the sub-token remainder
        // on every refill, and the loss compounds: at 120/minute with
        // arrivals every 750 ms each call earns 1.5 tokens, credits 1,
        // and forfeits 0.5 — delivering 80/minute against a configured
        // 120. Keeping the remainder makes the long-run rate the
        // configured one.
        let consumed_ms = gained.saturating_mul(MS_PER_MINUTE) / u64::from(self.per_minute);
        self.last_refill_ms = self.last_refill_ms.saturating_add(consumed_ms);
    }

    fn try_consume(&mut self, now_ms: u64) -> bool {
        self.refill(now_ms);
        if self.tokens == 0 {
            return false;
        }
        self.tokens -= 1;
        true
    }
}

/// Why ingress was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressDenial {
    /// This peer's own allowance is exhausted.
    PerPeerExhausted,
    /// The shared allowance is exhausted.
    GlobalExhausted,
}

/// Per-peer and global direct-ingress buckets.
#[derive(Debug)]
pub struct IngressLimiter {
    per_peer: BTreeMap<TransportIdentity, Bucket>,
    global: Bucket,
    per_peer_per_minute: u32,
    per_peer_burst: u32,
    last_prune_ms: u64,
}

/// How often admission sweeps fully-refilled buckets.
///
/// A DEFENCE AGAINST THE HELPER BEING FORGOTTEN. `prune_idle` is public
/// and explains why an unpruned map grows forever, and for the whole of
/// this stage nothing called it: the only caller was its own test. A
/// bound that depends on someone remembering to invoke it is not a
/// bound, so the sweep now runs from inside `admit` and no caller can
/// omit it.
///
/// One minute, which is comfortably longer than a bucket takes to refill
/// at the configured defaults (burst 32 at 120/minute is sixteen
/// seconds), so a sweep finds the peers that have genuinely gone quiet
/// rather than ones mid-conversation.
const PRUNE_INTERVAL_MS: u64 = 60_000;

impl IngressLimiter {
    /// Build a limiter with the contract defaults.
    #[must_use]
    pub const fn with_defaults(now_ms: u64) -> Self {
        Self::new(
            DEFAULT_PER_PEER_PER_MINUTE,
            DEFAULT_PER_PEER_BURST,
            DEFAULT_GLOBAL_PER_MINUTE,
            DEFAULT_GLOBAL_BURST,
            now_ms,
        )
    }

    /// Build a limiter with explicit rates.
    #[must_use]
    pub const fn new(
        per_peer_per_minute: u32,
        per_peer_burst: u32,
        global_per_minute: u32,
        global_burst: u32,
        now_ms: u64,
    ) -> Self {
        Self {
            per_peer: BTreeMap::new(),
            global: Bucket::new(global_per_minute, global_burst, now_ms),
            per_peer_per_minute,
            per_peer_burst,
            last_prune_ms: now_ms,
        }
    }

    /// Admit one inbound direct request.
    ///
    /// The per-peer bucket is charged **first**, and the global bucket is
    /// only charged once the peer's own check passed. Charging global
    /// first would let a peer already over its own limit still consume
    /// shared allowance on the way to being refused — spending everyone
    /// else's budget to be told no.
    ///
    /// # Errors
    /// Returns [`IngressDenial`] naming which bound was hit; both surface
    /// as coarse `overloaded` on the wire.
    pub fn admit(&mut self, peer: &TransportIdentity, now_ms: u64) -> Result<(), IngressDenial> {
        // THE SWEEP RUNS HERE so that it runs at all. A peer dropped from
        // the trust allowlist stops sending, its bucket refills to full,
        // and a full bucket is indistinguishable from one never seen —
        // so it is pure retained state. Trust rotations therefore grew
        // this map for the lifetime of the process.
        //
        // Pruning BEFORE the entry below is deliberate: if this peer's
        // own bucket is swept, `entry` recreates it full, which is
        // exactly what it already was.
        if now_ms.saturating_sub(self.last_prune_ms) >= PRUNE_INTERVAL_MS {
            self.prune_idle(now_ms);
            self.last_prune_ms = now_ms;
        }
        let per_peer_per_minute = self.per_peer_per_minute;
        let per_peer_burst = self.per_peer_burst;
        let bucket = self
            .per_peer
            .entry(peer.clone())
            .or_insert_with(|| Bucket::new(per_peer_per_minute, per_peer_burst, now_ms));
        if !bucket.try_consume(now_ms) {
            return Err(IngressDenial::PerPeerExhausted);
        }
        if !self.global.try_consume(now_ms) {
            return Err(IngressDenial::GlobalExhausted);
        }
        Ok(())
    }

    /// Forget peers whose buckets are full, bounding the map.
    ///
    /// A peer at full allowance is indistinguishable from one never seen,
    /// so retaining it is pure state growth — which is what an attacker
    /// cycling identities would otherwise cause.
    pub fn prune_idle(&mut self, now_ms: u64) {
        self.per_peer.retain(|_, b| {
            b.refill(now_ms);
            b.tokens < b.burst
        });
    }

    /// Number of peers currently tracked.
    #[must_use]
    pub fn tracked_peers(&self) -> usize {
        self.per_peer.len()
    }
}

/// Hard architectural ceiling on subscriptions held by one profile.
///
/// `resource-limits.md` states 128 default and 1024 as the ceiling. The
/// ceiling is what this type enforces, because a narrower deployment
/// value is configuration and this is the bound below which the design
/// stops being bounded.
pub const MAX_SUBSCRIPTIONS: usize = 1_024;

/// Hard ceiling on local sessions holding a join on ONE channel.
///
/// Derived rather than chosen: the same document caps IPC connections
/// (data and admin combined) at 64, and a join is held by a local
/// session. More joins on one channel than there can be sessions is a
/// number that cannot be reached honestly, so reaching it means
/// `release_session` has been missed somewhere and the set is leaking.
pub const MAX_SESSIONS_PER_CHANNEL: usize = 64;

/// Why a join was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionDenial {
    /// The profile already holds [`MAX_SUBSCRIPTIONS`] channels.
    TooManySubscriptions,
    /// This channel already has [`MAX_SESSIONS_PER_CHANNEL`] joins.
    TooManySessions,
}

impl core::fmt::Display for SubscriptionDenial {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::TooManySubscriptions => "the profile holds its maximum subscriptions",
            Self::TooManySessions => "the channel holds its maximum local joins",
        })
    }
}

impl core::error::Error for SubscriptionDenial {}

/// Local join references for broadcast channels.
///
/// A **client** join, which is not the same as the daemon's topic
/// subscription. A profile may keep a mesh warm with `channels.desired`
/// while zero clients hold a join; inbound messages then have no local
/// consumer and are dropped rather than buffered, which is the no-buffer
/// rule holding at this layer too.
///
/// # Both maps are bounded
///
/// `join` used to be infallible and both collections grew without a
/// ceiling: a local client could name channels until the process ran
/// out of memory, and a session set could accumulate entries a missed
/// `release_session` never removed. The adversary is a local client
/// rather than the network, which changes who can reach it and not
/// whether the structure is bounded -- and this crate's own rule is
/// that a map an outside party can grow is a bound or it is a leak.
///
/// It is enforced now rather than when the subscription port is
/// activated, because "the caller will remember the limit" is how the
/// dial gate, the source bucket, and the peer cache each lost theirs.
#[derive(Debug, Default)]
pub struct SubscriptionRegistry {
    joins: BTreeMap<ChannelId, BTreeSet<String>>,
    desired: BTreeSet<ChannelId>,
}

impl SubscriptionRegistry {
    /// Build a registry with the profile's warm-mesh channels.
    ///
    /// # Errors
    /// Returns [`SubscriptionDenial::TooManySubscriptions`] if more than
    /// [`MAX_SUBSCRIPTIONS`] channels are desired. A warm mesh costs the
    /// same resources as a joined one.
    pub fn new(desired: BTreeSet<ChannelId>) -> Result<Self, SubscriptionDenial> {
        if desired.len() > MAX_SUBSCRIPTIONS {
            return Err(SubscriptionDenial::TooManySubscriptions);
        }
        Ok(Self {
            joins: BTreeMap::new(),
            desired,
        })
    }

    /// Replace the profile's desired set, keeping every live join.
    ///
    /// A reconfigure is an operator action on warm-mesh policy, not a
    /// client disconnect — so unlike opening an endpoint queue, which
    /// discards what the previous lease holder held, this leaves client
    /// joins alone. A session that joined a channel the new profile no
    /// longer desires still holds it.
    ///
    /// # Errors
    /// [`SubscriptionDenial::TooManySubscriptions`] if the resulting set
    /// would exceed [`MAX_SUBSCRIPTIONS`]. Counted against the joins
    /// already held rather than against the new set alone, because both
    /// cost the same and the ceiling is on what this profile holds.
    pub fn set_desired(&mut self, desired: BTreeSet<ChannelId>) -> Result<(), SubscriptionDenial> {
        let joined_elsewhere = self.joins.keys().filter(|c| !desired.contains(*c)).count();
        if joined_elsewhere.saturating_add(desired.len()) > MAX_SUBSCRIPTIONS {
            return Err(SubscriptionDenial::TooManySubscriptions);
        }
        self.desired = desired;
        Ok(())
    }

    /// Record that a session joined a channel.
    ///
    /// Idempotent: re-joining a channel this session already holds
    /// succeeds without consuming anything, so a client that retries
    /// cannot exhaust a bound by repeating itself.
    ///
    /// # Errors
    /// Returns [`SubscriptionDenial`] naming the bound that is full.
    pub fn join(
        &mut self,
        channel: ChannelId,
        session: impl Into<String>,
    ) -> Result<(), SubscriptionDenial> {
        let session = session.into();
        // COUNTED AGAINST BOTH MAPS BEFORE EITHER IS TOUCHED. Inserting
        // the channel and then refusing the session would leave an
        // empty entry behind, which is the subscription ceiling being
        // consumed by a join that did not happen.
        match self.joins.get(&channel) {
            Some(sessions) => {
                if sessions.contains(&session) {
                    return Ok(());
                }
                if sessions.len() >= MAX_SESSIONS_PER_CHANNEL {
                    return Err(SubscriptionDenial::TooManySessions);
                }
            }
            None => {
                // A desired channel is already counted, so joining one
                // does not consume a second slot.
                let held = self.subscriptions();
                if !self.desired.contains(&channel) && held >= MAX_SUBSCRIPTIONS {
                    return Err(SubscriptionDenial::TooManySubscriptions);
                }
            }
        }
        self.joins.entry(channel).or_default().insert(session);
        Ok(())
    }

    /// Channels this profile holds, joined or merely desired.
    #[must_use]
    pub fn subscriptions(&self) -> usize {
        self.joins
            .keys()
            .filter(|c| !self.desired.contains(*c))
            .count()
            + self.desired.len()
    }

    /// Record that a session left a channel.
    pub fn leave(&mut self, channel: &ChannelId, session: &str) {
        if let Some(set) = self.joins.get_mut(channel) {
            set.remove(session);
            if set.is_empty() {
                self.joins.remove(channel);
            }
        }
    }

    /// Whether `session` still holds any join.
    ///
    /// The question a leave must ask before closing the session's queue:
    /// a session that left one channel of several is still live, and
    /// closing its queue would drop the deliveries it is owed on the
    /// others.
    #[must_use]
    pub fn holds_any(&self, session: &str) -> bool {
        self.joins.values().any(|set| set.contains(session))
    }

    /// Drop every join held by a session, as disconnect does.
    pub fn release_session(&mut self, session: &str) {
        self.joins.retain(|_, set| {
            set.remove(session);
            !set.is_empty()
        });
    }

    /// Whether this session may publish to this channel.
    ///
    /// A caller-owned join is required. The runtime does not implicitly
    /// subscribe, and it does not borrow another local client's join —
    /// publishing on someone else's subscription would let one client's
    /// membership authorize another's traffic.
    #[must_use]
    pub fn may_publish(&self, channel: &ChannelId, session: &str) -> bool {
        self.joins
            .get(channel)
            .is_some_and(|set| set.contains(session))
    }

    /// Sessions that should receive an inbound broadcast.
    ///
    /// Empty when nobody joined, even if the mesh is warm: a desired
    /// channel with no local consumer delivers to nobody and stores
    /// nothing.
    #[must_use]
    pub fn subscribers(&self, channel: &ChannelId) -> Vec<String> {
        self.joins
            .get(channel)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Whether the backend should hold a topic subscription.
    ///
    /// True when a client has joined **or** the profile desires it. The
    /// second case is what keeps a mesh warm with no local consumer.
    #[must_use]
    pub fn backend_should_subscribe(&self, channel: &ChannelId) -> bool {
        self.joins.contains_key(channel) || self.desired.contains(channel)
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_session_holding_one_of_two_channels_is_still_live() {
        let mut subs = SubscriptionRegistry::new(BTreeSet::new()).expect("an empty profile");
        let one = ChannelId::parse("one").expect("legal");
        let two = ChannelId::parse("two").expect("legal");
        subs.join(one.clone(), String::from("s")).expect("joins");
        subs.join(two.clone(), String::from("s")).expect("joins");

        subs.leave(&one, "s");
        assert!(
            subs.holds_any("s"),
            "a session that left one channel of two still holds the other"
        );

        subs.leave(&two, "s");
        assert!(
            !subs.holds_any("s"),
            "and holds nothing once the last one is left"
        );
    }

    #[test]
    fn one_sessions_leave_does_not_release_another() {
        let mut subs = SubscriptionRegistry::new(BTreeSet::new()).expect("an empty profile");
        let c = ChannelId::parse("shared").expect("legal");
        subs.join(c.clone(), String::from("mine")).expect("joins");
        subs.join(c.clone(), String::from("theirs")).expect("joins");

        subs.leave(&c, "mine");
        assert!(!subs.holds_any("mine"));
        assert!(
            subs.holds_any("theirs"),
            "a channel is not released for everyone by one session leaving it"
        );
    }

    #[test]
    fn an_unknown_session_holds_nothing() {
        let subs = SubscriptionRegistry::new(BTreeSet::new()).expect("an empty profile");
        assert!(!subs.holds_any("never-seen"));
    }
    use super::*;

    const P1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    const P2: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";

    fn peer(s: &str) -> TransportIdentity {
        TransportIdentity::parse(s).expect("valid identity")
    }
    /// A distinct syntactically valid identity per byte. Not real keys.
    fn other_peer(tag: u8) -> TransportIdentity {
        let tail: String = std::iter::repeat_n(char::from(tag), 44).collect();
        TransportIdentity::parse(format!("Qm{tail}")).expect("valid identity")
    }

    fn ch(n: &str) -> ChannelId {
        ChannelId::parse(n).expect("valid channel")
    }

    #[test]
    fn a_local_client_cannot_grow_the_registry_without_a_ceiling() {
        // Both maps grew without a bound: a client could name channels
        // until the process ran out of memory, and a session set could
        // accumulate entries a missed `release_session` never removed.
        // A local client rather than the network is the party who can
        // reach it, which changes who -- not whether the structure is
        // bounded.
        let mut r = SubscriptionRegistry::default();
        for i in 0..MAX_SUBSCRIPTIONS {
            r.join(ch(&format!("c{i}")), "a")
                .expect("within the ceiling");
        }
        assert_eq!(r.subscriptions(), MAX_SUBSCRIPTIONS);
        assert_eq!(
            r.join(ch("one-too-many"), "a"),
            Err(SubscriptionDenial::TooManySubscriptions)
        );
        // And the refusal left nothing behind. An entry inserted before
        // the check would consume a slot for a join that did not happen.
        assert_eq!(r.subscriptions(), MAX_SUBSCRIPTIONS);

        // Another session joining a channel already held is free: the
        // ceiling is on channels, not on joins.
        r.join(ch("c0"), "b").expect("an existing channel");

        // Leaving frees the slot, so the bound is a ceiling and not a
        // lifetime budget.
        r.leave(&ch("c1"), "a");
        r.join(ch("one-too-many"), "a")
            .expect("a released slot is reusable");
    }

    #[test]
    fn one_channel_cannot_hold_more_joins_than_there_can_be_sessions() {
        // The number is derived, not chosen: IPC connections cap at 64,
        // and a join is held by a local session. Exceeding it means
        // `release_session` was missed somewhere and the set is leaking.
        let mut r = SubscriptionRegistry::default();
        for i in 0..MAX_SESSIONS_PER_CHANNEL {
            r.join(ch("general"), format!("s{i}"))
                .expect("within the ceiling");
        }
        assert_eq!(
            r.join(ch("general"), "one-too-many"),
            Err(SubscriptionDenial::TooManySessions)
        );

        // Re-joining is idempotent and costs nothing, so a client that
        // retries cannot exhaust a bound by repeating itself.
        r.join(ch("general"), "s0").expect("already joined");
        assert_eq!(
            r.subscribers(&ch("general")).len(),
            MAX_SESSIONS_PER_CHANNEL
        );

        // A different channel is unaffected: the bound is per channel.
        r.join(ch("builds"), "one-too-many")
            .expect("a different channel has its own allowance");
    }

    #[test]
    fn a_desired_channel_is_counted_once_and_not_twice() {
        // A warm mesh costs the same resources as a joined one, so
        // `desired` counts against the ceiling -- and a client joining
        // a channel the profile already desires must not consume a
        // second slot, which would make the effective ceiling depend on
        // how the channel was reached.
        let desired: std::collections::BTreeSet<ChannelId> = (0..MAX_SUBSCRIPTIONS)
            .map(|i| ch(&format!("d{i}")))
            .collect();
        let mut r = SubscriptionRegistry::new(desired).expect("exactly the ceiling");
        assert_eq!(r.subscriptions(), MAX_SUBSCRIPTIONS);

        r.join(ch("d0"), "a")
            .expect("joining a desired channel adds no subscription");
        assert_eq!(r.subscriptions(), MAX_SUBSCRIPTIONS);
        assert_eq!(
            r.join(ch("new"), "a"),
            Err(SubscriptionDenial::TooManySubscriptions)
        );

        // And the constructor refuses more than it can hold rather than
        // accepting a registry that is over its own bound from birth.
        let too_many: std::collections::BTreeSet<ChannelId> = (0..=MAX_SUBSCRIPTIONS)
            .map(|i| ch(&format!("d{i}")))
            .collect();
        assert_eq!(
            SubscriptionRegistry::new(too_many).err(),
            Some(SubscriptionDenial::TooManySubscriptions)
        );
    }

    #[test]
    fn a_burst_is_allowed_and_then_the_peer_is_refused() {
        let mut l = IngressLimiter::new(120, 4, 1_200, 256, 0);
        for i in 0..4 {
            assert!(l.admit(&peer(P1), 0).is_ok(), "burst token {i}");
        }
        assert_eq!(l.admit(&peer(P1), 0), Err(IngressDenial::PerPeerExhausted));
    }

    #[test]
    fn tokens_refill_with_elapsed_time() {
        let mut l = IngressLimiter::new(60, 2, 1_200, 256, 0);
        l.admit(&peer(P1), 0).expect("first");
        l.admit(&peer(P1), 0).expect("second");
        assert!(l.admit(&peer(P1), 0).is_err());
        // 60/minute is one per second.
        assert!(l.admit(&peer(P1), 1_000).is_ok());
    }

    #[test]
    fn a_slow_steady_stream_is_not_starved_by_lost_remainders() {
        // If the clock advanced on every call regardless of whether a
        // whole token was earned, each arrival would forfeit its fraction
        // and the bucket would never refill.
        let mut l = IngressLimiter::new(60, 1, 1_200, 256, 0);
        l.admit(&peer(P1), 0).expect("initial token");
        // Poll every 100 ms, far below the 1 s a token takes.
        for t in (100..1_000).step_by(100) {
            assert!(l.admit(&peer(P1), t).is_err(), "at {t} ms");
        }
        // The earned token still arrives on schedule.
        assert!(l.admit(&peer(P1), 1_000).is_ok());
    }

    #[test]
    fn the_long_run_rate_is_the_configured_rate() {
        // The remainder bug, demonstrated where it actually bites: a poll
        // interval that does not divide the token interval. At 120/minute
        // a token is due every 500 ms; polling every 300 ms credits one
        // token per 600 ms and used to forfeit the extra 100 ms each
        // time, delivering 100/minute against a configured 120.
        //
        // The bucket is drained first so the measurement is of REFILL,
        // not of the initial burst.
        let mut l = IngressLimiter::new(120, 120, 1_000_000, 1_000_000, 0);
        for _ in 0..120 {
            l.admit(&peer(P1), 0).expect("draining the initial burst");
        }

        let mut admitted = 0;
        for t in (300..=60_000).step_by(300) {
            if l.admit(&peer(P1), t).is_ok() {
                admitted += 1;
            }
        }
        assert_eq!(
            admitted, 120,
            "a minute of refill should yield the configured 120 tokens"
        );
    }

    #[test]
    fn one_peer_exhausting_itself_does_not_spend_the_global_budget() {
        // Per-peer is charged first, so a peer already over its limit
        // cannot consume shared allowance on its way to being refused.
        let mut l = IngressLimiter::new(0, 1, 0, 4, 0);
        l.admit(&peer(P1), 0).expect("its one token");
        for _ in 0..10 {
            assert_eq!(l.admit(&peer(P1), 0), Err(IngressDenial::PerPeerExhausted));
        }
        // Three global tokens remain, and three OTHER peers can spend
        // them. Had the global bucket been charged first, P1's ten
        // refusals would have drained it and these would fail.
        assert!(l.admit(&peer(P2), 0).is_ok());
        assert!(l.admit(&other_peer(b'b'), 0).is_ok());
        assert!(l.admit(&other_peer(b'c'), 0).is_ok());
        // And now the global bucket really is empty.
        assert_eq!(
            l.admit(&other_peer(b'd'), 0),
            Err(IngressDenial::GlobalExhausted)
        );
    }

    #[test]
    fn the_global_bucket_still_bounds_the_aggregate() {
        let mut l = IngressLimiter::new(120, 32, 0, 2, 0);
        assert!(l.admit(&peer(P1), 0).is_ok());
        assert!(l.admit(&peer(P2), 0).is_ok());
        assert_eq!(l.admit(&peer(P1), 0), Err(IngressDenial::GlobalExhausted));
    }

    #[test]
    fn idle_peers_are_pruned_so_the_map_cannot_grow_forever() {
        let mut l = IngressLimiter::new(60_000, 4, 1_200, 256, 0);
        l.admit(&peer(P1), 0).expect("ok");
        assert_eq!(l.tracked_peers(), 1);
        // After refilling to full the peer is indistinguishable from one
        // never seen, so retaining it is pure state growth.
        l.prune_idle(60_000);
        assert_eq!(l.tracked_peers(), 0);
    }

    #[test]
    fn admission_sweeps_idle_buckets_without_being_asked() {
        // The defect this replaces: `prune_idle` existed, documented why
        // skipping it grows the map forever, and had no caller but its
        // own test.
        let mut l = IngressLimiter::new(60_000, 4, 1_200, 256, 0);
        l.admit(&peer(P1), 0).expect("ok");
        l.admit(&peer(P2), 0).expect("ok");
        assert_eq!(l.tracked_peers(), 2);

        // A minute later both have refilled to full. One of them sends
        // again; the sweep that admits it also drops the other.
        l.admit(&peer(P1), 60_000).expect("ok");
        assert_eq!(
            l.tracked_peers(),
            1,
            "the peer that went quiet is no longer tracked"
        );
    }

    #[test]
    fn the_sweep_keeps_a_bucket_that_is_still_spent() {
        // THE ASYMMETRY. A sweep that dropped everything would pass the
        // test above while destroying the accounting it exists to keep:
        // a peer mid-flood would get its allowance back every minute.
        let mut l = IngressLimiter::new(1, 4, 1_200, 256, 0);
        for _ in 0..4 {
            l.admit(&peer(P2), 0).expect("within the burst");
        }
        // P2 is empty and refills at 1/minute, so a minute later it has
        // one token back and is still short of its burst.
        l.admit(&peer(P1), 60_000).expect("ok");
        assert_eq!(
            l.tracked_peers(),
            2,
            "a peer that has not refilled keeps its bucket"
        );
    }

    #[test]
    fn no_sweep_runs_before_the_interval_elapses() {
        let mut l = IngressLimiter::new(60_000, 4, 1_200, 256, 0);
        l.admit(&peer(P1), 0).expect("ok");
        l.admit(&peer(P2), 0).expect("ok");
        // Full again after a second at this rate, but the interval has
        // not passed, so nothing is swept.
        l.admit(&peer(P1), 59_999).expect("ok");
        assert_eq!(l.tracked_peers(), 2);
    }

    #[test]
    fn publishing_requires_the_callers_own_join() {
        let mut r = SubscriptionRegistry::default();
        r.join(ch("general"), "a").expect("within the bounds");
        assert!(r.may_publish(&ch("general"), "a"));
        // Another client's join does not authorize this one's traffic.
        assert!(!r.may_publish(&ch("general"), "b"));
        assert!(!r.may_publish(&ch("other"), "a"));
    }

    #[test]
    fn a_warm_mesh_with_no_local_consumer_delivers_to_nobody() {
        let mut desired = BTreeSet::new();
        desired.insert(ch("general"));
        let r = SubscriptionRegistry::new(desired).expect("within the bounds");
        // The backend keeps the subscription...
        assert!(r.backend_should_subscribe(&ch("general")));
        // ...and there is no one to deliver to, and nothing is stored.
        assert!(r.subscribers(&ch("general")).is_empty());
        // A desired channel is still not a licence to publish.
        assert!(!r.may_publish(&ch("general"), "a"));
    }

    #[test]
    fn a_session_disconnect_drops_all_its_joins() {
        let mut r = SubscriptionRegistry::default();
        r.join(ch("general"), "a").expect("within the bounds");
        r.join(ch("builds"), "a").expect("within the bounds");
        r.join(ch("general"), "b").expect("within the bounds");
        r.release_session("a");
        assert_eq!(r.subscribers(&ch("general")), vec!["b".to_owned()]);
        assert!(r.subscribers(&ch("builds")).is_empty());
        // With nobody joined and nothing desired, the backend can leave.
        assert!(!r.backend_should_subscribe(&ch("builds")));
    }

    #[test]
    fn leaving_the_last_join_releases_the_channel() {
        let mut r = SubscriptionRegistry::default();
        r.join(ch("general"), "a").expect("within the bounds");
        r.leave(&ch("general"), "a");
        assert!(!r.backend_should_subscribe(&ch("general")));
        assert!(r.subscribers(&ch("general")).is_empty());
    }
}
