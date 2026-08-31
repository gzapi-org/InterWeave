// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The global query budgets (§15): one concurrency ceiling and one
//! sliding rate window, shared across all three query classes.
//!
//! Productionized from the model SPIKE-003's K22 proved, with its three
//! load-bearing choices kept: the permit is acquired BEFORE the behaviour
//! is invoked (a scheduler consulted afterwards records a decision that
//! has already been made); completion is KEYED, not counted (a bare
//! decrement could be called twice, or once for a query the provider
//! never scheduled, and the ceiling would admit more than its budget);
//! and a RELEASED permit refunds its rate charge while a FINISHED one
//! keeps it (an acquisition that never became a query spent nothing —
//! leaving its timestamp in the window would let a repeated recovery
//! path exhaust the budget with zero queries started; a query that ran
//! really did use the window). `consume` used to be the second half of
//! that pair, for a charge held outside `running`; a charge is bound by
//! its handle now, so `finish` is the one path and `consume` had no
//! caller left.
//!
//! SETTLEMENT IS KEYED BY THE QUERY, not by its class. The port used to
//! carry only a `QueryClass`, so a completion settled the oldest
//! outstanding permit of that class — and with two bootstraps
//! outstanding (one commanded, one the library started on an empty-table
//! insertion, SPIKE-003 F2) either completion settled either permit.
//! `QueryHandle` names the query for its whole life, so a completion
//! settles the permit that query holds and no other. A handle nothing
//! is holding is visible (`false`) rather than silently widening the
//! ceiling.

use interweave_kademlia_control_api::QueryHandle;
use std::collections::{HashMap, VecDeque};

/// The rate window: one minute, sliding.
const WINDOW_MS: u64 = 60_000;

/// Permission to start exactly one query.
///
/// Not `Copy` and not `Clone`: a permit is one slot, and duplicating it
/// would let one acquisition start two queries.
#[derive(Debug)]
pub(crate) struct Permit(u64);

/// Which budget refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BudgetRefusal {
    /// Every concurrency slot is held.
    Concurrency,
    /// The rate window is spent.
    Rate,
}

/// Concurrency slots and the sliding rate window.
#[derive(Debug)]
pub(crate) struct QueryBudgets {
    max_concurrent: usize,
    max_per_minute: u32,
    /// Query starts inside the current window, oldest first. A deque
    /// rather than a counter: a counter reset on a tick would let a
    /// caller spend the whole budget in the last millisecond of one
    /// window and again in the first of the next.
    starts: VecDeque<u64>,
    /// Permits bound to a running query, by the handle that names it.
    running: HashMap<QueryHandle, u64>,
    /// Slots taken but not yet bound, with the timestamp each spent —
    /// so releasing gives back both.
    unbound: HashMap<u64, u64>,
    next_permit: u64,
}

impl QueryBudgets {
    pub(crate) fn new(max_concurrent: usize, max_per_minute: u32) -> Self {
        Self {
            max_concurrent,
            max_per_minute,
            starts: VecDeque::new(),
            running: HashMap::new(),
            unbound: HashMap::new(),
            next_permit: 0,
        }
    }

    /// Acquire a slot BEFORE the query exists.
    ///
    /// # Errors
    /// [`BudgetRefusal`] naming which budget refused; the two are
    /// distinct because "wait for a slot" and "wait for the window" are
    /// different waits.
    pub(crate) fn acquire(&mut self, now_ms: u64) -> Result<Permit, BudgetRefusal> {
        self.prune(now_ms);
        if self.running.len() + self.unbound.len() >= self.max_concurrent {
            return Err(BudgetRefusal::Concurrency);
        }
        if self.starts.len() >= self.max_per_minute as usize {
            return Err(BudgetRefusal::Rate);
        }
        Ok(self.mint(now_ms))
    }

    /// Charge for work the scheduler could not gate.
    ///
    /// F2: the library starts one bootstrap the caller never requested
    /// when a routing insertion lands on an empty table, and it dials.
    /// The work is real whether or not the budget had room, so refusing
    /// the accounting would under-count; this takes the slot and the
    /// rate charge unconditionally, past both ceilings if it must.
    pub(crate) fn charge_unscheduled(&mut self, now_ms: u64) -> Permit {
        self.prune(now_ms);
        self.mint(now_ms)
    }

    fn mint(&mut self, now_ms: u64) -> Permit {
        self.starts.push_back(now_ms);
        self.next_permit += 1;
        self.unbound.insert(self.next_permit, now_ms);
        Permit(self.next_permit)
    }

    fn prune(&mut self, now_ms: u64) {
        // The window is pruned first, so an old start cannot occupy the
        // rate budget forever.
        while self
            .starts
            .front()
            .is_some_and(|t| now_ms.saturating_sub(*t) >= WINDOW_MS)
        {
            self.starts.pop_front();
        }
    }

    /// Bind a permit to the query it started.
    ///
    /// Consumes the permit, so it cannot be bound twice.
    pub(crate) fn bind(&mut self, permit: Permit, handle: QueryHandle) {
        if self.unbound.remove(&permit.0).is_some() {
            self.running.insert(handle, permit.0);
        }
    }

