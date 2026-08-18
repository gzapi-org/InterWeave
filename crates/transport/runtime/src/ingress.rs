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
}

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

/// Local join references for broadcast channels.
///
/// A **client** join, which is not the same as the daemon's topic
/// subscription. A profile may keep a mesh warm with `channels.desired`
/// while zero clients hold a join; inbound messages then have no local
/// consumer and are dropped rather than buffered, which is the no-buffer
/// rule holding at this layer too.
#[derive(Debug, Default)]
pub struct SubscriptionRegistry {
    joins: BTreeMap<ChannelId, BTreeSet<String>>,
    desired: BTreeSet<ChannelId>,
}

impl SubscriptionRegistry {
    /// Build a registry with the profile's warm-mesh channels.
    #[must_use]
    pub fn new(desired: BTreeSet<ChannelId>) -> Self {
        Self {
            joins: BTreeMap::new(),
            desired,
        }
    }

    /// Record that a session joined a channel.
    pub fn join(&mut self, channel: ChannelId, session: impl Into<String>) {
        self.joins
            .entry(channel)
            .or_default()
            .insert(session.into());
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
    fn publishing_requires_the_callers_own_join() {
        let mut r = SubscriptionRegistry::default();
        r.join(ch("general"), "a");
        assert!(r.may_publish(&ch("general"), "a"));
        // Another client's join does not authorize this one's traffic.
        assert!(!r.may_publish(&ch("general"), "b"));
        assert!(!r.may_publish(&ch("other"), "a"));
    }

    #[test]
    fn a_warm_mesh_with_no_local_consumer_delivers_to_nobody() {
        let mut desired = BTreeSet::new();
        desired.insert(ch("general"));
        let r = SubscriptionRegistry::new(desired);
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
        r.join(ch("general"), "a");
        r.join(ch("builds"), "a");
        r.join(ch("general"), "b");
        r.release_session("a");
        assert_eq!(r.subscribers(&ch("general")), vec!["b".to_owned()]);
        assert!(r.subscribers(&ch("builds")).is_empty());
        // With nobody joined and nothing desired, the backend can leave.
        assert!(!r.backend_should_subscribe(&ch("builds")));
    }

    #[test]
    fn leaving_the_last_join_releases_the_channel() {
        let mut r = SubscriptionRegistry::default();
        r.join(ch("general"), "a");
        r.leave(&ch("general"), "a");
        assert!(!r.backend_should_subscribe(&ch("general")));
        assert!(r.subscribers(&ch("general")).is_empty());
    }
}
