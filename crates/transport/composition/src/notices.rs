// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The runtime's peer notices to the in-process sessions: a session
//! holding `events` is owed each `peer.disconnected` (`LOCAL-IPC.md`'s
//! event catalogue: every connection with `events`), read before its
//! messages, as the reserved lane is.
//!
//! The runtime knows peers and the binding knows sessions, so the two
//! meet here: the driver posts a disconnect to every registered session,
//! and a session takes its own from `events`.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

use interweave_local_client_api::LocalSessionEvent;
use interweave_transport_api::{DisconnectReason, TransportIdentity};

/// The most peer notices one session is owed. A notice for a peer
/// already owed one replaces it, so a session that reads holds at most
/// one per peer; past this many distinct peers unread, the oldest goes
/// (`a_sessions_notices_are_bounded_and_keep_the_newest`).
pub(crate) const MAX_PEER_NOTICES: usize = 64;

/// The registry, shared by the binding (which registers and forgets
/// sessions) and the driver (which posts).
#[derive(Clone, Default)]
pub(crate) struct PeerNotices {
    owed: Arc<Mutex<BTreeMap<String, VecDeque<LocalSessionEvent>>>>,
}

impl PeerNotices {
    fn owed(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, VecDeque<LocalSessionEvent>>> {
        self.owed.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Start owing `session` its notices: from now, not before.
    pub(crate) fn register(&self, session: &str) {
        self.owed().entry(session.to_owned()).or_default();
    }

    /// A session that has gone is owed nothing, and holds nothing.
    pub(crate) fn forget(&self, session: &str) {
        self.owed().remove(session);
    }

    /// Owe every registered session `peer`'s disconnect.
    pub(crate) fn disconnected(&self, peer: &TransportIdentity, reason: DisconnectReason) {
        for queue in self.owed().values_mut() {
            queue.retain(|notice| {
                !matches!(notice, LocalSessionEvent::PeerDisconnected { peer: owed, .. } if owed == peer)
            });
            if queue.len() >= MAX_PEER_NOTICES {
                queue.pop_front();
            }
            queue.push_back(LocalSessionEvent::PeerDisconnected {
                peer: peer.clone(),
                reason_class: reason.as_str().to_owned(),
            });
        }
    }

    /// Take at most `max` of `session`'s notices, oldest first.
    pub(crate) fn take(&self, session: &str, max: usize) -> Vec<LocalSessionEvent> {
        let mut owed = self.owed();
        let Some(queue) = owed.get_mut(session) else {
            return Vec::new();
        };
        let n = queue.len().min(max);
        queue.drain(..n).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_PEER_NOTICES, PeerNotices};
    use interweave_local_client_api::LocalSessionEvent;
    use interweave_profile_identity::ProfileIdentity;
    use interweave_transport_api::{DisconnectReason, TransportIdentity};

    fn peer() -> TransportIdentity {
        ProfileIdentity::generate()
            .transport_identity()
            .expect("peer id")
    }

    fn gone(notices: &[LocalSessionEvent]) -> Vec<(TransportIdentity, String)> {
        notices
            .iter()
            .map(|n| match n {
                LocalSessionEvent::PeerDisconnected { peer, reason_class } => {
                    (peer.clone(), reason_class.clone())
                }
                other @ LocalSessionEvent::EndpointLeaseChanged { .. } => {
                    panic!("only disconnects: {other:?}")
                }
            })
            .collect()
    }

    /// Every registered session is told, an unregistered one is not, and
    /// a forgotten one holds nothing.
    #[test]
    fn each_registered_session_is_owed_the_disconnect() {
        let notices = PeerNotices::default();
        notices.register("a");
        notices.register("b");
        let p = peer();
        notices.disconnected(&p, DisconnectReason::Policy);
        for session in ["a", "b"] {
            assert_eq!(
                gone(&notices.take(session, usize::MAX)),
                [(p.clone(), "policy".to_owned())],
                "{session}"
            );
        }
        assert!(notices.take("never", usize::MAX).is_empty());
        notices.disconnected(&p, DisconnectReason::Closed);
        notices.forget("a");
        notices.register("a");
        assert!(
            notices.take("a", usize::MAX).is_empty(),
            "a forgotten session's notices went with it"
        );
        assert_eq!(notices.take("b", 0), Vec::new(), "a bounded take");
        assert_eq!(notices.take("b", usize::MAX).len(), 1);
    }

    /// One notice per peer: a second disconnect replaces the first, with
    /// its own class, at the back.
    #[test]
    fn a_peer_owed_twice_is_owed_once_with_the_latest_class() {
        let notices = PeerNotices::default();
        notices.register("s");
        let (p, q) = (peer(), peer());
        notices.disconnected(&p, DisconnectReason::Closed);
        notices.disconnected(&q, DisconnectReason::Closed);
        notices.disconnected(&p, DisconnectReason::Policy);
        assert_eq!(
            gone(&notices.take("s", usize::MAX)),
            [(q, "closed".to_owned()), (p, "policy".to_owned())]
        );
    }

    #[test]
    fn a_sessions_notices_are_bounded_and_keep_the_newest() {
        let notices = PeerNotices::default();
        notices.register("s");
        let peers: Vec<_> = (0..=MAX_PEER_NOTICES).map(|_| peer()).collect();
        for p in &peers {
            notices.disconnected(p, DisconnectReason::Closed);
        }
        let held = gone(&notices.take("s", usize::MAX));
        assert_eq!(held.len(), MAX_PEER_NOTICES, "never past the bound");
        assert_eq!(held[0].0, peers[1], "the oldest went");
        assert_eq!(held[MAX_PEER_NOTICES - 1].0, peers[MAX_PEER_NOTICES]);
    }
}
