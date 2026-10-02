// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The event queue, coalesced per key (agreed item 6b): one slot per
//! row, one for the session, one for connectivity, one per disconnected
//! peer, latest wins. Nothing is ever dropped to make room -- a terminal
//! status or a session transition is never lost -- and the queue is
//! bounded by the number of distinct keys rather than by the number of
//! changes.

use std::collections::{HashMap, VecDeque};

use interweave_human_store::RowId;
use interweave_transport_api::TransportIdentity;

use crate::model::ClientEvent;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    Row(RowId),
    Session,
    Connectivity,
    Peer(TransportIdentity),
}

impl Key {
    fn of(event: &ClientEvent) -> Self {
        match event {
            ClientEvent::Outbound(update) => Self::Row(update.row),
            ClientEvent::Session(_) => Self::Session,
            ClientEvent::Connectivity(_) => Self::Connectivity,
            ClientEvent::PeerDisconnected { peer } => Self::Peer(peer.clone()),
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct EventQueue {
    order: VecDeque<Key>,
    latest: HashMap<Key, ClientEvent>,
}

impl EventQueue {
    /// Queue `event`, replacing an unread one with the same key in
    /// place: the key keeps its position, the value is the newest.
    pub(crate) fn push(&mut self, event: ClientEvent) {
        let key = Key::of(&event);
        if self.latest.insert(key.clone(), event).is_none() {
            self.order.push_back(key);
        }
    }

    /// The oldest key's newest event.
    pub(crate) fn pop(&mut self) -> Option<ClientEvent> {
        let key = self.order.pop_front()?;
        self.latest.remove(&key)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.order.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Connectivity, SessionState};

    #[test]
    fn a_key_keeps_its_place_and_its_newest_value() {
        let mut q = EventQueue::default();
        q.push(ClientEvent::Connectivity(Connectivity::Unknown));
        q.push(ClientEvent::Session(SessionState::Closed));
        q.push(ClientEvent::Connectivity(Connectivity::OnlineDirect));
        assert_eq!(q.len(), 2, "two keys, however many changes");
        assert_eq!(
            q.pop(),
            Some(ClientEvent::Connectivity(Connectivity::OnlineDirect))
        );
        assert_eq!(q.pop(), Some(ClientEvent::Session(SessionState::Closed)));
        assert_eq!(q.pop(), None);
    }

    #[test]
    fn a_thousand_changes_to_one_key_hold_one_slot() {
        let mut q = EventQueue::default();
        for attempt in 0..1_000 {
            q.push(ClientEvent::Session(SessionState::Reconnecting {
                attempt,
                next_at: 0,
            }));
        }
        assert_eq!(q.len(), 1);
        assert_eq!(
            q.pop(),
            Some(ClientEvent::Session(SessionState::Reconnecting {
                attempt: 999,
                next_at: 0
            }))
        );
    }
}
