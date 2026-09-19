// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! The gate's own record of what it refused.
//!
//! # Why this exists at all
//!
//! A behaviour-originated dial that the root gate denies is
//! **invisible**. `Swarm::dial` builds `DialError::Denied`, hands the
//! behaviour `FromSwarm::DialFailure`, and returns the error — and the
//! caller for a behaviour-emitted `ToSwarm::Dial` is
//! `if let Ok(()) = self.dial(opts)` (libp2p-swarm 0.48.0
//! `lib.rs:1101`), which discards it. No `SwarmEvent::Dialing`, no
//! `SwarmEvent::OutgoingConnectionError`. Only the originating
//! behaviour is told, and an observer sees whatever that behaviour does
//! next: a Kademlia query that fails, or — SPIKE-004 measured this — a
//! relay listener closing with `reason: Ok(())`, a *successful* close.
//!
//! `ConnectionDenied`'s own `Display` is the bare string
//! `connection denied`, with everything the gate wrote about why
//! reachable only through `Error::source`. So a refusal logged the
//! obvious way says nothing either.
//!
//! The consequence is that a refusal not written down HERE is written
//! down nowhere. That is SPIKE-004's F8 and it is why this module is
//! step one of Stage 11 rather than an operational nicety: the whole
//! reachability stack fails closed against its own infrastructure if
//! attribution is wrong, and without this the failure has no symptom.
//!
//! # Bounded, because an attacker chooses the volume
//!
//! Counts are per `(origin, denial)` pair, which is a product of two
//! small enums and therefore bounded by construction. The recent-reason
//! ring is capped at [`RECENT_CAPACITY`] and drops oldest-first: a peer
//! that can provoke refusals must not be able to grow this without
//! limit.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use interweave_transport_runtime::{DialDenial, DialOrigin};

/// Refusals kept verbatim for diagnosis, oldest dropped first.
///
/// Small on purpose. The counts answer "is this happening"; the ring
/// answers "what did the most recent ones say", which is all an
/// operator needs before reaching for the counts.
pub const RECENT_CAPACITY: usize = 32;

/// One refusal, as the gate saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The origin the dial was attributed to, or `None` when the dial
    /// carried no attribution at all — which is itself a refusal
    /// reason, and the one that means a dialling behaviour was added
    /// without being wrapped.
    pub origin: Option<DialOrigin>,
    /// Why the policy said no, or `None` when the policy was never the
    /// one saying it: the gate refused before asking (no attribution,
    /// no peer, an identity outside the neutral grammar), or the policy
    /// ADMITTED the dial and the Swarm then failed it synchronously --
    /// a later field's denial, no address left -- and the gate released
    /// the ticket. `detail` tells the two apart, and
    /// [`DialRefusals::released_after_admission`] counts the second on
    /// its own.
    pub denial: Option<DialDenial>,
    /// What the gate would tell a reader, when neither of the above
    /// carries it.
    pub detail: &'static str,
}

/// Everything the gate has refused, shared with whoever reports it.
#[derive(Debug, Clone, Default)]
pub struct DialRefusals {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug, Default)]
struct Inner {
    counts: BTreeMap<(Option<DialOrigin>, Option<DialDenial>), u64>,
    recent: VecDeque<Refusal>,
    total: u64,
    released_after_admission: u64,
}

impl Inner {
    /// One refusal into the counts, the ring and the total, under the
    /// lock the caller holds -- so a release's own counter moves in the
    /// same critical section and no reader sees the total one ahead.
    fn write(&mut self, refusal: Refusal) {
        self.total = self.total.saturating_add(1);
        *self
            .counts
            .entry((refusal.origin, refusal.denial))
            .or_insert(0) += 1;
        if self.recent.len() == RECENT_CAPACITY {
            self.recent.pop_front();
        }
        self.recent.push_back(refusal);
    }
}

impl DialRefusals {
    /// Write down one refusal.
    pub fn record(&self, refusal: Refusal) {
        self.lock().write(refusal);
    }

