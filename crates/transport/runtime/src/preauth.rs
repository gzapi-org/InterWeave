// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Bounds on work done for a peer that has not authenticated yet.
//!
//! # The numbers here are not this crate's to choose
//!
//! `architecture/transport/libp2p/SECURITY.md` specifies the listener
//! policy: 64 pending handshakes globally and 8 per source bucket, a
//! 10-second handshake timeout, 30 starts per minute per bucket and 600
//! globally, with IPv4 bucketed by address and IPv6 by /64. Those are
//! the defaults below, and a limit that disagrees with that
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

use std::collections::{BTreeMap, BTreeSet};

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

/// Default time a handshake may take before it is reported for closing.
///
/// A handshake that never completes is indistinguishable from one that
/// is merely slow, and the difference does not matter: either way the
/// connection must be closed, or an attacker who opens connections and
/// then says nothing holds the budget for free.
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

/// The bucket an IP address is accounted under.
///
/// IPv4 buckets by address; IPv6 buckets by /64. That asymmetry is
/// `SECURITY.md`'s and it is not cosmetic: a residential IPv6 allocation
/// is routinely a /64 or larger, so keying on the full address hands one
/// party 2^64 buckets for free and makes per-source accounting mean
/// nothing at all. Every argument in this module about an attacker not
/// escaping the count depends on getting this right.
///
/// [`PreAuthGate::admit`] applies this to whatever it is given, so the rule
/// holds for every caller rather than for the ones who remembered. It is
/// public because a caller may want the same key for its own logging or
/// metrics, and because two different answers to "which bucket is this"
/// would be worse than none.
///
/// Accepts a socket address or a bare IP, since a listener has the
/// first and a policy layer usually has the second.
///
/// It does NOT parse a multiaddr: that grammar is libp2p's, and putting
/// it here would push a backend concept into a crate that has none
/// (CLAUDE.md §4). A backend holding `/ip4/198.51.100.7/tcp/4001` has
/// the socket address already and passes that.
///
/// Anything else — a relay's PeerId, a unix socket, a test label — is
/// its own bucket, unchanged. `SECURITY.md` requires a
/// relayed path with no original source IP to consume a
/// per-authenticated-relay bucket, which is exactly what passing the
/// relay's identity here produces: one abusive relay exhausts its own
/// bucket instead of minting a new one per circuit.
#[must_use]
pub fn source_bucket(source: &str) -> String {
    // A SOCKET ADDRESS FIRST, because that is what a listener has.
    // `TcpStream::peer_addr()` renders as `198.51.100.7:49152` or
    // `[2001:db8::1]:49152`, neither of which parses as an `IpAddr` — so
    // the fallback below would have kept the port in the key, and a
    // remote client would get a fresh bucket on every reconnect simply
    // by being assigned a new ephemeral port. The pending and rate
    // limits would then bound nothing at all.
    //
    // Which makes this the same defect as leaving the /64 rule to
    // callers, one layer further out: normalization that does not
    // recognise its real input is normalization that does not run.
    let ip = source
        .parse::<std::net::SocketAddr>()
        .map(|sock| sock.ip())
        .or_else(|_| source.parse::<std::net::IpAddr>());
    let Ok(ip) = ip else {
        return source.to_owned();
    };
    // A DUAL-STACK LISTENER REPORTS AN IPv4 CLIENT AS IPv6. Bind a
    // socket to `::` without `IPV6_V6ONLY` and `peer_addr()` renders an
    // IPv4 peer as `[::ffff:198.51.100.7]:49152`. That parses as
    // `IpAddr::V6`, and every IPv4-mapped address shares the `::/64`
    // prefix — so the /64 rule below would have collapsed the ENTIRE
    // IPv4 Internet into one bucket, and the first IPv4 client to spend
    // the per-source allowance would deny every other IPv4 client.
    //
    // The /64 rule is right for IPv6 for the reason stated below and
    // catastrophic here, so the mapping has to be undone before the
    // rule chooses. Only `::ffff:a.b.c.d` is unmapped: the deprecated
    // IPv4-COMPATIBLE form `::a.b.c.d` overlaps real addresses such as
    // `::1`, and `to_ipv4()` would turn the loopback into `0.0.0.1`.
    let ip = match ip {
        std::net::IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => std::net::IpAddr::V4(v4),
            None => std::net::IpAddr::V6(v6),
        },
        v4 => v4,
    };
    match ip {
        std::net::IpAddr::V4(v4) => v4.to_string(),
        std::net::IpAddr::V6(v6) => {
            let o = v6.octets();
            // The /64 prefix, with the interface identifier zeroed.
            let prefix = std::net::Ipv6Addr::from([
                o[0], o[1], o[2], o[3], o[4], o[5], o[6], o[7], 0, 0, 0, 0, 0, 0, 0, 0,
            ]);
            format!("{prefix}/64")
        }
    }
}

