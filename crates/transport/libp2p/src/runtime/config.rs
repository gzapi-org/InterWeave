// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! How the substrate is built, and what can go wrong building it.
//!
//! Split out of `runtime.rs` unchanged. Every capacity here is a
//! CEILING rather than a tuning value: without one, a configuration can
//! ask for an allocation large enough to be the denial of service it was
//! meant to prevent, and the request looks like ordinary tuning.

use std::time::{Duration, Instant};

use interweave_transport_runtime::preauth::PreAuthLimits;

/// Default depth of the command channel.
pub const DEFAULT_COMMAND_CAPACITY: usize = 64;

/// Default depth of the event channel.
///
/// Deeper than commands because events arrive from the network and
/// commands come from this process. It is still bounded: a burst of
/// remote activity must cost the Swarm task backpressure, never
/// unbounded local memory.
pub const DEFAULT_EVENT_CAPACITY: usize = 256;

/// Upper bound on every channel depth and table size below.
///
/// Not a tuning value — a ceiling. Without one a configuration can ask
/// for an allocation large enough to be the denial of service it was
/// meant to prevent, and the request looks like ordinary tuning.
pub const MAX_CONFIGURED_CAPACITY: usize = 65_536;

/// How the substrate is built.
#[derive(Debug, Clone, Copy)]
pub struct SubstrateConfig {
    /// Depth of the command channel.
    pub command_capacity: usize,
    /// Depth of the event channel.
    pub event_capacity: usize,
    /// Maximum concurrent pending dials.
    pub max_pending_dials: usize,
    /// Maximum established connections.
    pub max_connections: usize,
    /// Idle connection timeout.
    pub idle_timeout: Duration,
    /// The profile's EFFECTIVE direct payload limit, in bytes.
    ///
    /// May narrow the frozen ceiling and never widen it: `validate`
    /// refuses zero and anything above [`MAX_PAYLOAD_BYTES`]. Decoding
    /// every frame against the architecture maximum accepted payloads a
    /// profile had already refused, in both directions.
    pub max_payload_bytes: usize,
    /// Bounds on work done for a peer that has not authenticated.
    ///
    /// A `PreAuthLimits` value is proof its numbers were checked --
    /// `PreAuthLimitsBuilder::build` is the only way to make one -- so
    /// there is nothing for `validate` to re-check here.
    pub preauth: PreAuthLimits,
    /// How often the reconnect scheduler looks for due retries.
    ///
    /// A period, not a deadline: a retry becomes due when the policy
    /// says so, and this is how long it may wait to be noticed. Short
    /// enough that the backoff is what determines the delay, long
    /// enough that an idle profile is not walking a table every
    /// moment.
    pub retry_tick: Duration,
    /// Most listeners that may be bound at once.
    ///
    /// `max_pending_listens` bounds only listeners still AWAITING an
    /// address; a resolved one left that table and was counted by
    /// nothing, so any number could accumulate. This bounds the ones
    /// actually holding a socket.
    pub max_active_listeners: usize,
    /// Most peers the scheduler will dial in one tick.
    ///
    /// The retry table is bounded, so the whole of it could come due at
    /// once -- and dialing all of it in one pass is a burst this
    /// profile inflicts on itself. The rest stay due and are taken next
    /// tick.
    pub max_retries_per_tick: usize,
    /// Maximum listeners with a caller still awaiting their address.
    ///
    /// The command channel bounds how many `Listen` commands can be
    /// QUEUED, not how many can be accepted: the task drains commands
    /// continuously, so pending replies and OS listeners accumulate past
    /// any instantaneous queue depth. This is the bound on the table
    /// itself.
    pub max_pending_listens: usize,
    /// How long a remote directory result stays fresh, in milliseconds.
    ///
    /// The LOCAL term of `min(remote, local, 300000)`. Zero is legal and
    /// means "never cache": every query crosses the wire. `validate`
    /// refuses anything above the five-minute ceiling rather than
    /// clamping it, so a caller learns its configuration was wrong.
    pub directory_cache_ttl_ms: u32,
    /// Most remote peers whose directory is cached at once.
    pub directory_cache_peers: usize,
}

