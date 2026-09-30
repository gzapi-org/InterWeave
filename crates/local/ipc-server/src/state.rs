// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `server_state` for data clients (plan §16 (5)).
//!
//! Data clients get the NORMALIZED view -- the closed health set and the
//! connectivity summary's states and counts, never a probe server, a relay
//! `PeerId` or an address -- on connect and on change, coalesced to at most
//! one pending. The server derives it from an admin port it mints for
//! itself holding only `admin.status`: an internal holding that grants no
//! client anything. One timer, at the keepalive interval, re-reads it; a
//! push happens only when the view changed, and the summary's own
//! timestamp is not a change.

use std::time::Duration;

use interweave_ipc_protocol::ServerState;
use interweave_local_client_api::AdminPort;
use interweave_transport_api::ConnectivitySummary;
use tokio::sync::watch;

/// The normalized view: health and the summary without its timestamp.
fn same_view(a: &ServerState, b: &ServerState) -> bool {
    let strip = |summary: &Option<ConnectivitySummary>| {
        summary
            .clone()
            .map(|s| ConnectivitySummary { updated_at: 0, ..s })
    };
    a.health == b.health && strip(&a.connectivity) == strip(&b.connectivity)
}

/// Read the port's status as the normalized view.
pub(crate) async fn read<A: AdminPort>(port: &A) -> Option<ServerState> {
    port.status()
        .await
        .ok()
        .map(|status| ServerState::new(status.health, Some(status.connectivity)))
}

/// Publish `view` if it differs from what was last published; whether it
/// was.
pub(crate) fn publish(sender: &watch::Sender<Option<ServerState>>, view: ServerState) -> bool {
    sender.send_if_modified(|current| {
        if current.as_ref().is_some_and(|c| same_view(c, &view)) {
            return false;
        }
        *current = Some(view);
        true
    })
}

/// Re-read the view every `interval` until every receiver is gone.
pub(crate) async fn refresh<A: AdminPort>(
    port: A,
    sender: watch::Sender<Option<ServerState>>,
    interval: Duration,
) {
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            () = sender.closed() => return,
        }
        if let Some(view) = read(&port).await {
            publish(&sender, view);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::connectivity;
    use interweave_transport_api::Health;

    fn view(health: Health, updated_at: u64, reservations: u16) -> ServerState {
        ServerState::new(
            health,
            Some(ConnectivitySummary {
                updated_at,
                active_relay_reservations: reservations,
                ..connectivity()
            }),
        )
    }

    #[test]
    fn only_a_changed_view_is_published_and_a_timestamp_is_not_a_change() {
        let (sender, receiver) = watch::channel(None);
        assert!(
            publish(&sender, view(Health::Healthy, 1, 0)),
            "the first view"
        );
        assert!(
            !publish(&sender, view(Health::Healthy, 2, 0)),
            "only the timestamp moved"
        );
        assert!(
            publish(&sender, view(Health::Degraded, 3, 0)),
            "health moved"
        );
        assert!(
            publish(&sender, view(Health::Degraded, 4, 1)),
            "a count moved"
        );
        assert_eq!(
            receiver.borrow().as_ref().map(|s| s.health),
            Some(Health::Degraded)
        );
    }
}
