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

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use interweave_local_client_api::LocalSessionEvent;
use interweave_transport_api::{
    ConnectivitySummary, DisconnectReason, EndpointId, Health, PeerPath, TransportIdentity,
};
use tokio::sync::Notify;

/// The most peer notices one session is owed. A notice for a peer
/// already owed one replaces it, so a session that reads holds at most
/// one per peer; past this many distinct peers unread, the oldest goes
/// (`a_sessions_notices_are_bounded_and_keep_the_newest`).
pub const MAX_PEER_NOTICES: usize = 64;

/// The most peers one session is held to have a route to. A route is a
/// data-plane exchange -- a direct message delivered to or accepted from
/// the session, a broadcast it received -- so its peers are trusted ones,
/// and the trust allowlist's own ceiling bounds them; past it a new route
/// is counted, not kept (`a_sessions_routes_are_bounded_and_counted`). A
/// revocation forgets the peer's routes ([`SessionNotices::revoked`]), so
/// the bound tracks the allowlist under churn rather than every peer a
/// session ever exchanged with (`a_revoked_peers_route_frees_its_place`).
/// A route made after the revocation is a new one -- a message from the
/// peer queued before it and drained after, or a send accepted just
/// before it -- and lasts until the peer is allowed and revoked again or
/// the session ends, held by the bound meanwhile.
pub const MAX_ROUTED_PEERS: usize = interweave_trust_api::PeerTrustPolicy::MAX_ALLOWED_PEERS;

/// One pending path notice, before it is taken.
#[derive(Clone)]
struct PathNotice {
    previous: PeerPath,
    current: PeerPath,
    reason_class: String,
    observed_at: u64,
}

/// One session's entry.
struct Owed {
    /// The runtime's state, the newest only: replaced, never queued
    /// (`the_state_is_owed_on_register_and_coalesced_to_the_newest`).
    state: Option<LocalSessionEvent>,
    peers: VecDeque<LocalSessionEvent>,
    /// The endpoint the session's lease names, whose deliveries wake it.
    endpoint: Option<EndpointId>,
    /// The peers this session has a route to, whose path changes it is
    /// owed; at most [`MAX_ROUTED_PEERS`].
    routes: BTreeSet<TransportIdentity>,
    /// One pending path notice per routed peer, merged
    /// (`a_path_change_is_coalesced_per_peer_and_a_round_trip_withdrawn`).
    paths: BTreeMap<TransportIdentity, PathNotice>,
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
    /// Path notices replaced or withdrawn by a newer change, or withdrawn
    /// by the peer's revocation, before they were taken, across every
    /// session (LOCAL-IPC.md §Push events' path-notice rule).
    paths_replaced: Arc<AtomicU64>,
    /// Routes not kept past [`MAX_ROUTED_PEERS`], across every session.
    routes_refused: Arc<AtomicU64>,
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
    /// Path notices a newer change replaced, or withdrew as no change, or
    /// the peer's revocation or disconnect withdrew, before they were taken.
    pub paths_replaced_total: u64,
    /// Routes refused past a session's [`MAX_ROUTED_PEERS`].
    pub routes_refused_total: u64,
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
                routes: BTreeSet::new(),
                paths: BTreeMap::new(),
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

