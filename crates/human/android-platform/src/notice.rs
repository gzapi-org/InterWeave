// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! What the platform is asked to tell a person while no window of the
//! app has their focus: that messages arrived, how many, and never what
//! they say (human-client-android.md, "Notifications": a message is
//! committed unread first, then a local notification MAY be posted).
//!
//! A count, not a list: a notice carries no content, no sender and no
//! conversation, so nothing the store deletes lives on in it -- a
//! notification must not become a shadow archive. Previews, which a
//! person may opt into, are a later step.

use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

#[derive(Debug, Default)]
struct State {
    waiting: u32,
    /// The count changed since the platform last asked.
    changed: bool,
}

/// The messages waiting for a person's notice.
#[derive(Debug, Default)]
pub struct Notices {
    state: Mutex<State>,
    changed: Condvar,
}

impl Notices {
    /// A message was committed unread while no window had focus.
    pub fn received(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.waiting = state.waiting.saturating_add(1);
        state.changed = true;
        self.changed.notify_all();
    }

    /// A window took the person's focus: what waited is read there now,
    /// and the platform withdraws the notice.
    pub fn clear(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.waiting != 0 {
            state.waiting = 0;
            state.changed = true;
            self.changed.notify_all();
        }
    }

    /// Wait at most `timeout` for the count to change, and answer it --
    /// zero meaning "withdraw the notice" -- or `None` when it did not.
    /// Several changes in between are one answer, the latest.
    #[must_use]
    pub fn wait_change(&self, timeout: Duration) -> Option<u32> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut state, _) = self
            .changed
            .wait_timeout_while(state, timeout, |state| !state.changed)
            .unwrap_or_else(PoisonError::into_inner);
        if !state.changed {
            return None;
        }
        state.changed = false;
        Some(state.waiting)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: Duration = Duration::ZERO;

    #[test]
    fn arrivals_are_counted_and_answered_once_as_the_latest() {
        let notices = Notices::default();
        assert_eq!(notices.wait_change(NOW), None, "nothing changed yet");
        notices.received();
        notices.received();
        assert_eq!(notices.wait_change(NOW), Some(2));
        assert_eq!(notices.wait_change(NOW), None, "answered");
    }

    #[test]
    fn focus_withdraws_the_notice_and_an_empty_clear_says_nothing() {
        let notices = Notices::default();
        notices.clear();
        assert_eq!(notices.wait_change(NOW), None, "nothing to withdraw");
        notices.received();
        notices.clear();
        assert_eq!(notices.wait_change(NOW), Some(0), "withdraw");
    }

    #[test]
    fn a_waiter_is_woken_by_an_arrival_from_another_thread() {
        let notices = std::sync::Arc::new(Notices::default());
        let other = std::sync::Arc::clone(&notices);
        let arrival = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            other.received();
        });
        assert_eq!(notices.wait_change(Duration::from_secs(10)), Some(1));
        arrival.join().expect("joined");
    }
}