/// Largest value any configured ceiling in this module may take.
///
/// The gate holds one entry per tracked source, so `max_sources` is
/// memory an operator can ask for and an unauthenticated party then
/// fills. A stated ceiling makes the worst case a number someone chose
/// rather than whatever a typo produced.
pub const MAX_CONFIGURED_LIMIT: usize = 65_536;

/// Largest start-rate allowance the gate will accept, per window.
///
/// The two attempt ceilings are what bound CPU: every start admitted is
/// a Noise handshake this process performs for a party that has proved
/// nothing. Leaving them uncapped while capping the counts and the
/// durations was a bound with a hole in it -- `u32::MAX` starts inside
/// the one-hour maximum window is `u32::MAX` handshakes, so the gate
/// validated its configuration and then defended nothing.
///
/// The number is two orders of magnitude above the specified 600/minute
/// global policy: wide enough that a deployment which genuinely needs
/// to be louder can be, narrow enough that it is still a rate.
pub const MAX_CONFIGURED_ATTEMPTS: u32 = 60_000;

/// Longest rate window or handshake timeout the gate will accept, in
/// milliseconds -- one hour.
///
/// A window this long is already far outside the specified policy; the
/// bound exists so that `saturating_add` arithmetic on deadlines cannot
/// be handed a value that makes every deadline effectively infinite.
pub const MAX_CONFIGURED_DURATION_MS: u64 = 3_600_000;

/// What the gate enforces, after validation.
///
/// # Why the fields are private
///
/// A limit set out of range does not fail loudly; it changes what the
/// gate does while leaving it looking like a gate. The worst of them
/// fails OPEN: with `rate_window_ms` at zero, every window in
/// [`PreAuthGate::admit`] is elapsed on arrival, so both counters reset
/// before they are read and neither start-rate bound exists any more.
/// The listener would report the same limits, refuse nothing, and pass
/// every test that does not set the clock.
///
/// Making the fields public put that one edit away from any caller. The
/// only door is now [`PreAuthLimitsBuilder::build`], so a
/// `PreAuthLimits` value IS the proof that its numbers were checked --
/// there is no unvalidated one to pass to the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreAuthLimits {
    max_pending_total: usize,
    max_pending_per_source: usize,
    handshake_timeout_ms: u64,
    max_sources: usize,
    rate_window_ms: u64,
    max_attempts_per_window: u32,
    max_global_attempts_per_window: u32,
}

impl PreAuthLimits {
    /// Handshakes in flight across all sources.
    #[must_use]
    pub const fn max_pending_total(&self) -> usize {
        self.max_pending_total
    }

    /// Handshakes in flight from one source bucket.
    #[must_use]
    pub const fn max_pending_per_source(&self) -> usize {
        self.max_pending_per_source
    }

    /// How long a handshake may take before its slot is reclaimed.
    #[must_use]
    pub const fn handshake_timeout_ms(&self) -> u64 {
        self.handshake_timeout_ms
    }

    /// Source buckets tracked at once.
    #[must_use]
    pub const fn max_sources(&self) -> usize {
        self.max_sources
    }

    /// Length of the rate-accounting window.
    #[must_use]
    pub const fn rate_window_ms(&self) -> u64 {
        self.rate_window_ms
    }

    /// Starts one source bucket may make within a window.
    #[must_use]
    pub const fn max_attempts_per_window(&self) -> u32 {
        self.max_attempts_per_window
    }

    /// Starts all sources together may make within a window.
    #[must_use]
    pub const fn max_global_attempts_per_window(&self) -> u32 {
        self.max_global_attempts_per_window
    }
}

