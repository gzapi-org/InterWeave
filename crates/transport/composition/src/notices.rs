// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! What the runtime owes the in-process sessions beyond their queues,
//! and how a session waiting in `ready` is woken.
//!
//! A session holding `events` is owed each `peer.disconnected`
//! (`LOCAL-IPC.md`'s event catalogue: every connection with `events`) and
//! the runtime's state as one coalesced `ServerState`, both read before
//! its messages, as the reserved lane is. The runtime knows peers, health
//! and deliveries, and the binding knows sessions, so the two meet here:
//! the driver posts and wakes, and a session takes its own from `events`.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use interweave_local_client_api::LocalSessionEvent;
use interweave_transport_api::{
    ConnectivitySummary, DisconnectReason, EndpointId, Health, TransportIdentity,
};
use tokio::sync::Notify;

/// The most peer notices one session is owed. A notice for a peer
/// already owed one replaces it, so a session that reads holds at most
/// one per peer; past this many distinct peers unread, the oldest goes
/// (`a_sessions_notices_are_bounded_and_keep_the_newest`).
pub const MAX_PEER_NOTICES: usize = 64;

/// One session's entry.
struct Owed {
    /// The runtime's state, the newest only: replaced, never queued
    /// (`the_state_is_owed_on_register_and_coalesced_to_the_newest`).
    state: Option<LocalSessionEvent>,
    peers: VecDeque<LocalSessionEvent>,
    /// The endpoint the session's lease names, whose deliveries wake it.
    endpoint: Option<EndpointId>,
    wake: Arc<Notify>,
}

impl Owed {
    fn wake(&self) {
        // Every task waiting in this session's `ready` -- it takes
        // `&self`, so there may be several -- and a stored permit for
        // the next (`every_concurrent_ready_ends_with_the_runtime`).
        self.wake.notify_waiters();
        self.wake.notify_one();
    }
}

#[derive(Default)]
struct Registry {
    sessions: BTreeMap<String, Owed>,
    /// The state last published, owed to a session as it registers.
    current: Option<(Health, Option<ConnectivitySummary>)>,
    /// The driver has ended: every session's wait is over.
    ended: bool,
}

/// The registry, shared by the binding (which registers and forgets
/// sessions) and the driver (which posts and wakes).
#[derive(Clone, Default)]
pub(crate) struct SessionNotices {
    registry: Arc<Mutex<Registry>>,
    /// Notices lost to the bound, across every session: LOCAL-IPC.md
    /// §Push events counts a drop rather than letting it pass unseen.
    evicted: Arc<AtomicU64>,
}

/// The registry as an operator reads it (`Diagnostics::peer_notices`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerNoticeDiagnostics {
    /// Sessions registered for peer notices: those holding `events` that
    /// have not ended. A session's entry goes when it closes or drops.
    pub sessions: usize,
    /// Notices dropped, oldest first, at a session's
    /// [`MAX_PEER_NOTICES`] bound since the runtime started.
    pub evicted_total: u64,
}

/// Two states are the same view when only the summary's timestamp
/// differs: a re-read is not a change (`CONNECTIVITY.md` §5).
fn same_view(
    a: &(Health, Option<ConnectivitySummary>),
    b: &(Health, Option<ConnectivitySummary>),
) -> bool {
    let strip = |s: &Option<ConnectivitySummary>| {
        s.clone()
            .map(|s| ConnectivitySummary { updated_at: 0, ..s })
    };
    a.0 == b.0 && strip(&a.1) == strip(&b.1)
}

fn state_event(
    (health, connectivity): &(Health, Option<ConnectivitySummary>),
) -> LocalSessionEvent {
    LocalSessionEvent::ServerState {
        health: *health,
        connectivity: connectivity.clone(),
    }
}