    /// Give back a permit that never became a query — the rate charge
    /// too, not only the slot.
    pub(crate) fn release(&mut self, permit: Permit) {
        if let Some(at) = self.unbound.remove(&permit.0)
            && let Some(i) = self.starts.iter().position(|t| *t == at)
        {
            self.starts.remove(i);
        }
    }

    /// This query finished: settle the permit it holds, and no other.
    ///
    /// `false` means no permit was bound to that handle — a duplicate
    /// completion, or one for a query this provider never commanded.
    /// Visible rather than a widened ceiling, and unlike the
    /// class-keyed version it cannot settle somebody else's permit to
    /// say so.
    pub(crate) fn finish(&mut self, handle: QueryHandle) -> bool {
        self.running.remove(&handle).is_some()
    }

    /// Slots held: bound to a query, or taken and not yet bound.
    #[cfg(test)]
    pub(crate) fn held(&self) -> usize {
        self.running.len() + self.unbound.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ceiling_admits_exactly_its_budget() {
        let mut b = QueryBudgets::new(2, 60);
        let p0 = b.acquire(0).expect("first fits");
        let p1 = b.acquire(0).expect("second fits");
        assert_eq!(b.acquire(0).unwrap_err(), BudgetRefusal::Concurrency);
        b.bind(p0, QueryHandle::commanded(1));
        b.bind(p1, QueryHandle::commanded(2));
        assert_eq!(b.acquire(0).unwrap_err(), BudgetRefusal::Concurrency);
        assert!(b.finish(QueryHandle::commanded(1)), "keyed settle");
        b.acquire(1).expect("a settled slot is a free slot");
    }

    #[test]
    fn completion_is_keyed_by_class_not_counted() {
        let mut b = QueryBudgets::new(2, 60);
        let p = b.acquire(0).expect("fits");
        b.bind(p, QueryHandle::commanded(1));
        assert!(
            !b.finish(QueryHandle::implicit(1)),
            "a completion for a class with nothing outstanding is foreign"
        );
        assert_eq!(b.held(), 1, "and it settles nothing");
        assert!(b.finish(QueryHandle::commanded(1)));
        assert!(
            !b.finish(QueryHandle::commanded(1)),
            "a duplicate completion is visible, not a widened ceiling"
        );
    }

    #[test]
    fn the_rate_window_slides_rather_than_resetting() {
        let mut b = QueryBudgets::new(100, 6);
        // The whole budget, spent in the last moments of one "bucket".
        for _ in 0..6 {
            let p = b.acquire(59_990).expect("within rate");
            b.bind(p, QueryHandle::commanded(1));
            b.finish(QueryHandle::commanded(1));
        }
        assert_eq!(
            b.acquire(60_010).unwrap_err(),
            BudgetRefusal::Rate,
            "a fixed bucket would have reset at 60_000 and admitted this; \
             20ms after the spend the sliding window still refuses"
        );
        b.acquire(59_990 + WINDOW_MS)
            .expect("one window after the spend, the oldest start has aged out");
    }

    #[test]
    fn release_refunds_the_rate_and_finishing_keeps_it() {
        // K22's third choice: an acquisition that never became a query
        // spent nothing, so leaving its timestamp in the window would
        // let a repeated recovery path exhaust the budget with zero
        // queries started. A query that RAN really did use the window,
        // so finishing it returns the slot and keeps the charge.
        let mut b = QueryBudgets::new(10, 2);
        let kept = b.acquire(0).expect("fits");
        let refunded = b.acquire(0).expect("fits");
        assert_eq!(b.acquire(0).unwrap_err(), BudgetRefusal::Rate);
        b.release(refunded);
        let p = b
            .acquire(1)
            .expect("an acquisition that never became a query spent nothing");
        b.bind(p, QueryHandle::commanded(10));
        b.bind(kept, QueryHandle::commanded(11));
        assert!(b.finish(QueryHandle::commanded(10)));
        assert!(b.finish(QueryHandle::commanded(11)));
        assert_eq!(b.held(), 0, "finishing frees the slot");
        assert_eq!(
            b.acquire(2).unwrap_err(),
            BudgetRefusal::Rate,
            "but the rate charge of work that ran stays spent"
        );
    }

    #[test]
    fn an_unscheduled_charge_ignores_both_ceilings() {
        let mut b = QueryBudgets::new(1, 1);
        let p = b.acquire(0).expect("fits");
        b.bind(p, QueryHandle::commanded(2));
        let charge = b.charge_unscheduled(0);
        b.bind(charge, QueryHandle::implicit(1));
        assert_eq!(b.held(), 2, "the work was real; the accounting follows it");
        // SETTLED BY ITS OWN NAME. Both outstanding queries are of the
        // same class, so a class-keyed settle could not say which one
        // this completion was for — which is the whole reason the
        // handle exists.
        assert!(b.finish(QueryHandle::implicit(1)));
        assert_eq!(b.held(), 1, "and the commanded one still holds its slot");
        assert!(
            !b.finish(QueryHandle::implicit(1)),
            "a duplicate settles nothing rather than releasing its neighbour"
        );
        assert_eq!(b.held(), 1);
    }
}
