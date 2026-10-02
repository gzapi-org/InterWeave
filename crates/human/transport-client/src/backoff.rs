// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Retry schedules: exponential from a base, capped, with jitter that is
//! a pure function of a key and the attempt number -- no random source
//! in the loop, so a test replays a schedule exactly and two rows
//! failing together do not retry in lockstep.

/// One schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Backoff {
    base_ms: u64,
    cap_ms: u64,
}

/// A failed send's retry (agreed item 1c: 1 s, doubling, 5 min cap;
/// unbounded in count).
pub(crate) const SEND: Backoff = Backoff::new(1_000, 300_000);
/// Re-opening a session whose binding ended.
pub(crate) const REOPEN: Backoff = Backoff::new(500, 30_000);
/// Re-checking a degraded store (agreed item 4c: 5 s doubling to 5 min).
pub(crate) const RECHECK: Backoff = Backoff::new(5_000, 300_000);

impl Backoff {
    pub(crate) const fn new(base_ms: u64, cap_ms: u64) -> Self {
        Self { base_ms, cap_ms }
    }

    /// The delay before attempt `attempt + 1`, after `attempt` failures
    /// (`attempt` 1 is the first retry). In `[ceiling / 2, ceiling]`,
    /// where `ceiling` is `base * 2^(attempt - 1)` capped at `cap`: never
    /// over the cap, and never under half of it once the cap is reached.
    pub(crate) const fn delay(self, attempt: u32, key: u64) -> u64 {
        let shift = if attempt == 0 { 0 } else { attempt - 1 };
        let ceiling = if shift >= 63 {
            self.cap_ms
        } else {
            let raw = self.base_ms.saturating_mul(1 << shift);
            if raw > self.cap_ms { self.cap_ms } else { raw }
        };
        let half = ceiling / 2;
        let span = ceiling - half;
        half + mix(key ^ (attempt as u64).rotate_left(32)) % (span + 1)
    }
}

/// `splitmix64`'s finaliser: a fixed, well-spread bijection on `u64`.
const fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_delay_doubles_and_never_passes_the_cap() {
        for key in [0, 1, 42, u64::MAX] {
            let mut last_ceiling = 0;
            for attempt in 1..200 {
                let d = SEND.delay(attempt, key);
                assert!(d <= 300_000, "attempt {attempt}: {d}");
                let ceiling = (1_000u64 << (attempt - 1).min(20)).min(300_000);
                assert!(d >= ceiling / 2 && d <= ceiling, "attempt {attempt}: {d}");
                assert!(ceiling >= last_ceiling);
                last_ceiling = ceiling;
            }
        }
    }

    #[test]
    fn the_first_retry_is_within_the_base() {
        assert!((500..=1_000).contains(&SEND.delay(1, 7)));
        assert!((2_500..=5_000).contains(&RECHECK.delay(1, 7)));
    }

    #[test]
    fn the_same_key_and_attempt_give_the_same_delay_and_keys_spread() {
        assert_eq!(SEND.delay(5, 9), SEND.delay(5, 9));
        let spread: std::collections::BTreeSet<u64> =
            (0..64).map(|key| SEND.delay(9, key)).collect();
        assert!(
            spread.len() > 32,
            "64 rows do not retry in lockstep: {spread:?}"
        );
    }
}