impl SessionNotices {
    fn registry(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.registry.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Start owing `session` its notices: from now, not before -- and
    /// the current state at once, as a connection is sent it on connect.
    /// What wakes it: these, and deliveries to `endpoint`.
    pub(crate) fn register(&self, session: &str, endpoint: Option<EndpointId>) -> Arc<Notify> {
        let mut registry = self.registry();
        let state = registry.current.as_ref().map(state_event);
        let owed = registry
            .sessions
            .entry(session.to_owned())
            .or_insert_with(|| Owed {
                state: None,
                peers: VecDeque::new(),
                endpoint,
                wake: Arc::new(Notify::new()),
            });
        if state.is_some() {
            owed.state = state;
            owed.wake();
        }
        Arc::clone(&owed.wake)
    }

    /// A session that has gone is owed nothing, and holds nothing.
    pub(crate) fn forget(&self, session: &str) {
        self.registry().sessions.remove(session);
    }

    /// Owe every registered session `peer`'s disconnect.
    pub(crate) fn disconnected(&self, peer: &TransportIdentity, reason: DisconnectReason) {
        for owed in self.registry().sessions.values_mut() {
            owed.peers.retain(|notice| {
                !matches!(notice, LocalSessionEvent::PeerDisconnected { peer: owed, .. } if owed == peer)
            });
            if owed.peers.len() >= MAX_PEER_NOTICES {
                owed.peers.pop_front();
                self.evicted.fetch_add(1, Ordering::Relaxed);
            }
            owed.peers.push_back(LocalSessionEvent::PeerDisconnected {
                peer: peer.clone(),
                reason_class: reason.as_str().to_owned(),
            });
            owed.wake();
        }
    }

    /// Publish the runtime's state: owed to every session, replacing what
    /// it held, when it differs from the last published -- and to each
    /// session registering later. Whether it was published.
    pub(crate) fn server_state(
        &self,
        health: Health,
        connectivity: Option<ConnectivitySummary>,
    ) -> bool {
        let mut registry = self.registry();
        let view = (health, connectivity);
        if registry
            .current
            .as_ref()
            .is_some_and(|current| same_view(current, &view))
        {
            return false;
        }
        let event = state_event(&view);
        registry.current = Some(view);
        for owed in registry.sessions.values_mut() {
            owed.state = Some(event.clone());
            owed.wake();
        }
        true
    }

    /// Wake the sessions whose lease names `endpoint`: a message was
    /// queued for it. A session whose lease has since ended finds
    /// nothing when it looks, and waits again.
    pub(crate) fn delivered_to(&self, endpoint: &EndpointId) {
        for owed in self.registry().sessions.values() {
            if owed.endpoint.as_ref() == Some(endpoint) {
                owed.wake();
            }
        }
    }

    /// Wake `session`: something was queued for it by name.
    pub(crate) fn wake(&self, session: &str) {
        if let Some(owed) = self.registry().sessions.get(session) {
            owed.wake();
        }
    }

    /// The runtime has ended: every wait is over, now and later.
    pub(crate) fn end(&self) {
        let mut registry = self.registry();
        registry.ended = true;
        for owed in registry.sessions.values() {
            owed.wake();
        }
    }

    /// Whether `session`'s wait is over here: a notice owed, or the end.
    pub(crate) fn ready(&self, session: &str) -> bool {
        let registry = self.registry();
        registry.ended
            || registry
                .sessions
                .get(session)
                .is_some_and(|owed| owed.state.is_some() || !owed.peers.is_empty())
    }

    /// The registry's size and what its bound has dropped.
    pub(crate) fn diagnostics(&self) -> PeerNoticeDiagnostics {
        PeerNoticeDiagnostics {
            sessions: self.registry().sessions.len(),
            evicted_total: self.evicted.load(Ordering::Relaxed),
        }
    }

    /// Take at most `max` of `session`'s notices: the state first, then
    /// the disconnects oldest first.
    pub(crate) fn take(&self, session: &str, max: usize) -> Vec<LocalSessionEvent> {
        let mut registry = self.registry();
        let Some(owed) = registry.sessions.get_mut(session) else {
            return Vec::new();
        };
        let mut taken = Vec::new();
        if max > 0
            && let Some(state) = owed.state.take()
        {
            taken.push(state);
        }
        let n = owed.peers.len().min(max - taken.len());
        taken.extend(owed.peers.drain(..n));
        taken
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_PEER_NOTICES, SessionNotices};
    use interweave_local_client_api::LocalSessionEvent;
    use interweave_profile_identity::ProfileIdentity;
    use interweave_transport_api::{
        ConnectivitySummary, DirectInboundState, DisconnectReason, EndpointId, Health,
        PathReadiness, PreferredPathPolicy, TransportIdentity,
    };

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
                other @ (LocalSessionEvent::EndpointLeaseChanged { .. }
                | LocalSessionEvent::ServerState { .. }) => {
                    panic!("only disconnects: {other:?}")
                }
            })
            .collect()
    }

    /// Every registered session is told, an unregistered one is not, and
    /// a forgotten one holds nothing.
    #[test]
    fn each_registered_session_is_owed_the_disconnect() {
        let notices = SessionNotices::default();
        notices.register("a", None);
        notices.register("b", None);
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
        notices.register("a", None);
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
        let notices = SessionNotices::default();
        notices.register("s", None);
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
        let notices = SessionNotices::default();
        notices.register("s", None);
        let peers: Vec<_> = (0..=MAX_PEER_NOTICES).map(|_| peer()).collect();
        for p in &peers {
            notices.disconnected(p, DisconnectReason::Closed);
        }
        let held = gone(&notices.take("s", usize::MAX));
        assert_eq!(held.len(), MAX_PEER_NOTICES, "never past the bound");
        assert_eq!(
            notices.diagnostics().evicted_total,
            1,
            "and the one dropped is counted"
        );
        assert_eq!(held[0].0, peers[1], "the oldest went");
        assert_eq!(held[MAX_PEER_NOTICES - 1].0, peers[MAX_PEER_NOTICES]);
    }

    fn summary(verified: bool, updated_at: u64) -> ConnectivitySummary {
        ConnectivitySummary {
            direct_inbound: if verified {
                DirectInboundState::VerifiedPublic
            } else {
                DirectInboundState::Unknown
            },
            relay_inbound: PathReadiness::Unavailable,
            active_relay_reservations: 0,
            target_relay_reservations: 0,
            active_relayed_peer_paths: 0,
            hole_punch_inflight: 0,
            preferred_path_policy: PreferredPathPolicy::DirectFirst,
            updated_at,
        }
    }

    fn healths(taken: &[LocalSessionEvent]) -> Vec<Health> {
        taken
            .iter()
            .filter_map(|e| match e {
                LocalSessionEvent::ServerState { health, .. } => Some(*health),
                _ => None,
            })
            .collect()
    }

    /// The state is owed on register when one is known, and held as the
    /// newest only: three changes unread are one notice, the last. A
    /// re-read differing only in its timestamp is not a change.
    #[test]
    fn the_state_is_owed_on_register_and_coalesced_to_the_newest() {
        let notices = SessionNotices::default();
        notices.register("early", None);
        assert!(
            notices.take("early", usize::MAX).is_empty(),
            "none known yet"
        );
        assert!(notices.server_state(Health::Healthy, Some(summary(false, 1))));
        assert!(
            !notices.server_state(Health::Healthy, Some(summary(false, 2))),
            "only the timestamp moved"
        );
        notices.register("late", None);
        assert_eq!(
            healths(&notices.take("late", usize::MAX)),
            [Health::Healthy]
        );
        assert!(notices.server_state(Health::Degraded, Some(summary(false, 3))));
        assert!(notices.server_state(Health::Degraded, Some(summary(true, 4))));
        assert!(notices.server_state(Health::Unavailable, Some(summary(true, 5))));
        let taken = notices.take("early", usize::MAX);
        assert_eq!(healths(&taken), [Health::Unavailable], "{taken:?}");
        assert!(notices.take("early", usize::MAX).is_empty(), "taken once");
    }

    /// The state comes before the disconnects, under the same `max`.
    #[test]
    fn the_state_is_taken_first_under_max() {
        let notices = SessionNotices::default();
        notices.register("s", None);
        let p = peer();
        notices.disconnected(&p, DisconnectReason::Closed);
        notices.server_state(Health::Healthy, None);
        assert!(notices.take("s", 0).is_empty());
        assert_eq!(healths(&notices.take("s", 1)), [Health::Healthy]);
        assert_eq!(gone(&notices.take("s", 1)), [(p, "closed".to_owned())]);
    }

    /// `ready` is what a take would return here, or the end; each post
    /// wakes the sessions it is owed to, and a delivery wakes only the
    /// sessions whose lease names its endpoint.
    #[test]
    fn posts_wake_their_sessions_and_ready_says_what_is_owed() {
        // One poll with no waker: ready only on a stored permit, which
        // the poll consumes.
        let woken = |wake: &std::sync::Arc<tokio::sync::Notify>| {
            let mut notified = std::pin::pin!(wake.notified());
            std::future::Future::poll(
                notified.as_mut(),
                &mut std::task::Context::from_waker(std::task::Waker::noop()),
            )
            .is_ready()
        };
        let human = EndpointId::parse("human").expect("endpoint");
        let notices = SessionNotices::default();
        let a = notices.register("a", Some(human.clone()));
        let b = notices.register("b", None);
        assert!(!notices.ready("a") && !woken(&a) && !woken(&b));

        notices.delivered_to(&human);
        assert!(woken(&a) && !woken(&b), "only the lease's holder");
        assert!(
            !notices.ready("a"),
            "a delivery is the substrate's to count"
        );
        notices.wake("b");
        assert!(woken(&b) && !woken(&a));

        notices.disconnected(&peer(), DisconnectReason::Closed);
        assert!(woken(&a) && woken(&b));
        assert!(notices.ready("a") && notices.ready("b"));
        notices.take("a", usize::MAX);
        assert!(!notices.ready("a") && notices.ready("b"));

        notices.server_state(Health::Healthy, None);
        assert!(woken(&a) && notices.ready("a"));
        notices.take("a", usize::MAX);
        assert!(!notices.ready("a"));

        notices.end();
        assert!(woken(&a) && notices.ready("a") && notices.ready("never"));
    }
}
