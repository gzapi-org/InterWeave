// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Pull mode's one queue (architect-cto's ruling, relay seq 18691): what
//! the bridge drains from its session while the host has not yet called
//! `receive`.
//!
//! **Bounded at the session's granted event queue**, the same number as
//! the IPC client's receive buffer -- never a new constant. The bridge
//! must keep draining its session (a full IPC receive buffer stops the
//! socket and the daemon closes the connection as wedged), so a host that
//! pulls slowly is answered from here, and what does not fit is counted,
//! never silently gone.
//!
//! **The reserved lane is kept** (LOCAL-IPC.md §Push events): an item the
//! caller marks reserved is never dropped. Past the bound, the OLDEST
//! ordinary item goes -- a host that comes back wants what is newest, and
//! the count says what it missed. Reserved items are few by construction
//! (the session's state, its lease, a peer's disconnect, its close), so
//! they may hold the queue past its bound rather than be lost.
//!
//! Pure and generic over the item: what an item is (its kind, content and
//! metadata) is the bridge's to decide.

use std::collections::VecDeque;

/// One take from the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Take<T> {
    /// The items taken, oldest first.
    pub items: Vec<T>,
    /// Ordinary items dropped since the previous take.
    pub dropped: u64,
    /// The queue's depth after this take: a host calls again while it is
    /// not zero.
    pub remaining: usize,
}

/// The pull queue.
#[derive(Debug)]
pub struct PullQueue<T> {
    bound: usize,
    items: VecDeque<(T, bool)>,
    /// Dropped since the last take.
    dropped: u64,
    /// Dropped since the queue was made, for `status`.
    dropped_total: u64,
}

impl<T> PullQueue<T> {
    /// An empty queue holding at most `bound` items (at least one).
    #[must_use]
    pub fn new(bound: usize) -> Self {
        Self {
            bound: bound.max(1),
            items: VecDeque::new(),
            dropped: 0,
            dropped_total: 0,
        }
    }

    /// The queue's bound: the most a `receive` may take at once.
    #[must_use]
    pub const fn bound(&self) -> usize {
        self.bound
    }

    /// Queue `item`. Past the bound the oldest ORDINARY item is dropped
    /// and counted; a reserved one is never dropped, so with only
    /// reserved items held the queue grows past its bound.
    pub fn push(&mut self, item: T, reserved: bool) {
        self.items.push_back((item, reserved));
        if self.items.len() > self.bound
            && let Some(at) = self.items.iter().position(|(_, reserved)| !reserved)
        {
            self.items.remove(at);
            self.dropped += 1;
            self.dropped_total += 1;
        }
    }

    /// Take at most `max` items, oldest first -- `max` clamped to the
    /// bound, never refused -- with the drops since the last take.
    pub fn take(&mut self, max: usize) -> Take<T> {
        let n = max.min(self.bound).min(self.items.len());
        let items = self.items.drain(..n).map(|(item, _)| item).collect();
        Take {
            items,
            dropped: std::mem::take(&mut self.dropped),
            remaining: self.items.len(),
        }
    }

    /// Items waiting.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.items.len()
    }

    /// Ordinary items dropped since the queue was made.
    #[must_use]
    pub const fn dropped_total(&self) -> u64 {
        self.dropped_total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Past the bound the oldest ordinary item goes, counted once in the
    /// next take and in the total; the newest are kept, oldest first.
    #[test]
    fn past_the_bound_the_oldest_ordinary_item_is_dropped_and_counted() {
        let mut q = PullQueue::new(3);
        for i in 0..5 {
            q.push(i, false);
        }
        assert_eq!(q.depth(), 3);
        let take = q.take(10);
        assert_eq!(take.items, [2, 3, 4], "the newest, oldest first");
        assert_eq!(take.dropped, 2);
        assert_eq!(take.remaining, 0);
        assert_eq!(q.dropped_total(), 2);
        // Counted once: the next take reports none.
        q.push(9, false);
        assert_eq!(q.take(10).dropped, 0);
        assert_eq!(q.dropped_total(), 2);
    }

    /// A reserved item is never dropped: past the bound the oldest
    /// ORDINARY one goes in its place, and only reserved items may hold
    /// the queue past its bound. The control is the same pushes all
    /// ordinary, where the reserved positions are dropped too.
    #[test]
    fn a_reserved_item_is_never_dropped() {
        let mut q = PullQueue::new(2);
        q.push("lease", true);
        q.push("a", false);
        q.push("b", false);
        assert_eq!(q.take(10).items, ["lease", "b"]);

        let mut all_reserved = PullQueue::new(2);
        for item in ["state", "lease", "close"] {
            all_reserved.push(item, true);
        }
        let take = all_reserved.take(10);
        assert_eq!(take.items.len(), 2, "a take is clamped to the bound");
        assert_eq!(take.remaining, 1);
        assert_eq!(take.dropped, 0, "nothing reserved was dropped");

        let mut control = PullQueue::new(2);
        for item in ["state", "a", "b"] {
            control.push(item, false);
        }
        assert_eq!(control.take(10).items, ["a", "b"]);
    }

    /// A take is clamped to the bound and to what waits, and says how many
    /// remain; zero takes nothing and still reports.
    #[test]
    fn a_take_is_clamped_and_says_what_remains() {
        let mut q = PullQueue::new(4);
        for i in 0..4 {
            q.push(i, false);
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