impl Default for SubstrateConfig {
    fn default() -> Self {
        Self {
            command_capacity: DEFAULT_COMMAND_CAPACITY,
            event_capacity: DEFAULT_EVENT_CAPACITY,
            max_payload_bytes: interweave_transport_api::MAX_PAYLOAD_BYTES,
            max_pending_dials: 32,
            max_connections: 256,
            idle_timeout: Duration::from_secs(60),
            preauth: PreAuthLimits::default(),
            retry_tick: Duration::from_secs(1),
            max_active_listeners: 64,
            max_retries_per_tick: 4,
            max_pending_listens: 64,
            directory_cache_ttl_ms: interweave_transport_runtime::directory::DEFAULT_CACHE_TTL_MS,
            directory_cache_peers: interweave_transport_runtime::directory::DEFAULT_CACHE_PEERS,
        }
    }
}

/// Head-room between validating a period and rescheduling on it.
///
/// `validate` runs before the runtime is built, so a period that is
/// representable at this instant and not at the next would pass here and
/// abort there.
const CLOCK_MARGIN: Duration = Duration::from_secs(86_400);

/// The largest whole-millisecond period this machine's clock can carry.
///
/// Computed rather than written down: the boundary is a property of
/// `Instant` on the host, not a number this project gets to choose, and
/// the last one written down here was an allocation ceiling that rejected
/// a five-minute heartbeat. Sixty-four `checked_add` calls, once, at
/// startup.
fn max_representable_tick_ms() -> usize {
    let addable = |ms: u64| {
        Instant::now()
            .checked_add(Duration::from_millis(ms))
            .and_then(|t| t.checked_add(CLOCK_MARGIN))
            .is_some()
    };
    let (mut lo, mut hi) = (0_u64, u64::MAX);
    while lo < hi {
        // Rounds up, so `lo` only ever moves to a value known addable.
        let mid = lo + (hi - lo) / 2 + (hi - lo) % 2;
        if addable(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    usize::try_from(lo).unwrap_or(usize::MAX)
}

impl SubstrateConfig {
    /// Check every limit before anything is built.
    ///
    /// # Errors
    /// Returns [`SubstrateError::InvalidConfig`] naming the first field
    /// outside `1..=`[`MAX_CONFIGURED_CAPACITY`].
    pub fn validate(&self) -> Result<(), SubstrateError> {
        // CHANNEL DEPTHS need at least one slot: `mpsc::channel(0)`
        // panics, so zero here is not a strict policy but an abort.
        let depths = [
            ("command_capacity", self.command_capacity, 1),
            ("event_capacity", self.event_capacity, 1),
            // CAPS may be zero, and zero is not a mistake: a policy
            // admitting no dial, holding no connection, or accepting no
            // listen is a coherent thing to configure and is how the
            // refusal paths are exercised. Rejecting it would turn a
            // panic guard into a policy opinion.
            ("max_pending_dials", self.max_pending_dials, 0),
            ("max_connections", self.max_connections, 0),
            ("max_pending_listens", self.max_pending_listens, 0),
            ("max_active_listeners", self.max_active_listeners, 0),
            // A tick that dialed the whole table would be a burst; zero
            // is a scheduler that never dials, which is the state this
            // stage exists to leave.
            ("max_retries_per_tick", self.max_retries_per_tick, 1),
            // A cache of zero peers is a map that cannot hold its first
            // entry; `DirectoryCache::new` would clamp it to one, and a
            // configuration silently corrected is the shape this
            // function exists to refuse.
            ("directory_cache_peers", self.directory_cache_peers, 1),
        ];
        for (field, got, min) in depths {
            if got < min || got > MAX_CONFIGURED_CAPACITY {
                return Err(SubstrateError::InvalidConfig {
                    field,
                    got,
                    allowed: (min, MAX_CONFIGURED_CAPACITY),
                });
            }
        }
        // THE PAYLOAD LIMIT IS NOT A CHANNEL DEPTH, so it is checked
        // against its own ceiling rather than added to the table above.
        // Zero is refused because a profile that admits no payload
        // admits no direct message at all — the same reasoning that
        // refuses a zero-length event queue — and above the frozen
        // ceiling is refused rather than clamped, so a caller learns its
        // configuration was wrong instead of quietly getting another.
        if self.max_payload_bytes == 0
            || self.max_payload_bytes > interweave_transport_api::MAX_PAYLOAD_BYTES
        {
            return Err(SubstrateError::InvalidConfig {
                field: "max_payload_bytes",
                got: self.max_payload_bytes,
                allowed: (1, interweave_transport_api::MAX_PAYLOAD_BYTES),
            });
        }
        // THE DIRECTORY TTL IS A u32 with its own ceiling. Zero means
        // "never cache" and is legal; above five minutes is refused, not
        // clamped, for the reason the payload limit is.
        if self.directory_cache_ttl_ms > interweave_transport_api::MAX_DIRECTORY_TTL_MS {
            return Err(SubstrateError::InvalidConfig {
                field: "directory_cache_ttl_ms",
                got: self.directory_cache_ttl_ms as usize,
                allowed: (0, interweave_transport_api::MAX_DIRECTORY_TTL_MS as usize),
            });
        }
        // THE HEARTBEAT IS A DURATION, which is why it was not in the
        // table above and why it went unchecked: every entry there is a
        // `usize`. `tokio::time::interval` panics on a zero period, so a
        // `retry_tick` of `Duration::ZERO` aborted the daemon from a
        // public configuration field — the same abort the channel depths
        // are checked to prevent, one type away from the loop that
        // prevents it.
        //
        // Measured in whole milliseconds, so a sub-millisecond tick is
        // refused rather than truncated to the zero this rejects. A
        // heartbeat of a few microseconds is a busy loop that starves the
        // select loop it shares a task with — silently, which is worse
        // than the panic above.
        //
        // THE UPPER BOUND IS THE CLOCK, not a capacity. An earlier
        // version compared this against `MAX_CONFIGURED_CAPACITY`, which
        // is 65,536 — a ceiling on ALLOCATION SIZES, reused as though it
        // were a duration, and it rejected an ordinary five-minute
        // heartbeat. Removing it went too far the other way: there IS a
        // real ceiling, and it is the one `tokio::time::interval` can
        // represent.
        //
        // `Interval` reschedules by `timeout.checked_add(period)` while
        // it is on time, which saturates. But when a poll arrives more
        // than 5ms after the deadline it takes the missed-tick path
        // instead, and the `Delay` behaviour this runtime selects
        // computes `now + period` with plain `Instant` arithmetic, which
        // PANICS on overflow. The first tick is due immediately, so the
        // very first poll takes that path whenever the task spent 5ms
        // getting there — which building a Swarm does. `Duration::MAX`
        // aborts the daemon; `Duration::from_secs(1 << 40)` does not.
        // `checked_add` is exactly that boundary rather than a guess at
        // it.
        let tick_ms = usize::try_from(self.retry_tick.as_millis()).unwrap_or(usize::MAX);
        // The margin is because THIS runs before the interval is built:
        // a period that is representable now and not a moment later
        // would pass here and abort there.
        let representable = Instant::now()
            .checked_add(self.retry_tick)
            .and_then(|t| t.checked_add(CLOCK_MARGIN))
            .is_some();
        if tick_ms == 0 || !representable {
            return Err(SubstrateError::InvalidConfig {
                field: "retry_tick_ms",
                got: tick_ms,
                allowed: (1, max_representable_tick_ms()),
            });
        }
        Ok(())
    }
}

/// What can go wrong building or driving the substrate.
#[derive(Debug)]
pub enum SubstrateError {
    /// The transport could not be constructed.
    Transport(String),
    /// The Swarm task is gone.
    ///
    /// Every command path returns this rather than panicking: the task
    /// ending is a normal outcome of shutdown, and a caller racing it
    /// should get an error, not an abort.
    Stopped,
    /// A stored or observed PeerId is not one the neutral contract accepts.
    Identity(String),
    /// A [`SubstrateConfig`] value outside its permitted range.
    ///
    /// Returned rather than panicked. `mpsc::channel(0)` aborts the
    /// process, and this is a transport daemon whose lint policy treats a
    /// reachable panic as a defect — a configuration mistake must not be
    /// the thing that takes it down.
    /// A profile configuration the canonical validator refused.
    ///
    /// Carries every broken rule rather than the first: an operator
    /// fixing sixty endpoints should not find their mistakes one run
    /// apart.
    InvalidProfile(Vec<String>),
    /// A [`SubstrateConfig`] value outside its permitted range.
    InvalidConfig {
        /// Which field.
        field: &'static str,
        /// The value supplied.
        got: usize,
        /// The permitted range, inclusive.
        allowed: (usize, usize),
    },
}

impl core::fmt::Display for SubstrateError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Transport(d) => write!(f, "transport: {d}"),
            Self::Stopped => write!(f, "the swarm task has stopped"),
            Self::Identity(d) => write!(f, "identity: {d}"),
            Self::InvalidProfile(broken) => {
                write!(
                    f,
                    "the profile configuration is invalid: {}",
                    broken.join("; ")
                )
            }
            Self::InvalidConfig {
                field,
                got,
                allowed: (min, max),
            } => write!(f, "{field} is {got}; it must be {min}..={max}"),
        }
    }
}

impl core::error::Error for SubstrateError {}