    /// Write down a dial the policy admitted and the Swarm then failed
    /// before dialling, whose ticket the gate released.
    ///
    /// Recorded like every refusal, and counted apart: in `counts()` it
    /// lands under `(origin, None)` beside the pre-policy refusals, and
    /// an operator asking "is Kademlia being refused, or admitted and
    /// then failing" needs the two separable. Review finding on PR #91.
    pub fn record_release(&self, refusal: Refusal) {
        let mut inner = self.lock();
        inner.write(refusal);
        inner.released_after_admission = inner.released_after_admission.saturating_add(1);
    }

    /// Admitted dials the Swarm failed synchronously, whose tickets the
    /// gate released; a subset of [`Self::total`]. A punch dial the
    /// DCUtR wrapper denied to reissue it without its refused
    /// candidates is taken back and not counted here: it refused
    /// nothing (step 8).
    #[must_use]
    pub fn released_after_admission(&self) -> u64 {
        self.lock().released_after_admission
    }

    /// Refusals since start, by `(origin, denial)`.
    #[must_use]
    pub fn counts(&self) -> BTreeMap<(Option<DialOrigin>, Option<DialDenial>), u64> {
        self.lock().counts.clone()
    }

    /// The most recent refusals, oldest first, at most
    /// [`RECENT_CAPACITY`].
    #[must_use]
    pub fn recent(&self) -> Vec<Refusal> {
        self.lock().recent.iter().cloned().collect()
    }

    /// Every refusal ever recorded, including those the ring dropped.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.lock().total
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // Recovered rather than propagated, as elsewhere in this crate:
        // a poisoned diagnostic must not become a refusal path of its
        // own.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;

    fn refusal(origin: Option<DialOrigin>) -> Refusal {
        Refusal {
            origin,
            denial: Some(DialDenial::Unauthorized),
            detail: "test",
        }
    }

    /// THE PROPERTY THE RUNTIME DEPENDS ON.
    ///
    /// `SwarmRuntime` clones this handle and then moves the gate into
    /// the behaviour, where nothing can reach it again. That is only
    /// correct if a clone taken BEFORE a refusal still sees it — an
    /// eagerly-copied snapshot would leave the runtime reading a record
    /// frozen at start-up, which looks exactly like "no refusals".
    #[test]
    fn a_clone_taken_early_sees_refusals_recorded_later() {
        let held = DialRefusals::default();
        let taken_before = held.clone();
        assert_eq!(taken_before.total(), 0);

        held.record(refusal(Some(DialOrigin::KademliaQuery)));

        assert_eq!(
            taken_before.total(),
            1,
            "the clone is a view of one record, not a copy of an empty one"
        );
        assert_eq!(taken_before.recent().len(), 1);
        assert_eq!(
            taken_before.recent()[0].origin,
            Some(DialOrigin::KademliaQuery)
        );
    }

    #[test]
    fn the_ring_drops_oldest_first_while_the_total_keeps_counting() {
        let r = DialRefusals::default();
        for _ in 0..RECENT_CAPACITY {
            r.record(refusal(Some(DialOrigin::KademliaQuery)));
        }
        // One more, distinguishable from every entry before it.
        r.record(refusal(None));

        let recent = r.recent();
        assert_eq!(recent.len(), RECENT_CAPACITY);
        assert_eq!(
            recent[RECENT_CAPACITY - 1].origin,
            None,
            "the newest is kept"
        );
        assert!(
            recent
                .iter()
                .take(RECENT_CAPACITY - 1)
                .all(|e| e.origin == Some(DialOrigin::KademliaQuery)),
            "and the one dropped was the oldest"
        );
        assert_eq!(r.total(), (RECENT_CAPACITY + 1) as u64);
        assert_eq!(
            r.counts().values().sum::<u64>(),
            (RECENT_CAPACITY + 1) as u64,
            "the counts are unbounded in value and bounded in KEYS, so they lose nothing"
        );
    }
}