impl Default for PreAuthLimits {
    fn default() -> Self {
        // The specified policy, which is valid by construction. The test
        // `the_specified_defaults_are_valid` is what keeps that true if
        // a default is ever retuned.
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

/// Unvalidated limits, on their way to becoming [`PreAuthLimits`].
///
/// Public fields on purpose: this type is plainly the untrusted side of
/// the boundary, and [`Self::build`] is where it stops being untrusted.
/// [`Default`] is the specified policy, so `..Default::default()`
/// narrows one bound without restating the other six.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreAuthLimitsBuilder {
    /// Handshakes in flight across all sources.
    pub max_pending_total: usize,
    /// Handshakes in flight from one source bucket.
    pub max_pending_per_source: usize,
    /// How long a handshake may take before its slot is reclaimed.
    pub handshake_timeout_ms: u64,
    /// Source buckets tracked at once.
    pub max_sources: usize,
    /// Length of the rate-accounting window.
    pub rate_window_ms: u64,
    /// Starts one source bucket may make within a window.
    pub max_attempts_per_window: u32,
    /// Starts all sources together may make within a window.
    pub max_global_attempts_per_window: u32,
}

impl Default for PreAuthLimitsBuilder {
    fn default() -> Self {
        let d = PreAuthLimits::default();
        Self {
            max_pending_total: d.max_pending_total,
            max_pending_per_source: d.max_pending_per_source,
            handshake_timeout_ms: d.handshake_timeout_ms,
            max_sources: d.max_sources,
            rate_window_ms: d.rate_window_ms,
            max_attempts_per_window: d.max_attempts_per_window,
            max_global_attempts_per_window: d.max_global_attempts_per_window,
        }
    }
}

impl PreAuthLimitsBuilder {
    /// Check the numbers and produce limits the gate will accept.
    ///
    /// # Errors
    /// Returns the first [`InvalidPreAuthLimits`] that applies.
    pub const fn build(self) -> Result<PreAuthLimits, InvalidPreAuthLimits> {
        use InvalidPreAuthLimits as E;

        // EVERY BOUND IS POSITIVE. A zero here does not turn a bound
        // off in a way an operator would notice: it either refuses
        // everything or, for the two window fields, refuses nothing.
        if self.max_pending_total == 0 || self.max_pending_total > MAX_CONFIGURED_LIMIT {
            return Err(E::PendingTotalOutOfRange);
        }
        if self.max_pending_per_source == 0 || self.max_pending_per_source > MAX_CONFIGURED_LIMIT {
            return Err(E::PendingPerSourceOutOfRange);
        }
        if self.max_sources == 0 || self.max_sources > MAX_CONFIGURED_LIMIT {
            return Err(E::SourcesOutOfRange);
        }
        if self.handshake_timeout_ms == 0 || self.handshake_timeout_ms > MAX_CONFIGURED_DURATION_MS
        {
            return Err(E::HandshakeTimeoutOutOfRange);
        }
        // THE ONE THAT FAILS OPEN. See `PreAuthLimits`.
        if self.rate_window_ms == 0 || self.rate_window_ms > MAX_CONFIGURED_DURATION_MS {
            return Err(E::RateWindowOutOfRange);
        }
        // BOTH ENDS. Zero refuses every connection; the top end is the
        // one that fails open, because these two are the only bounds on
        // how many Noise handshakes an anonymous party can make this
        // process compute.
        if self.max_attempts_per_window == 0
            || self.max_attempts_per_window > MAX_CONFIGURED_ATTEMPTS
        {
            return Err(E::AttemptsPerWindowOutOfRange);
        }
        if self.max_global_attempts_per_window == 0
            || self.max_global_attempts_per_window > MAX_CONFIGURED_ATTEMPTS
        {
            return Err(E::GlobalAttemptsPerWindowOutOfRange);
        }

        // CROSS-FIELD. Per-source accounting exists so that one bucket
        // cannot spend the whole budget; a per-source ceiling at or
        // above the global one is per-source accounting that never
        // binds, which is the same outcome as not having it.
        if self.max_pending_per_source > self.max_pending_total {
            return Err(E::PerSourcePendingExceedsTotal);
        }
        if self.max_attempts_per_window > self.max_global_attempts_per_window {
            return Err(E::PerSourceAttemptsExceedGlobal);
        }

        Ok(PreAuthLimits {
            max_pending_total: self.max_pending_total,
            max_pending_per_source: self.max_pending_per_source,
            handshake_timeout_ms: self.handshake_timeout_ms,
            max_sources: self.max_sources,
            rate_window_ms: self.rate_window_ms,
            max_attempts_per_window: self.max_attempts_per_window,
            max_global_attempts_per_window: self.max_global_attempts_per_window,
        })
    }
}

/// Why a set of proposed limits was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidPreAuthLimits {
    /// `max_pending_total` is zero or above [`MAX_CONFIGURED_LIMIT`].
    PendingTotalOutOfRange,
    /// `max_pending_per_source` is zero or above [`MAX_CONFIGURED_LIMIT`].
    PendingPerSourceOutOfRange,
    /// `max_sources` is zero or above [`MAX_CONFIGURED_LIMIT`].
    SourcesOutOfRange,
    /// `handshake_timeout_ms` is zero or above [`MAX_CONFIGURED_DURATION_MS`].
    HandshakeTimeoutOutOfRange,
    /// `rate_window_ms` is zero or above [`MAX_CONFIGURED_DURATION_MS`].
    ///
    /// Zero is the fail-open case: every window is elapsed on arrival,
    /// so both start-rate counters reset before they are read.
    RateWindowOutOfRange,
    /// `max_attempts_per_window` is zero or above [`MAX_CONFIGURED_ATTEMPTS`].
    AttemptsPerWindowOutOfRange,
    /// `max_global_attempts_per_window` is zero or above
    /// [`MAX_CONFIGURED_ATTEMPTS`].
    GlobalAttemptsPerWindowOutOfRange,
    /// One bucket may hold at least the whole in-flight budget.
    PerSourcePendingExceedsTotal,
    /// One bucket may make at least every start in the window.
    PerSourceAttemptsExceedGlobal,
}

