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
//! `if let Ok(()) = self.dial(opts)` (libp2p-swarm 0.47.1
//! `lib.rs:1098`), which discards it. No `SwarmEvent::Dialing`, no
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
    /// Why the policy said no, or `None` when the gate refused before
    /// asking it.
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
}

impl DialRefusals {
    /// Write down one refusal.
    pub fn record(&self, refusal: Refusal) {
        let mut inner = self.lock();
        inner.total = inner.total.saturating_add(1);
        *inner
            .counts
            .entry((refusal.origin, refusal.denial))
            .or_insert(0) += 1;
        if inner.recent.len() == RECENT_CAPACITY {
            inner.recent.pop_front();
        }
        inner.recent.push_back(refusal);
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
