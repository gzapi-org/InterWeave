// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The event queue, coalesced per key (agreed item 6b, as amended by
//! A4): one slot per row, one for the session, one for connectivity, one
//! per disconnected peer, one per peer's path, one for unread-in-store
//! (A5), latest wins. The LATEST value per key is never
//! dropped -- so a row's terminal status, which nothing overwrites, is
//! never lost -- while intermediate session states between two polls
//! collapse into the last one. Bounded by the number of distinct keys
//! rather than by the number of changes.

use std::collections::{HashMap, VecDeque};

use interweave_human_client_api::ClientEvent;
use interweave_human_core::RowId;
use interweave_transport_api::TransportIdentity;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    Row(RowId),
    Session,
    Connectivity,
    Peer(TransportIdentity),
    Path(TransportIdentity),
    UnreadInStore,
}

impl Key {
    fn of(event: &ClientEvent) -> Self {
        match event {
            ClientEvent::Outbound(update) => Self::Row(update.row),
            ClientEvent::Session(_) => Self::Session,
            ClientEvent::Connectivity(_) => Self::Connectivity,
            ClientEvent::PeerDisconnected { peer } => Self::Peer(peer.clone()),
            ClientEvent::PeerPath { peer, .. } => Self::Path(peer.clone()),
            ClientEvent::UnreadInStore { .. } => Self::UnreadInStore,
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

/// A FIFO with a hard cap: past it, a push is refused and the caller
/// counts it. Holds committed inbound awaiting hand-over, which stays in
/// the store as unread either way -- an overflow is a message shown from
/// the store rather than from this queue, never one lost.
#[derive(Debug)]
pub(crate) struct Capped<T> {
    items: VecDeque<T>,
    cap: usize,
}

impl<T> Capped<T> {
    pub(crate) const fn new(cap: usize) -> Self {
        Self {
            items: VecDeque::new(),
            cap,
        }
    }

    /// Push `item` unless the cap is reached; `false` if refused.
    pub(crate) fn push(&mut self, item: T) -> bool {
        if self.items.len() >= self.cap {
            return false;
        }
        self.items.push_back(item);
        true
    }

    pub(crate) fn pop(&mut self) -> Option<T> {
        self.items.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interweave_human_client_api::{Connectivity, SessionState};

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
    fn the_capped_queue_refuses_past_its_cap_and_keeps_its_order() {
        let mut held = Capped::new(2);
        assert!(held.push(1));
        assert!(held.push(2));
        assert!(!held.push(3), "the third is refused, not queued");
        assert_eq!(held.pop(), Some(1));
        assert!(held.push(4), "room again once one is taken");
        assert_eq!(
            (held.pop(), held.pop(), held.pop()),
            (Some(2), Some(4), None)
        );
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