impl std::fmt::Display for InvalidPreAuthLimits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self {
            Self::PendingTotalOutOfRange => "max_pending_total is out of range",
            Self::PendingPerSourceOutOfRange => "max_pending_per_source is out of range",
            Self::SourcesOutOfRange => "max_sources is out of range",
            Self::HandshakeTimeoutOutOfRange => "handshake_timeout_ms is out of range",
            Self::RateWindowOutOfRange => "rate_window_ms is out of range",
            Self::AttemptsPerWindowOutOfRange => "max_attempts_per_window is out of range",
            Self::GlobalAttemptsPerWindowOutOfRange => {
                "max_global_attempts_per_window is out of range"
            }
            Self::PerSourcePendingExceedsTotal => {
                "max_pending_per_source exceeds max_pending_total"
            }
            Self::PerSourceAttemptsExceedGlobal => {
                "max_attempts_per_window exceeds max_global_attempts_per_window"
            }
        };
        f.write_str(what)
    }
}

impl std::error::Error for InvalidPreAuthLimits {}

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

    /// The bucket this handshake was accounted under.
    ///
    /// The BUCKET, not the address the caller passed: `admit` normalizes,
    /// so an IPv6 peer's slot reports the /64 it was counted against.
    /// Diagnostics only, and not an identity (see the module docs).
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }
}

#[derive(Debug, Default, Clone)]
struct SourceState {
    /// Handshake id -> start time.
    pending: BTreeMap<u64, u64>,
    /// Ids already handed to the runtime for closing.
    ///
    /// Bounded by `pending`, since an id leaves both together.
    reported: BTreeSet<u64>,
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
        // `pending` still holds handshakes reported for closing, which is
        // deliberate: the socket is open until the runtime says
        // otherwise, so the entry is carrying something either way.
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
    /// Handshakes past their deadline, waiting to be closed.
    expired: Vec<HandshakeSlot>,
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
            expired: Vec::new(),
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

        // NORMALIZED HERE, not by the caller. Publishing `source_bucket`
        // and asking every call site to remember it left the /64 rule as
        // advice, and advice is what a caller passing a raw transport
        // address silently ignores: `2001:db8::1` and `2001:db8::2` would
        // be two buckets, and the per-source limits this whole module is
        // built on would mean nothing.
        //
        // The gate owns the key it accounts on, so there is no call
        // sequence that produces unbucketed accounting. Idempotent, so a
        // caller that does bucket first loses nothing.
        let source = source_bucket(source);
        let source = source.as_str();

        if source.is_empty() || source.len() > MAX_SOURCE_BYTES {
            return Err(PreAuthDenial::SourceNotAccountable);
        }

