// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Pull mode's one queue (architect-cto's ruling, relay seq 18784): the
//! direct messages and broadcasts the bridge took from its session and the
//! host has not yet taken with `receive`.
//!
//! **It drops nothing it took.** A direct message in it was already
//! acknowledged to its sender, and TRANSPORT.md §Backpressure forbids
//! acknowledging and then losing one. So the queue is bounded -- at the
//! session's granted event queue, the same number as the IPC client's
//! receive buffer, never a new constant -- and when it is FULL the bridge
//! stops draining its session: the daemon's own rules then decide what the
//! session cannot take (a direct refused to its sender as overloaded, an
//! ordinary broadcast dropped and counted at the daemon). [`PullQueue::push`]
//! refuses past the bound rather than evict, so the caller cannot forget.
//!
//! Session notices are never in it: they are the bridge's state and its
//! `status` in both modes.
//!
//! Pure and generic over the item.

use std::collections::VecDeque;

/// One take from the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Take<T> {
    /// The items taken, in the order they were pushed.
    pub items: Vec<T>,
    /// The queue's depth after this take: a host calls again while it is
    /// not zero.
    pub remaining: usize,
}

/// The pull queue.
#[derive(Debug)]
pub struct PullQueue<T> {
    bound: usize,
    items: VecDeque<T>,
}

impl<T> PullQueue<T> {
    /// An empty queue holding at most `bound` items (at least one).
    #[must_use]
    pub fn new(bound: usize) -> Self {
        Self {
            bound: bound.max(1),
            items: VecDeque::new(),
        }
    }

    /// The queue's bound: the most a `receive` takes at once.
    #[must_use]
    pub const fn bound(&self) -> usize {
        self.bound
    }

    /// Room left: how many more the caller may take from its session.
    /// Zero is the paused state.
    #[must_use]
    pub fn room(&self) -> usize {
        self.bound - self.items.len()
    }

    /// Whether the queue is full, so the session's draining is paused.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.room() == 0
    }

    /// Queue `item`.
    ///
    /// # Errors
    /// The item back when the queue is full: nothing taken is ever
    /// evicted, so a caller that took past [`PullQueue::room`] keeps it.
    pub fn push(&mut self, item: T) -> Result<(), T> {
        if self.is_full() {
            return Err(item);
        }
        self.items.push_back(item);
        Ok(())
    }

    /// Take at most `max` items, in the order pushed -- `max` clamped to
    /// the bound, never refused.
    pub fn take(&mut self, max: usize) -> Take<T> {
        let n = max.min(self.bound).min(self.items.len());
        let items = self.items.drain(..n).collect();
        Take {
            items,
            remaining: self.items.len(),
        }
    }

    /// Items waiting.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.items.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Full, the queue refuses: the item comes back, nothing held is
    /// evicted, and the room is zero -- the paused state -- until a take.
    #[test]
    fn a_full_queue_refuses_and_evicts_nothing() {
        let mut q = PullQueue::new(2);
        assert_eq!(q.room(), 2);
        q.push("a").expect("room");
        q.push("b").expect("room");
        assert!(q.is_full());
        assert_eq!(q.push("c"), Err("c"), "refused, returned");
        let take = q.take(1);
        assert_eq!(take.items, ["a"], "nothing held was evicted");
        assert_eq!(take.remaining, 1);
        assert!(!q.is_full(), "a take lifts the pause");
        q.push("c").expect("room again");
        assert_eq!(q.take(10).items, ["b", "c"], "in the order pushed");
    }

    /// A take is clamped to the bound and to what waits, and says how many
    /// remain; zero takes nothing and still reports.
    #[test]
    fn a_take_is_clamped_and_says_what_remains() {
        let mut q = PullQueue::new(4);
        for i in 0..4 {
            q.push(i).expect("room");
        }
        let first = q.take(3);
        assert_eq!((first.items.len(), first.remaining), (3, 1));
        let none = q.take(0);
        assert_eq!((none.items.len(), none.remaining), (0, 1));
        let rest = q.take(usize::MAX);
        assert_eq!((rest.items, rest.remaining), (vec![3], 0));
        assert_eq!(PullQueue::<u8>::new(0).bound(), 1, "at least one");
    }
}
