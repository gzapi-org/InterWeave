// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The negotiated keepalive (`LOCAL-IPC.md` §Disconnect/reconnect and
//! optional keepalive; plan §16 (8)), as a clock-free state machine the
//! connection loop drives.
//!
//! One nonce outstanding at a time, 128 bits from a CSPRNG; only an exact
//! echo of the CURRENT nonce satisfies it, and a stale, duplicate or wrong
//! one changes nothing. An unanswered probe is a miss once its response
//! timeout passes; after `max_missed` misses in a row the connection is
//! closed and its lease released as for any disconnect. The nonce is not
//! authentication -- but it must be unpredictable, or a wedged client (or
//! a helper speaking for it) could answer probes it never read and keep
//! its lease forever, which is what keepalive exists to prevent.

use std::time::Instant;

use interweave_ipc_protocol::{Nonce, Ping, Pong};
use interweave_transport_api::base64url;

use crate::hello::KeepalivePolicy;

/// What the connection loop does at a wake-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    /// Nothing is due.
    Nothing,
    /// Send this probe.
    Probe(Ping),
    /// The miss threshold is reached: `close{Timeout}`.
    Close,
}

#[derive(Debug)]
pub(crate) struct Keepalive {
    policy: KeepalivePolicy,
    outstanding: Option<(Nonce, Instant)>,
    missed: u32,
    next_probe: Instant,
}

impl Keepalive {
    /// A keepalive whose first probe is one interval after `now`.
    pub(crate) fn new(policy: KeepalivePolicy, now: Instant) -> Self {
        Self {
            policy,
            outstanding: None,
            missed: 0,
            next_probe: now + policy.interval,
        }
    }

    /// When the loop must next call [`Keepalive::wake`].
    pub(crate) fn next_wake(&self) -> Instant {
        self.outstanding
            .as_ref()
            .map_or(self.next_probe, |(_, sent)| {
                *sent + self.policy.response_timeout
            })
    }

    pub(crate) fn wake(&mut self, now: Instant) -> Action {
        if let Some((_, sent)) = &self.outstanding {
            if now < *sent + self.policy.response_timeout {
                return Action::Nothing;
            }
            self.outstanding = None;
            self.missed += 1;
            if self.missed >= self.policy.max_missed {
                return Action::Close;
            }
        }
        if now < self.next_probe {
            return Action::Nothing;
        }
        let nonce = mint();
        self.outstanding = Some((nonce.clone(), now));
        self.next_probe = now + self.policy.interval;
        Action::Probe(Ping::new(nonce))
    }

    /// An echo. Only the current nonce, exactly, satisfies the probe and
    /// clears the misses; anything else is ignored.
    pub(crate) fn pong(&mut self, pong: &Pong) {
        if self
            .outstanding
            .as_ref()
            .is_some_and(|(nonce, _)| *nonce == pong.nonce)
        {
            self.outstanding = None;
            self.missed = 0;
        }
    }
}

/// 128 bits from `rand`'s thread-local generator -- a CSPRNG seeded from
/// the operating system -- as unpadded base64url (22 characters).
fn mint() -> Nonce {
    let bytes: [u8; 16] = rand::random();
    Nonce::new(base64url::encode(&bytes))
        .unwrap_or_else(|_| unreachable!("22 base64url characters are a nonce"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;
    use std::collections::BTreeSet;
    use std::time::Duration;

    fn policy() -> KeepalivePolicy {
        KeepalivePolicy {
            enabled: true,
            required_for_lease: true,
            interval: Duration::from_secs(30),
            response_timeout: Duration::from_secs(10),
            max_missed: 3,
        }
    }

    fn probe(action: Action) -> Ping {
        match action {
            Action::Probe(ping) => ping,
            other => panic!("expected a probe, got {other:?}"),
        }
    }

    #[test]
    fn nonces_are_128_bits_and_never_repeat() {
        let nonces: BTreeSet<String> = (0..1000).map(|_| mint().as_str().to_owned()).collect();
        assert_eq!(nonces.len(), 1000, "a thousand probes, a thousand nonces");
        for nonce in nonces.iter().take(3) {
            assert_eq!(base64url::decode(nonce).expect("base64url").len(), 16);
        }
    }

    #[test]
    fn an_exact_echo_satisfies_the_probe_and_anything_else_does_not() {
        let t0 = Instant::now();
        let mut keepalive = Keepalive::new(policy(), t0);
        assert_eq!(
            keepalive.wake(t0),
            Action::Nothing,
            "nothing before the interval"
        );
        let ping = probe(keepalive.wake(t0 + Duration::from_secs(30)));
        // Only one outstanding: no second probe while it waits.
        assert_eq!(
            keepalive.wake(t0 + Duration::from_secs(35)),
            Action::Nothing
        );
        let wrong = Ping::new(mint()).echo();
        keepalive.pong(&wrong);
        assert!(
            keepalive.outstanding.is_some(),
            "a wrong nonce changes nothing"
        );
        keepalive.pong(&ping.echo());
        assert!(
            keepalive.outstanding.is_none(),
            "the exact echo satisfies it"
        );
        keepalive.pong(&ping.echo());
        assert_eq!(keepalive.missed, 0, "a duplicate is ignored");
    }

    #[test]
    fn max_missed_unanswered_probes_close_the_connection() {
        let t0 = Instant::now();
        let mut keepalive = Keepalive::new(policy(), t0);
        let mut now = t0;
        let mut closes = 0;
        for round in 1..=3 {
            now += Duration::from_secs(30);
            let _ = probe(keepalive.wake(now));
            now += Duration::from_secs(10);
            match keepalive.wake(now) {
                Action::Close => closes += 1,
                Action::Nothing | Action::Probe(_) => {
                    assert!(round < 3, "the third miss closes");
                }
            }
        }
        assert_eq!(closes, 1);
    }

    #[test]
    fn an_answer_in_time_resets_the_count() {
        let t0 = Instant::now();
        let mut keepalive = Keepalive::new(policy(), t0);
        let mut now = t0;
        for _ in 0..2 {
            now += Duration::from_secs(30);
            let _ = probe(keepalive.wake(now));
            now += Duration::from_secs(10);
            assert_ne!(keepalive.wake(now), Action::Close);
        }
        now += Duration::from_secs(30);
        let ping = probe(keepalive.wake(now));
        keepalive.pong(&ping.echo());
        assert_eq!(
            keepalive.missed, 0,
            "two misses then an answer: the count restarts"
        );
        now += Duration::from_secs(30);
        let _ = probe(keepalive.wake(now));
        now += Duration::from_secs(10);
        assert_ne!(
            keepalive.wake(now),
            Action::Close,
            "one miss after the reset"
        );
    }

    #[test]
    fn the_next_wake_is_the_timeout_while_a_probe_waits() {
        let t0 = Instant::now();
        let mut keepalive = Keepalive::new(policy(), t0);
        assert_eq!(keepalive.next_wake(), t0 + Duration::from_secs(30));
        let at = t0 + Duration::from_secs(30);
        let _ = probe(keepalive.wake(at));
        assert_eq!(keepalive.next_wake(), at + Duration::from_secs(10));
    }
}