    /// `peer` left the allowlist: no session has a route to it any more,
    /// and a path notice still pending for it is withdrawn and counted --
    /// its disconnect, owed apart, says what the client must know
    /// (`a_revoked_peers_route_frees_its_place`).
    pub(crate) fn revoked(&self, peer: &TransportIdentity) {
        for owed in self.registry().sessions.values_mut() {
            owed.routes.remove(peer);
            if owed.paths.remove(peer).is_some() {
                self.paths_replaced.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Owe every registered session `peer`'s disconnect, and withdraw its
    /// pending path notice, counted as a replacement: the path it names
    /// went with the connection, and a client taking it after the
    /// disconnect could not tell it from a change since a reconnect
    /// (rust-ui-dev, #191's review). The route is kept, so a change after
    /// a reconnect is owed (`a_disconnect_withdraws_a_pending_path_and_keeps_the_route`).
    pub(crate) fn disconnected(&self, peer: &TransportIdentity, reason: DisconnectReason) {
        for owed in self.registry().sessions.values_mut() {
            if owed.paths.remove(peer).is_some() {
                self.paths_replaced.fetch_add(1, Ordering::Relaxed);
            }
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

    /// Hold that `owed` has a route to `peer`, within the bound.
    fn route(&self, owed: &mut Owed, peer: &TransportIdentity) {
        if owed.routes.contains(peer) {
            return;
        }
        if owed.routes.len() >= MAX_ROUTED_PEERS {
            self.routes_refused.fetch_add(1, Ordering::Relaxed);
            return;
        }
        owed.routes.insert(peer.clone());
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

    /// `session` took messages from `peers`: it now has a route to each --
    /// a received message becomes a route when the session takes it, as
    /// LOCAL-CLIENT.md §2 states (A 2026-10-04). Recorded from what the
    /// session DRAINED, not from the substrate's
    /// delivery reports, which it drops under backpressure while the
    /// message itself stays queued -- a route recorded there could be
    /// lost with no count (#184 review F1;
    /// `a_drained_message_is_a_route_and_a_path_change_follows_it`).
    pub(crate) fn drained_from<'a>(
        &self,
        session: &str,
        peers: impl IntoIterator<Item = &'a TransportIdentity>,
    ) {
        if let Some(owed) = self.registry().sessions.get_mut(session) {
            for peer in peers {
                self.route(owed, peer);
            }
        }
    }

    /// `session` sent `peer` a direct message that was accepted: it now
    /// has a route to `peer` (LOCAL-CLIENT.md §2, A 2026-10-04: a sent
    /// direct message is a route at its acceptance).
    pub(crate) fn sent_to(&self, session: &str, peer: &TransportIdentity) {
        if let Some(owed) = self.registry().sessions.get_mut(session) {
            self.route(owed, peer);
        }
    }

    /// Wake `session`: something was queued for it by name.
    pub(crate) fn wake(&self, session: &str) {
        if let Some(owed) = self.registry().sessions.get(session) {
            owed.wake();
        }
    }

    /// `peer`'s path changed: owed to every session with a route to it,
    /// merged into its pending notice -- the pending `previous` kept, the
    /// newer `current`, class and time taken; one that comes back to its
    /// `previous` announces no change and is withdrawn. Each merge is
    /// counted.
    pub(crate) fn path_changed(
        &self,
        peer: &TransportIdentity,
        previous: PeerPath,
        current: PeerPath,
        reason_class: &str,
        observed_at: u64,
    ) {
        let mut registry = self.registry();
        for owed in registry.sessions.values_mut() {
            if !owed.routes.contains(peer) {
                continue;
            }
            let merged = match owed.paths.remove(peer) {
                Some(pending) => {
                    self.paths_replaced.fetch_add(1, Ordering::Relaxed);
                    pending.previous
                }
                None => previous,
            };
            if merged != current {
                owed.paths.insert(
                    peer.clone(),
                    PathNotice {
                        previous: merged,
                        current,
                        reason_class: reason_class.to_owned(),
                        observed_at,
                    },
                );
            }
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
            || registry.sessions.get(session).is_some_and(|owed| {
                owed.state.is_some() || !owed.peers.is_empty() || !owed.paths.is_empty()
            })
    }

    /// The registry's size and what its bound has dropped.
    pub(crate) fn diagnostics(&self) -> PeerNoticeDiagnostics {
        PeerNoticeDiagnostics {
            sessions: self.registry().sessions.len(),
            evicted_total: self.evicted.load(Ordering::Relaxed),
            paths_replaced_total: self.paths_replaced.load(Ordering::Relaxed),
            routes_refused_total: self.routes_refused.load(Ordering::Relaxed),
        }
    }

    /// Take at most `max` of `session`'s path notices, in the ordinary
    /// lane: after its messages, which `events` takes first.
    pub(crate) fn take_paths(&self, session: &str, max: usize) -> Vec<LocalSessionEvent> {
        let mut registry = self.registry();
        let Some(owed) = registry.sessions.get_mut(session) else {
            return Vec::new();
        };
        let peers: Vec<TransportIdentity> = owed.paths.keys().take(max).cloned().collect();
        peers
            .into_iter()
            .filter_map(|peer| {
                owed.paths
                    .remove(&peer)
                    .map(|n| LocalSessionEvent::PeerPathChanged {
                        peer,
                        previous: n.previous,
                        current: n.current,
                        reason_class: n.reason_class,
                        observed_at: n.observed_at,
                    })
            })
            .collect()
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
    use super::{MAX_PEER_NOTICES, MAX_ROUTED_PEERS, SessionNotices};
    use interweave_local_client_api::LocalSessionEvent;
    use interweave_profile_identity::ProfileIdentity;
    use interweave_transport_api::{
        ConnectivitySummary, DirectInboundState, DisconnectReason, EndpointId, Health,
        PathReadiness, PeerPath, PreferredPathPolicy, TransportIdentity,
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
                | LocalSessionEvent::ServerState { .. }
                | LocalSessionEvent::PeerPathChanged { .. }) => {
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

    /// `ready` takes `&self`, so several tasks may wait on one session:
    /// a wake reaches EVERY registered wait, not the first alone.
    #[test]
    fn every_concurrent_ready_ends_with_the_runtime() {
        let notices = SessionNotices::default();
        let wake = notices.register("s", None);
        let poll = |n: std::pin::Pin<&mut tokio::sync::futures::Notified<'_>>| {
            std::future::Future::poll(
                n,
                &mut std::task::Context::from_waker(std::task::Waker::noop()),
            )
            .is_ready()
        };
        let mut a = std::pin::pin!(wake.notified());
        let mut b = std::pin::pin!(wake.notified());
        a.as_mut().enable();
        b.as_mut().enable();
        assert!(!poll(a.as_mut()) && !poll(b.as_mut()), "both wait");
        notices.end();
        assert!(poll(a.as_mut()), "the first wait ends");
        assert!(poll(b.as_mut()), "and so does the second");
    }

    fn paths(events: &[LocalSessionEvent]) -> Vec<(PeerPath, PeerPath, String, u64)> {
        events
            .iter()
            .map(|e| match e {
                LocalSessionEvent::PeerPathChanged {
                    previous,
                    current,
                    reason_class,
                    observed_at,
                    ..
                } => (*previous, *current, reason_class.clone(), *observed_at),
                other => panic!("only path notices: {other:?}"),
            })
            .collect()
    }

    /// One pending notice per peer: a newer change keeps the pending
    /// `previous` and takes the newer `current`, class and time; a merge
    /// that comes back to its `previous` is withdrawn; each merge is
    /// counted. A session with no route to the peer is owed nothing, and
    /// the reserved-lane take never carries a path notice.
    #[test]
    fn a_path_change_is_coalesced_per_peer_and_a_round_trip_withdrawn() {
        use PeerPath::{Direct, Relayed};
        let notices = SessionNotices::default();
        notices.register("routed", None);
        notices.register("stranger", None);
        let p = peer();
        notices.sent_to("routed", &p);

        notices.path_changed(&p, Relayed, Direct, "dcutr", 1);
        notices.path_changed(&p, Direct, Relayed, "direct_lost", 2);
        assert!(
            !notices.ready("routed"),
            "relayed -> direct -> relayed is no change"
        );
        assert!(notices.take_paths("routed", usize::MAX).is_empty());
        assert_eq!(notices.diagnostics().paths_replaced_total, 1);

        notices.path_changed(&p, Relayed, Direct, "direct_established", 3);
        notices.path_changed(&p, Direct, Direct, "dcutr", 4);
        assert!(notices.ready("routed"));
        assert!(
            notices.take("routed", usize::MAX).is_empty(),
            "not the reserved lane"
        );
        assert_eq!(
            paths(&notices.take_paths("routed", usize::MAX)),
            [(Relayed, Direct, "dcutr".to_owned(), 4)],
            "the first previous, the newest current, class and time"
        );
        assert_eq!(notices.diagnostics().paths_replaced_total, 2);
        assert!(
            notices.take_paths("routed", usize::MAX).is_empty(),
            "taken once"
        );
        assert!(!notices.ready("stranger"), "no route, nothing owed");

        // A message the session drained is a route too, and a delivery
        // report alone is not: it only wakes.
        let human = EndpointId::parse("human").expect("endpoint");
        notices.register("reader", Some(human.clone()));
        let (q, r) = (peer(), peer());
        notices.delivered_to(&human);
        notices.drained_from("reader", [&q]);
        notices.path_changed(&q, Relayed, Direct, "dcutr", 5);
        notices.path_changed(&r, Relayed, Direct, "dcutr", 6);
        assert_eq!(
            paths(&notices.take_paths("reader", usize::MAX)).len(),
            1,
            "q's, drained; not r's, never exchanged"
        );
    }

    /// A session holds at most `MAX_ROUTED_PEERS` routes; one past it is
    /// counted and not kept, so its path changes are not owed.
    #[test]
    fn a_sessions_routes_are_bounded_and_counted() {
        let notices = SessionNotices::default();
        notices.register("s", None);
        let peers: Vec<_> = (0..=MAX_ROUTED_PEERS).map(|_| peer()).collect();
        for p in &peers {
            notices.sent_to("s", p);
        }
        notices.sent_to("s", &peers[0]);
        assert_eq!(
            notices.diagnostics().routes_refused_total,
            1,
            "the one past"
        );
        notices.path_changed(
            &peers[MAX_ROUTED_PEERS],
            PeerPath::Relayed,
            PeerPath::Direct,
            "dcutr",
            1,
        );
        assert!(!notices.ready("s"), "the refused route is owed nothing");
        notices.path_changed(&peers[0], PeerPath::Relayed, PeerPath::Direct, "dcutr", 2);
        assert!(notices.ready("s"), "a kept route is");
    }

    /// A revoked peer's route frees its place under the bound in EVERY
    /// session, and its pending path notice is withdrawn and counted; the
    /// other routes stand.
    #[test]
    fn a_revoked_peers_route_frees_its_place() {
        let notices = SessionNotices::default();
        notices.register("s", None);
        notices.register("t", None);
        let peers: Vec<_> = (0..MAX_ROUTED_PEERS).map(|_| peer()).collect();
        for p in &peers {
            notices.sent_to("s", p);
        }
        notices.sent_to("t", &peers[0]);
        notices.path_changed(&peers[0], PeerPath::Relayed, PeerPath::Direct, "dcutr", 1);
        assert!(
            notices.ready("s") && notices.ready("t"),
            "the control: a routed peer is owed in both"
        );
        notices.revoked(&peers[0]);
        assert!(
            !notices.ready("s") && !notices.ready("t"),
            "its pending notice is withdrawn in both"
        );
        assert_eq!(
            notices.diagnostics().paths_replaced_total,
            2,
            "and each withdrawal counted"
        );
        notices.path_changed(&peers[0], PeerPath::Direct, PeerPath::Relayed, "dcutr", 2);
        assert!(
            !notices.ready("s") && !notices.ready("t"),
            "and nothing more is owed for it"
        );
        let newcomer = peer();
        notices.sent_to("s", &newcomer);
        assert_eq!(
            notices.diagnostics().routes_refused_total,
            0,
            "the freed place is taken, not refused"
        );
        notices.path_changed(&peers[1], PeerPath::Relayed, PeerPath::Direct, "dcutr", 3);
        assert!(notices.ready("s"), "the other routes stand");
    }

    /// A disconnect withdraws the peer's pending path notice, counted, and
    /// leaves another peer's; the route stands, so a change after a
    /// reconnect is owed and is taken after the disconnect.
    #[test]
    fn a_disconnect_withdraws_a_pending_path_and_keeps_the_route() {
        let notices = SessionNotices::default();
        notices.register("s", None);
        let (gone, other) = (peer(), peer());
        notices.sent_to("s", &gone);
        notices.sent_to("s", &other);
        notices.path_changed(&gone, PeerPath::Direct, PeerPath::Relayed, "direct_lost", 1);
        notices.path_changed(&other, PeerPath::Relayed, PeerPath::Direct, "dcutr", 2);
        notices.disconnected(&gone, DisconnectReason::Policy);
        assert_eq!(notices.diagnostics().paths_replaced_total, 1, "counted");
        let taken = notices.take("s", usize::MAX);
        assert!(
            matches!(&taken[..], [LocalSessionEvent::PeerDisconnected { peer, .. }] if *peer == gone),
            "the disconnect is owed: {taken:?}"
        );
        assert_eq!(
            paths(&notices.take_paths("s", usize::MAX)),
            [(PeerPath::Relayed, PeerPath::Direct, "dcutr".to_owned(), 2)],
            "the control: the other peer's notice stays; the gone peer's is withdrawn"
        );
        notices.path_changed(&gone, PeerPath::Relayed, PeerPath::Direct, "dcutr", 3);
        assert_eq!(
            paths(&notices.take_paths("s", usize::MAX)),
            [(PeerPath::Relayed, PeerPath::Direct, "dcutr".to_owned(), 3)],
            "the route stands: a change after the reconnect is owed"
        );
    }
}