        // BEFORE the caps are read, so a stalled peer's handshake is
        // reported for closing at the first moment anyone asks. It is NOT
        // freed here: see `expire` for why a slot outlives its deadline.
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
            state.reported.remove(&slot.id);
            self.pending_total = self.pending_total.saturating_sub(1);
        }
    }

    /// Report handshakes that have taken too long, so they can be closed.
    ///
    /// Returns how many were newly reported. The slots they hold are NOT
    /// released here, and that is the whole point.
    ///
    /// # Why a deadline does not free the slot
    ///
    /// The budget bounds sockets, not bookkeeping. Freeing a slot at the
    /// deadline lets the next admission reuse it while the timed-out
    /// connection is still open and still costing what it cost — so the
    /// real number of pre-Noise handshakes climbs past
    /// `max_pending_total` while the counter says otherwise, which is the
    /// bound reporting success for a thing it stopped measuring.
    ///
    /// So expiry hands the token to the runtime, the runtime closes the
    /// connection, and [`Self::completed`] releases the slot. A runtime
    /// that never drains keeps the gate full, and that is honest: it
    /// really does have that many sockets open.
    ///
    /// Each handshake is reported once. Calling this repeatedly does not
    /// hand out duplicates for the runtime to close twice.
    pub fn expire(&mut self, now_ms: u64) -> usize {
        let deadline = self.limits.handshake_timeout_ms;
        let mut reported = 0;
        for (source, state) in &mut self.sources {
            for (id, started) in &state.pending {
                if now_ms.saturating_sub(*started) >= deadline && state.reported.insert(*id) {
                    self.expired.push(HandshakeSlot {
                        source: source.clone(),
                        id: *id,
                        started_at_ms: *started,
                    });
                    reported += 1;
                }
            }
        }
        reported
    }

    /// Take the handshakes waiting to be closed.
    ///
    /// The runtime closes each and calls [`Self::completed`] with it.
    /// Draining without closing is the one misuse this type cannot
    /// detect, and it would reintroduce exactly the overshoot that
    /// keeping the slot prevents.
    #[must_use]
    pub fn take_expired(&mut self) -> Vec<HandshakeSlot> {
        core::mem::take(&mut self.expired)
    }

    /// Handshakes past their deadline that have not been taken yet.
    #[must_use]
    pub fn awaiting_close(&self) -> usize {
        self.expired.len()
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

    /// The narrowed policy the cases below run against.
    ///
    /// A builder rather than a literal so every test config goes
    /// through the same door a caller has to use. Private fields are
    /// still visible to this child module, so a struct literal would
    /// have compiled and quietly skipped the validation the rest of
    /// the world cannot skip.
    fn builder() -> PreAuthLimitsBuilder {
        PreAuthLimitsBuilder {
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

    fn limits() -> PreAuthLimits {
        builder()
            .build()
            .expect("the narrowed test policy is legal")
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
    fn the_deadline_is_where_it_is_said_to_be() {
        // The boundary, since an off-by-one here either reclaims a
        // handshake that is still negotiating or lets a dead one sit for
        // another whole cycle.
        let mut g = gate();
        let held = g.admit("10.0.0.1", 0).expect("admitted");

        assert_eq!(g.expire(999), 0, "still negotiating just before it");
        assert_eq!(g.awaiting_close(), 0);

        assert_eq!(g.expire(1_000), 1, "reported at the deadline");
        assert_eq!(g.pending(), 1, "but the socket is still open");

        for slot in g.take_expired() {
            g.completed(&slot);
        }
        assert_eq!(g.pending(), 0);

        // Completing a slot twice must not drive the count below what is
        // really open.
        g.completed(&held);
        assert_eq!(g.pending(), 0);
    }

    #[test]
    fn a_timed_out_handshake_is_reported_for_closing_before_its_slot_returns() {
        // The budget bounds SOCKETS, not bookkeeping. Freeing a slot at
        // the deadline lets the next admission reuse it while the
        // timed-out connection is still open and still costing what it
        // cost, so the real number of pre-Noise handshakes climbs past
        // the ceiling while the counter says otherwise.
        let mut g = gate();
        let mut held = Vec::new();
        for i in 0..3 {
            held.push(g.admit(&format!("10.0.0.{i}"), 0).expect("fill"));
            held.push(g.admit(&format!("10.0.0.{i}"), 0).expect("fill"));
        }
        assert_eq!(g.pending(), 6);
        assert_eq!(g.admit("10.0.0.1", 0), Err(PreAuthDenial::TooManyPending));

        // Past the deadline. The gate reports them and STAYS FULL: those
        // sockets are still open, and saying otherwise would be the
        // counter losing track of the thing it exists to count.
        let later = 1_000 + 1;
        assert_eq!(g.expire(later), 6, "all six are reported");
        assert_eq!(g.pending(), 6, "and none is freed by the deadline alone");
        assert_eq!(
            g.admit("10.0.0.9", later),
            Err(PreAuthDenial::TooManyPending),
            "a runtime that has not closed them really does hold six sockets"
        );

        // Reporting is once per handshake, so a runtime polling in a loop
        // is not handed the same connection to close repeatedly.
        assert_eq!(g.expire(later + 1), 0);
        assert_eq!(g.awaiting_close(), 6);

        // The runtime takes them, closes them, and says so. Only then
        // does the budget come back.
        let to_close = g.take_expired();
        assert_eq!(to_close.len(), 6);
        assert!(g.take_expired().is_empty(), "taking twice yields nothing");
        for slot in &to_close {
            g.completed(slot);
        }
        assert_eq!(g.pending(), 0);
        // An already-tracked bucket, so this is testing the pending
        // budget rather than the source table's own cap.
        g.admit("10.0.0.0", later).expect("the budget is back");
    }

    #[test]
    fn admitting_reports_stalled_handshakes_without_being_asked() {
        // The runtime should not have to poll to learn that something
        // needs closing: the moment anyone touches the gate, whatever has
        // passed its deadline is queued.
        let mut g = gate();
        let _held = g.admit("10.0.0.1", 0).expect("admitted");
        let _other = g.admit("10.0.0.2", 2_000).expect("admitted later");
        assert_eq!(
            g.awaiting_close(),
            1,
            "the first handshake was past its deadline when the second arrived"
        );
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

        // At 1000 the slow one is over its deadline and the fast one
        // would not have been. The right one is reported.
        assert_eq!(g.expire(1_000), 1);
        let reported = g.take_expired();
        assert_eq!(reported.len(), 1);
        assert_eq!(reported[0].started_at_ms(), slow.started_at_ms());
        g.completed(&reported[0]);
        assert_eq!(g.pending(), 0);
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
        let mut g = PreAuthGate::new(
            PreAuthLimitsBuilder {
                handshake_timeout_ms: 60_000,
                rate_window_ms: 1_000,
                ..builder()
            }
            .build()
            .expect("a legal narrowing"),
        );
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
        let mut g = PreAuthGate::new(
            PreAuthLimitsBuilder {
                max_global_attempts_per_window: 10,
                max_sources: 64,
                ..builder()
            }
            .build()
            .expect("a legal narrowing"),
        );

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
        let mut g = PreAuthGate::new(
            PreAuthLimitsBuilder {
                max_global_attempts_per_window: 10,
                max_attempts_per_window: 1,
                // Room for the nine other buckets below, so the source table
                // is not what refuses them.
                max_sources: 64,
                ..builder()
            }
            .build()
            .expect("a legal narrowing"),
        );

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
    fn ipv6_is_bucketed_by_prefix_so_an_address_range_is_not_free() {
        // A residential IPv6 allocation is routinely a /64 or larger.
        // Keying on the full address hands one party 2^64 buckets, and
        // every argument in this module about not escaping the count
        // depends on this not happening.
        let a = source_bucket("2001:db8:1:2::1");
        let b = source_bucket("2001:db8:1:2:ffff:ffff:ffff:ffff");
        assert_eq!(a, b, "one /64 is one bucket");

        let elsewhere = source_bucket("2001:db8:1:3::1");
        assert_ne!(a, elsewhere, "a different /64 is a different bucket");

        // IPv4 keys on the address itself.
        assert_eq!(source_bucket("198.51.100.7"), "198.51.100.7");
        assert_ne!(source_bucket("198.51.100.7"), source_bucket("198.51.100.8"));

        // Anything that is not an IP is its own bucket, unchanged — which
        // is what makes a relay's identity a per-relay bucket rather than
        // one per circuit.
        let relay = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
        assert_eq!(source_bucket(relay), relay);

        // And the GATE applies it, given raw addresses — which is the
        // only version of this property that matters. Publishing the
        // helper and asking callers to remember it left the rule as
        // advice, and a caller passing a transport address straight
        // through would have had one bucket per host.
        let mut g = gate();
        g.admit("2001:db8:1:2::1", 0).expect("first");
        g.admit("2001:db8:1:2::2", 0)
            .expect("same /64, second slot");
        assert_eq!(
            g.admit("2001:db8:1:2::3", 0),
            Err(PreAuthDenial::TooManyFromSource),
            "a /64 cannot buy more slots by changing the low bits"
        );
        assert_eq!(g.tracked_sources(), 1, "one /64 is one tracked source");

        // A different /64 is genuinely separate.
        g.admit("2001:db8:1:3::1", 0).expect("another prefix");
        assert_eq!(g.tracked_sources(), 2);

        // The slot reports the bucket it was counted against, so a
        // diagnostic cannot suggest accounting that did not happen.
        let mut g = gate();
        let slot = g.admit("2001:db8:1:2::abcd", 0).expect("admitted");
        assert_eq!(slot.source(), "2001:db8:1:2::/64");

        // Bucketing twice changes nothing, so a caller that already
        // applied the helper is not penalised.
        let mut g = gate();
        g.admit(&source_bucket("2001:db8:1:2::1"), 0)
            .expect("first");
        g.admit("2001:db8:1:2::2", 0)
            .expect("still the same bucket");
        assert_eq!(g.tracked_sources(), 1);
    }

    #[test]
    fn an_ephemeral_port_does_not_buy_a_new_bucket() {
        // What a listener actually has is `TcpStream::peer_addr()`, which
        // renders as `198.51.100.7:49152` — and an `IpAddr` parse rejects
        // that, so the port would have stayed in the key. A client gets a
        // new ephemeral port on every reconnect, so every reconnect would
        // have been a fresh bucket and the per-source limits would have
        // bounded nothing.
        assert_eq!(source_bucket("198.51.100.7:49152"), "198.51.100.7");
        assert_eq!(source_bucket("198.51.100.7:1"), "198.51.100.7");
        assert_eq!(
            source_bucket("[2001:db8:1:2::1]:49152"),
            "2001:db8:1:2::/64"
        );

        // Bare addresses still work: a policy layer usually has one of
        // those rather than a socket.
        assert_eq!(source_bucket("198.51.100.7"), "198.51.100.7");
        assert_eq!(source_bucket("2001:db8:1:2::1"), "2001:db8:1:2::/64");

        // And the gate is what applies it, given exactly what a listener
        // would hand over.
        let mut g = gate();
        g.admit("198.51.100.7:49152", 0).expect("first connection");
        g.admit("198.51.100.7:51000", 0).expect("second, new port");
        assert_eq!(
            g.admit("198.51.100.7:53000", 0),
            Err(PreAuthDenial::TooManyFromSource),
            "reconnecting from a new port must not reset the per-source count"
        );
        assert_eq!(g.tracked_sources(), 1, "one host is one bucket");

        // The v6 case combines both normalizations: drop the port, then
        // the interface identifier.
        let mut g = gate();
        g.admit("[2001:db8:1:2::1]:40000", 0).expect("first");
        g.admit("[2001:db8:1:2::99]:41000", 0).expect("same /64");
        assert_eq!(
            g.admit("[2001:db8:1:2::ff]:42000", 0),
            Err(PreAuthDenial::TooManyFromSource)
        );
        assert_eq!(g.tracked_sources(), 1);
    }

    #[test]
    fn a_zero_rate_window_would_disable_both_start_bounds() {
        // FIRST, PROVE THE DANGER IS REAL. This child module can still
        // reach the private fields, so it can build the value the
        // outside world no longer can -- and show what the gate does
        // with it. Without this half, the assertion below is a claim
        // about a constructor rather than about a security bound.
        let broken = PreAuthLimits {
            rate_window_ms: 0,
            ..limits()
        };
        let mut g = PreAuthGate::new(broken);
        // Four starts is the per-source ceiling in `limits()`. Complete
        // each one so the pending count is never what refuses, leaving
        // the start rate as the only bound in play.
        for _ in 0..40 {
            let slot = g
                .admit("10.0.0.1", 0)
                .expect("with a zero window nothing is ever rate limited");
            g.completed(&slot);
        }
        assert!(
            g.admit("10.0.0.1", 0).is_ok(),
            "ten times the per-window allowance at one instant, refused never"
        );

        // The same traffic against a legal window IS refused, so the
        // line above is about the zero and not about the loop.
        let mut sane = PreAuthGate::new(limits());
        for _ in 0..4 {
            let slot = sane.admit("10.0.0.1", 0).expect("within the window");
            sane.completed(&slot);
        }
        assert_eq!(sane.admit("10.0.0.1", 0), Err(PreAuthDenial::RateLimited));

        // SECOND, PROVE THE DOOR IS SHUT. Every window in `admit` is
        // elapsed on arrival at zero, so both counters reset before
        // they are read: the bound is not merely loose, it is absent,
        // and it is the one misconfiguration here that fails OPEN.
        assert_eq!(
            PreAuthLimitsBuilder {
                rate_window_ms: 0,
                ..builder()
            }
            .build(),
            Err(InvalidPreAuthLimits::RateWindowOutOfRange)
        );
    }

    #[test]
    fn the_specified_defaults_are_valid() {
        // `Default` bypasses the builder, so retuning a default could
        // otherwise mint a `PreAuthLimits` the builder would refuse --
        // an invalid value of a type whose whole claim is that there
        // are none.
        let d = PreAuthLimits::default();
        assert_eq!(
            PreAuthLimitsBuilder::default().build(),
            Ok(d),
            "the specified policy must survive its own validator"
        );
    }

    #[test]
    fn every_bound_is_positive_and_ceilinged() {
        use InvalidPreAuthLimits as E;

        // Zero, one field at a time. Each of these either refuses
        // every connection or, for the two durations, stops refusing
        // anything -- and both look like a working gate from outside.
        let zeroed: [(PreAuthLimitsBuilder, E); 7] = [
            (
                PreAuthLimitsBuilder {
                    max_pending_total: 0,
                    ..builder()
                },
                E::PendingTotalOutOfRange,
            ),
            (
                PreAuthLimitsBuilder {
                    max_pending_per_source: 0,
                    ..builder()
                },
                E::PendingPerSourceOutOfRange,
            ),
            (
                PreAuthLimitsBuilder {
                    max_sources: 0,
                    ..builder()
                },
                E::SourcesOutOfRange,
            ),
            (
                PreAuthLimitsBuilder {
                    handshake_timeout_ms: 0,
                    ..builder()
                },
                E::HandshakeTimeoutOutOfRange,
            ),
            (
                PreAuthLimitsBuilder {
                    rate_window_ms: 0,
                    ..builder()
                },
                E::RateWindowOutOfRange,
            ),
            (
                PreAuthLimitsBuilder {
                    max_attempts_per_window: 0,
                    ..builder()
                },
                E::AttemptsPerWindowOutOfRange,
            ),
            (
                PreAuthLimitsBuilder {
                    max_global_attempts_per_window: 0,
                    ..builder()
                },
                E::GlobalAttemptsPerWindowOutOfRange,
            ),
        ];
        for (b, want) in zeroed {
            assert_eq!(b.build(), Err(want), "a zero must not build");
        }

        // THE TOP END OF THE ATTEMPT CEILINGS, which is the one that
        // fails open. Capping the counts and the durations while
        // leaving these two unbounded was a validated configuration
        // that defended nothing: `u32::MAX` starts inside the one-hour
        // maximum window is `u32::MAX` Noise handshakes this process
        // performs for parties that have proved nothing, and CPU is the
        // resource this layer exists to bound.
        for over in [
            PreAuthLimitsBuilder {
                max_attempts_per_window: MAX_CONFIGURED_ATTEMPTS + 1,
                max_global_attempts_per_window: u32::MAX,
                ..builder()
            },
            PreAuthLimitsBuilder {
                max_global_attempts_per_window: u32::MAX,
                ..builder()
            },
        ] {
            assert!(
                over.build().is_err(),
                "an unbounded start rate is not a start rate"
            );
        }
        assert!(
            PreAuthLimitsBuilder {
                max_attempts_per_window: MAX_CONFIGURED_ATTEMPTS,
                max_global_attempts_per_window: MAX_CONFIGURED_ATTEMPTS,
                ..builder()
            }
            .build()
            .is_ok(),
            "the ceiling itself is legal"
        );

        // And the top end, because `max_sources` is memory an operator
        // asks for and an unauthenticated party then fills.
        assert_eq!(
            PreAuthLimitsBuilder {
                max_sources: MAX_CONFIGURED_LIMIT + 1,
                ..builder()
            }
            .build(),
            Err(E::SourcesOutOfRange)
        );
        assert!(
            PreAuthLimitsBuilder {
                max_sources: MAX_CONFIGURED_LIMIT,
                ..builder()
            }
            .build()
            .is_ok(),
            "the ceiling itself is legal"
        );
        assert_eq!(
            PreAuthLimitsBuilder {
                rate_window_ms: MAX_CONFIGURED_DURATION_MS + 1,
                ..builder()
            }
            .build(),
            Err(E::RateWindowOutOfRange)
        );
    }

    #[test]
    fn a_per_source_bound_at_the_global_one_is_not_a_per_source_bound() {
        use InvalidPreAuthLimits as E;

        // The point of per-source accounting is that one bucket cannot
        // spend the whole budget. A per-source ceiling above the global
        // one never binds before the global one does, so the gate keeps
        // its shape and loses the property.
        assert_eq!(
            PreAuthLimitsBuilder {
                max_pending_total: 8,
                max_pending_per_source: 9,
                ..builder()
            }
            .build(),
            Err(E::PerSourcePendingExceedsTotal)
        );
        assert_eq!(
            PreAuthLimitsBuilder {
                max_attempts_per_window: 100,
                max_global_attempts_per_window: 99,
                ..builder()
            }
            .build(),
            Err(E::PerSourceAttemptsExceedGlobal)
        );

        // Equal is allowed: one bucket may reach the global ceiling,
        // which is a narrow policy rather than an absent one.
        assert!(
            PreAuthLimitsBuilder {
                max_pending_total: 8,
                max_pending_per_source: 8,
                ..builder()
            }
            .build()
            .is_ok()
        );
    }

    #[test]
    fn a_dual_stack_listener_does_not_merge_the_ipv4_internet() {
        // The third round of the same mistake, and the widest: a socket
        // bound to `::` without `IPV6_V6ONLY` accepts IPv4 too, and
        // reports those peers as `[::ffff:198.51.100.7]:49152`. That is
        // an `IpAddr::V6`, and EVERY IPv4-mapped address sits in `::/64`
        // — so the prefix rule that is correct for native IPv6 put the
        // whole IPv4 Internet in one bucket. One client spending the
        // per-source allowance denied every other IPv4 client, which is
        // a worse outcome than having no per-source accounting at all.
        assert_eq!(source_bucket("[::ffff:198.51.100.7]:49152"), "198.51.100.7");
        assert_eq!(source_bucket("[::ffff:198.51.100.8]:49152"), "198.51.100.8");
        assert_eq!(source_bucket("::ffff:198.51.100.7"), "198.51.100.7");

        // The mapped form and the plain form are the SAME client, so
        // they must not be two budgets either.
        assert_eq!(
            source_bucket("[::ffff:198.51.100.7]:49152"),
            source_bucket("198.51.100.7:49152")
        );

        // Only the mapped form is unwrapped. `::1` is IPv4-COMPATIBLE
        // shaped, and unwrapping that class would rewrite the loopback
        // as `0.0.0.1` — a native IPv6 address must keep the /64 rule.
        assert_eq!(source_bucket("::1"), "::/64");
        assert_eq!(source_bucket("2001:db8:1:2::1"), "2001:db8:1:2::/64");

        // And the gate is what has to apply it. Two unrelated IPv4
        // clients, arriving the way a dual-stack listener reports them:
        // each gets its own allowance, neither can deny the other.
        let mut g = gate();
        g.admit("[::ffff:198.51.100.7]:49152", 0)
            .expect("client a, first");
        g.admit("[::ffff:198.51.100.7]:49153", 0)
            .expect("client a, second");
        assert_eq!(
            g.admit("[::ffff:198.51.100.7]:49154", 0),
            Err(PreAuthDenial::TooManyFromSource),
            "one mapped client is still capped"
        );
        g.admit("[::ffff:198.51.100.8]:49152", 0)
            .expect("a DIFFERENT IPv4 client is not denied by the first one");
        assert_eq!(
            g.tracked_sources(),
            2,
            "two IPv4 clients are two buckets, not one"
        );
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
