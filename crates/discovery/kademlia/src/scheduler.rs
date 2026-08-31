// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Query pacing: exploration due-times with jitter and no-progress
//! backoff, and bootstrap spacing (§9.1, §9.3, §13).

use interweave_kademlia_control_api::RoutingView;

/// When the next query of each pace-limited kind may run.
#[derive(Debug, Default)]
pub(crate) struct Pacing {
    /// When exploration is next due. `None` until first scheduled — the
    /// first eligible tick explores immediately.
    next_exploration_due: Option<u64>,
    /// When the last bootstrap was issued, for `bootstrap_min_interval`.
    last_bootstrap: Option<u64>,
}

impl Pacing {
    /// Whether exploration is due.
    pub(crate) fn exploration_due(&self, now_ms: u64) -> bool {
        self.next_exploration_due.is_none_or(|due| now_ms >= due)
    }

    /// When exploration is next due, if scheduled.
    pub(crate) const fn next_exploration_due_ms(&self) -> Option<u64> {
        self.next_exploration_due
    }

    /// Schedule the next exploration from the view's backed-off interval
    /// plus jitter.
    ///
    /// The interval comes from [`RoutingView::next_exploration_interval_ms`]
    /// — consumed, not reimplemented — which doubles per no-progress round
    /// and caps at 15 minutes. Jitter is ±`jitter_percent` of that
    /// interval, derived from caller-supplied entropy so synchronized
    /// fleets drift apart; deterministic given the entropy, which is what
    /// makes it testable.
    pub(crate) fn schedule_exploration(
        &mut self,
        now_ms: u64,
        view: &RoutingView,
        base_interval_ms: u64,
        jitter_percent: u32,
        entropy: u64,
    ) {
        let interval = view.next_exploration_interval_ms(base_interval_ms);
        let range = interval
            .saturating_mul(u64::from(jitter_percent))
            .checked_div(100)
            .unwrap_or(0);
        // A value in [-range, +range], from the entropy.
        let span = range.saturating_mul(2).saturating_add(1);
        let offset = entropy % span;
        let jittered = interval.saturating_add(offset).saturating_sub(range);
        self.next_exploration_due = Some(now_ms.saturating_add(jittered));
    }

    /// Reset the exploration pace to "due now" — progress arrived, so
    /// the backed-off interval no longer describes the situation.
    pub(crate) fn reset_exploration(&mut self) {
        self.next_exploration_due = None;
    }

    /// Whether a bootstrap may run under `bootstrap_min_interval`.
    pub(crate) fn bootstrap_allowed(&self, now_ms: u64, min_interval_ms: u64) -> bool {
        self.last_bootstrap
            .is_none_or(|at| now_ms.saturating_sub(at) >= min_interval_ms)
    }

    /// Whether the periodic refresh is due under
    /// `bootstrap_refresh_interval`.
    pub(crate) fn bootstrap_refresh_due(&self, now_ms: u64, refresh_interval_ms: u64) -> bool {
        self.last_bootstrap
            .is_some_and(|at| now_ms.saturating_sub(at) >= refresh_interval_ms)
    }

    /// Record that a bootstrap was issued.
    pub(crate) const fn record_bootstrap(&mut self, now_ms: u64) {
        self.last_bootstrap = Some(now_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interweave_kademlia_control_api::MAX_EXPLORATION_INTERVAL_MS;

    fn view(no_progress_rounds: u32) -> RoutingView {
        RoutingView {
            routing_peers: 1,
            target_routing_peers: 64,
            max_routing_peers: 256,
            remote_trusted_population: 2,
            no_progress_rounds,
        }
    }

    #[test]
    fn unscheduled_exploration_is_due_immediately() {
        let p = Pacing::default();
        assert!(p.exploration_due(0), "the first eligible tick explores");
    }

    #[test]
    fn no_progress_backs_off_to_the_cap() {
        let mut p = Pacing::default();
        // Zero jitter, so the interval is exact.
        p.schedule_exploration(0, &view(0), 60_000, 0, 0);
        assert_eq!(p.next_exploration_due_ms(), Some(60_000), "base interval");
        p.schedule_exploration(0, &view(3), 60_000, 0, 0);
        assert_eq!(
            p.next_exploration_due_ms(),
            Some(480_000),
            "three no-progress rounds double three times"
        );
        p.schedule_exploration(0, &view(30), 60_000, 0, 0);
        assert_eq!(
            p.next_exploration_due_ms(),
            Some(MAX_EXPLORATION_INTERVAL_MS),
            "a two-peer overlay does not run a useless 60-second loop forever; \
             it rests at the 15-minute cap"
        );
        assert!(!p.exploration_due(MAX_EXPLORATION_INTERVAL_MS - 1));
        assert!(p.exploration_due(MAX_EXPLORATION_INTERVAL_MS));
    }

    #[test]
    fn jitter_stays_inside_its_band_and_tracks_entropy() {
        let mut p = Pacing::default();
        // ±20% of 60s = ±12s.
        p.schedule_exploration(0, &view(0), 60_000, 20, 0);
        assert_eq!(p.next_exploration_due_ms(), Some(48_000), "the low edge");
        p.schedule_exploration(0, &view(0), 60_000, 20, 24_000);
        assert_eq!(p.next_exploration_due_ms(), Some(72_000), "the high edge");
        p.schedule_exploration(0, &view(0), 60_000, 20, 12_000);
        assert_eq!(p.next_exploration_due_ms(), Some(60_000), "the middle");
    }

    #[test]
    fn progress_resets_the_pace() {
        let mut p = Pacing::default();
        p.schedule_exploration(0, &view(5), 60_000, 0, 0);
        assert!(!p.exploration_due(100));
        p.reset_exploration();
        assert!(p.exploration_due(100));
    }

    #[test]
    fn bootstrap_spacing_has_a_floor_and_a_refresh() {
        let mut p = Pacing::default();
        assert!(p.bootstrap_allowed(0, 300_000), "the first is free");
        assert!(
            !p.bootstrap_refresh_due(0, 900_000),
            "nothing to refresh yet"
        );
        p.record_bootstrap(0);
        assert!(!p.bootstrap_allowed(299_999, 300_000), "the floor holds");
        assert!(p.bootstrap_allowed(300_000, 300_000));
        assert!(!p.bootstrap_refresh_due(899_999, 900_000));
        assert!(p.bootstrap_refresh_due(900_000, 900_000));
    }
}
