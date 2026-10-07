// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! An in-memory binding of the four neutral traits (plan §17 (2)): two
//! nodes wired to each other by [`FakeNetwork::pair`], each with its own
//! endpoint table, lease table and per-session queues, and scripted
//! faults.
//!
//! WHAT IT PROVES AND WHAT IT CANNOT. It passes the same generic
//! functions `tests/local-client-conformance` runs against the real
//! bindings (`tests/fake.rs` there), so every local-semantics claim a
//! client relies on -- leases, epochs, queues, notices, the admin overlay
//! -- holds here as it does there. What a fake cannot honour is ASSERTED,
//! not proved: the peer identity a message carries is the configured one
//! ("Noise proved the peer" is configuration here), the two nodes trust
//! each other from pairing until an administrator's `set_trust` revokes
//! it, and the fake does not produce `Timeout` itself: a client sees it
//! via [`FakeNode::inject_send`]. A revocation cuts the pair in both
//! directions, as the runtime's closing of the connections does: the
//! revoking node's sends and queries to the peer are `UnauthorizedPeer`,
//! the peer's to it `PeerUnreachable`, and no broadcast crosses either
//! way; the revocation's `PeerDisconnected` with the `policy` reason is
//! owed to every session of the revoking node -- the peer's own sessions
//! are told nothing, where the runtime would report the closed
//! connection as `closed`. Two more the fake produces from its own
//! state: `PeerUnreachable` when the
//! other node is dropped or stopped, and `RemoteEndpointUnavailable` when
//! the destination is unknown, disabled or unleased (or no default is
//! configured). So a client tested against it is proved to handle every
//! outcome, not that the network produces them -- reachability stays with
//! `tests/direct-v2` and the end-to-end suites.
//!
//! Every lock is a `std::sync::Mutex` held for one synchronous step and
//! never across an await, and one node's lock is never held while taking
//! the other's.

#![expect(
    clippy::unused_async_trait_impl,
    reason = "the traits are async for the bindings that cross a connection; this one is in memory"
)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::task::{Poll, Waker};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use interweave_local_client_api::{
    AdminBinding, AdminCapability, AdminPort, AdminStatus, DataCapability, DataSessionBinding,
    DataSessionPort, EndpointAdminView, EndpointLease, Generation, LeaseRecord, LocalAdminPort,
    LocalDataSession, LocalSessionEvent, MAX_EVENT_QUEUE, PeerGateView, PeerOutcome,
    ReceivedBroadcast, ReceivedDirect, SessionEvent, SessionRequest, TrustAdminView, TrustSource,
    TrustedPeer,
};
use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, ConnectivitySummary, DirectDestination, DirectInboundState,
    EndpointDirectoryV1, EndpointId, Health, MessageId, PathReadiness, Payload, PeerPath,
    PreferredPathPolicy, TransportError, TransportIdentity,
};

/// The freshness a directory answer carries, as the real runtime's
/// default clamp gives it: long enough that a test reads it as fresh.
pub const DIRECTORY_TTL_MS: u32 = 60_000;

/// One configured endpoint, as a profile would declare it.
#[derive(Debug, Clone)]
pub struct FakeEndpoint {
    /// The endpoint.
    pub id: EndpointId,
    /// Whether it may be claimed and receives traffic.
    pub enabled: bool,
    /// Whether a directory query lists it while it is leased.
    pub advertise: bool,
    /// The client kinds that may claim it; `None` admits every kind.
    pub allowed_client_kinds: Option<BTreeSet<String>>,
}

impl FakeEndpoint {
    /// An enabled endpoint any kind may claim.
    #[must_use]
    pub const fn open(id: EndpointId, advertise: bool) -> Self {
        Self {
            id,
            enabled: true,
            advertise,
            allowed_client_kinds: None,
        }
    }
}

/// One node's configuration: what a profile would give the runtime.
#[derive(Debug, Clone)]
pub struct FakeConfig {
    /// This node's identity, carried on everything it sends.
    pub peer: TransportIdentity,
    /// Its configured endpoints.
    pub endpoints: Vec<FakeEndpoint>,
    /// The endpoint that receives a directed send naming none.
    pub default_endpoint: Option<EndpointId>,
    /// Every session queue's bound: direct, broadcast and notices alike.
    pub queue_bound: usize,
}

/// The pair constructor.
pub struct FakeNetwork;

impl FakeNetwork {
    /// Two nodes, each the other's only peer, trusting each other.
    ///
    /// # Panics
    /// If a configuration's `queue_bound` is zero or over
    /// [`MAX_EVENT_QUEUE`], which no session may carry -- refused here so
    /// no `open` fails after taking its lease.
    #[must_use]
    pub fn pair(a: FakeConfig, b: FakeConfig) -> (FakeNode, FakeNode) {
        for bound in [a.queue_bound, b.queue_bound] {
            assert!(
                (1..=MAX_EVENT_QUEUE).contains(&bound),
                "a session queue holds 1 to {MAX_EVENT_QUEUE} events, not {bound}"
            );
        }
        let a = Arc::new(Node::new(a, "a"));
        let b = Arc::new(Node::new(b, "b"));
        *lock(&a.remote) = Arc::downgrade(&b);
        *lock(&b.remote) = Arc::downgrade(&a);
        lock(&a.state).trusted.insert(b.peer.clone());
        lock(&b.state).trusted.insert(a.peer.clone());
        (FakeNode(a), FakeNode(b))
    }
}

/// One node of a pair: a [`DataSessionBinding`] and an [`AdminBinding`].
#[derive(Clone)]
pub struct FakeNode(Arc<Node>);

impl FakeNode {
    /// This node's identity.
    #[must_use]
    pub fn peer(&self) -> &TransportIdentity {
        &self.0.peer
    }

    /// The next `send_direct` from any session of this node answers
    /// `error` instead of reaching the other node: one per call, in the
    /// order injected. How a client is shown the network outcomes a fake
    /// cannot produce.
    pub fn inject_send(&self, error: TransportError) {
        lock(&self.0.injected).push_back(error);
    }

    /// The runtime has stopped: every call from now answers
    /// `BackendUnavailable`.
    pub fn stop(&self) {
        let mut state = lock(&self.0.state);
        state.stopped = true;
        // No session waits on a stopped runtime.
        for queues in state.sessions.values_mut() {
            queues.wake();
        }
    }

    /// `peer`'s path changed: owed to each session with a route to it,
    /// merged into its pending notice -- the pending `previous` kept, the
    /// newer `current`, class and time taken; one that comes back to its
    /// `previous` is withdrawn. How a test drives what a real runtime
    /// reports.
    pub fn path_changed(
        &self,
        peer: &TransportIdentity,
        previous: PeerPath,
        current: PeerPath,
        reason_class: &str,
        observed_at: u64,
    ) {
        let mut state = lock(&self.0.state);
        for queues in state.sessions.values_mut() {
            if !queues.routes.contains(peer) {
                continue;
            }
            let merged = queues
                .paths
                .remove(peer)
                .map_or(previous, |(pending, ..)| pending);
            if merged != current {
                queues.paths.insert(
                    peer.clone(),
                    (merged, current, reason_class.to_owned(), observed_at),
                );
            }
            queues.wake();
        }
    }

    /// The node's health is now `health`: each session is owed it as its
    /// one pending `ServerState`, replacing what it held, when it
    /// changed. How a test drives the state a real runtime computes.
    pub fn set_health(&self, health: Health) {
        let mut state = lock(&self.0.state);
        if state.health == health {
            return;
        }
        state.health = health;
        for queues in state.sessions.values_mut() {
            queues.state = Some(state_event(health));
            queues.wake();
        }
    }

    /// What the owner was asked by `admin.shutdown`, oldest first.
    #[must_use]
    pub fn shutdown_requests(&self) -> Vec<Duration> {
        lock(&self.0.state).shutdown_requests.clone()
    }

    /// Sessions open on this node: how a test watches a dropped session
    /// go.
    #[must_use]
    pub fn open_sessions(&self) -> usize {
        lock(&self.0.state).sessions.len()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

struct Node {
    peer: TransportIdentity,
    /// The other node of the pair. Weak, so a pair dropped is freed.
    remote: Mutex<Weak<Node>>,
    state: Mutex<State>,
    injected: Mutex<VecDeque<TransportError>>,
    /// Fresh generations: a node tag and a counter, so an epoch or a
    /// session id is never issued twice by either node.
    tag: &'static str,
    next: AtomicU64,
}

#[derive(Debug, Clone)]
struct Lease {
    epoch: Generation,
    session: Generation,
    client_kind: String,
}

#[derive(Default)]
struct Queues {
    /// The node's state, the newest only.
    state: Option<LocalSessionEvent>,
    notices: VecDeque<LocalSessionEvent>,
    direct: VecDeque<ReceivedDirect>,
    broadcast: VecDeque<ReceivedBroadcast>,
    joins: BTreeSet<ChannelId>,
    /// The peers this session has a route to: a direct message it took or
    /// sent and had accepted, a broadcast it took -- taken, not merely
    /// queued, as the runtime records them. Unbounded and
    /// uncounted, unlike the runtime's (`MAX_ROUTED_PEERS`): a fake node
    /// has exactly one other peer, its pair, so neither set grows past
    /// one entry.
    routes: BTreeSet<TransportIdentity>,
    /// One pending path notice per routed peer, merged as the runtime
    /// merges them; the merges are not counted here.
    paths: BTreeMap<TransportIdentity, (PeerPath, PeerPath, String, u64)>,
    /// Every `ready` waiting on this session -- it takes `&self`, so
    /// there may be several -- woken by what is queued, and all of them.
    wakers: Vec<Waker>,
}

impl Queues {
    fn wake(&mut self) {
        for waker in self.wakers.drain(..) {
            waker.wake();
        }
    }

    fn holds_anything(&self) -> bool {
        self.state.is_some()
            || !self.notices.is_empty()
            || !self.direct.is_empty()
            || !self.broadcast.is_empty()
            || !self.paths.is_empty()
    }
}

/// The summary the fake reports: nothing measured, as a node with no
/// connectivity behaviour configured reads.
fn summary() -> ConnectivitySummary {
    ConnectivitySummary {
        direct_inbound: DirectInboundState::Unknown,
        relay_inbound: PathReadiness::Unavailable,
        active_relay_reservations: 0,
        target_relay_reservations: 0,
        active_relayed_peer_paths: 0,
        hole_punch_inflight: 0,
        preferred_path_policy: PreferredPathPolicy::DirectFirst,
        updated_at: wall_ms(),
    }
}

fn state_event(health: Health) -> LocalSessionEvent {
    LocalSessionEvent::ServerState {
        health,
        connectivity: Some(summary()),
    }
}

struct State {
    endpoints: BTreeMap<EndpointId, FakeEndpoint>,
    default: Option<EndpointId>,
    leases: BTreeMap<EndpointId, Lease>,
    sessions: BTreeMap<Generation, Queues>,
    queue_bound: usize,
    stopped: bool,
    shutdown_requests: Vec<Duration>,
    /// What [`FakeNode::set_health`] last set.
    health: Health,
    /// The data-plane allowlist: the pair's other node from pairing, then
    /// what the admin port's `set_trust` makes of it.
    trusted: BTreeSet<TransportIdentity>,
}

impl Node {
    fn new(config: FakeConfig, tag: &'static str) -> Self {
        Self {
            peer: config.peer,
            remote: Mutex::new(Weak::new()),
            state: Mutex::new(State {
                endpoints: config
                    .endpoints
                    .into_iter()
                    .map(|e| (e.id.clone(), e))
                    .collect(),
                default: config.default_endpoint,
                leases: BTreeMap::new(),
                sessions: BTreeMap::new(),
                queue_bound: config.queue_bound,
                stopped: false,
                shutdown_requests: Vec::new(),
                health: Health::Healthy,
                trusted: BTreeSet::new(),
            }),
            injected: Mutex::new(VecDeque::new()),
            tag,
            next: AtomicU64::new(1),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "`fake-<tag>-<what>-<16 digits>` is inside the generation grammar by construction"
    )]
    fn fresh(&self, what: &str) -> Generation {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        Generation::parse(format!("fake-{}-{what}-{n:016}", self.tag))
            .expect("inside the generation grammar")
    }

    fn running(&self) -> Result<MutexGuard<'_, State>, TransportError> {
        let state = lock(&self.state);
        if state.stopped {
            Err(TransportError::BackendUnavailable)
        } else {
            Ok(state)
        }
    }

    /// This node as the FAR end sees it: stopped is the peer gone, never
    /// the sender's own `BackendUnavailable`, which says its runtime
    /// stopped.
    fn reachable(&self) -> Result<MutexGuard<'_, State>, TransportError> {
        self.running().map_err(|_| TransportError::PeerUnreachable)
    }

    fn remote(&self) -> Result<Arc<Self>, TransportError> {
        lock(&self.remote)
            .upgrade()
            .ok_or(TransportError::PeerUnreachable)
    }

    /// The pair's other node, refused as the runtime refuses a peer it
    /// does not trust (a revocation by `set_trust`).
    fn trusted_remote(&self) -> Result<Arc<Self>, TransportError> {
        let remote = self.remote()?;
        if self.running()?.trusted.contains(&remote.peer) {
            Ok(remote)
        } else {
            Err(TransportError::UnauthorizedPeer)
        }
    }

    /// End `endpoint`'s lease as an administrative act: its holder is
    /// owed the epoch that ended, and what waited on its queue goes.
    fn revoke(state: &mut State, endpoint: &EndpointId) -> Option<Generation> {
        let lease = state.leases.remove(endpoint)?;
        let bound = state.queue_bound;
        if let Some(queues) = state.sessions.get_mut(&lease.session) {
            queues.direct.clear();
            if queues.notices.len() >= bound {
                queues.notices.pop_front();
            }
            queues
                .notices
                .push_back(LocalSessionEvent::EndpointLeaseChanged {
                    endpoint: endpoint.clone(),
                    revoked_epoch: lease.epoch.clone(),
                });
            queues.wake();
        }
        Some(lease.epoch)
    }

    /// Admit a direct message to the lease holder of its destination, as
    /// the receiving runtime does: resolve the endpoint (explicit or the
    /// default), require it enabled and leased, then room in the holder's
    /// queue. A refusal is the coarse remote class the wire carries.
    fn admit_direct(
        &self,
        source_peer: &TransportIdentity,
        source_endpoint: &EndpointId,
        destination: Option<EndpointId>,
        message_id: MessageId,
        payload: Payload,
    ) -> Result<EndpointId, TransportError> {
        let mut state = self.reachable()?;
        // A peer this node revoked is one whose connections it closed:
        // the sender reaches nothing, as it would not over the network.
        if !state.trusted.contains(source_peer) {
            return Err(TransportError::PeerUnreachable);
        }
        let endpoint = destination
            .or_else(|| state.default.clone())
            .ok_or(TransportError::RemoteEndpointUnavailable)?;
        let enabled = state.endpoints.get(&endpoint).is_some_and(|e| e.enabled);
        let holder = state.leases.get(&endpoint).map(|l| l.session.clone());
        let (true, Some(holder)) = (enabled, holder) else {
            return Err(TransportError::RemoteEndpointUnavailable);
        };
        let bound = state.queue_bound;
        let queues = state
            .sessions
            .get_mut(&holder)
            .ok_or(TransportError::RemoteEndpointUnavailable)?;
        if queues.direct.len() >= bound {
            return Err(TransportError::Overloaded);
        }
        queues.direct.push_back(ReceivedDirect {
            source_peer: source_peer.clone(),
            source_endpoint: source_endpoint.clone(),
            destination_endpoint: endpoint.clone(),
            message_id,
            payload,
            received_at_ms: wall_ms(),
        });
        queues.wake();
        Ok(endpoint)
    }

    /// Deliver a broadcast to every session on this node that joined its
    /// channel. A full queue gives up its oldest broadcast, as the real
    /// session queue does: a publisher is never refused for a slow reader.
    fn deliver_broadcast(
        &self,
        source_peer: &TransportIdentity,
        channel: &ChannelId,
        message: &BroadcastMessageV1,
    ) {
        let Ok(mut state) = self.running() else {
            return;
        };
        // Nor does a revoked peer's broadcast arrive.
        if !state.trusted.contains(source_peer) {
            return;
        }
        let bound = state.queue_bound;
        for queues in state.sessions.values_mut() {
            if !queues.joins.contains(channel) {
                continue;
            }
            if queues.broadcast.len() >= bound {
                queues.broadcast.pop_front();
            }
            queues.broadcast.push_back(ReceivedBroadcast {
                source_peer: source_peer.clone(),
                channel: channel.clone(),
                message_id: message.message_id,
                payload: message.payload.clone(),
                received_at_ms: wall_ms(),
            });
            queues.wake();
        }
    }
}

impl DataSessionBinding for FakeNode {
    type Session = FakeSession;

    async fn open(&self, request: SessionRequest) -> Result<FakeSession, TransportError> {
        let node = &self.0;
        let session_id = node.fresh("session");
        let mut state = node.running()?;
        let lease = match request.endpoint() {
            None => None,
            Some(endpoint) => {
                let configured = state
                    .endpoints
                    .get(endpoint)
                    .ok_or(TransportError::EndpointUnknown)?;
                if !configured.enabled {
                    return Err(TransportError::EndpointDisabled);
                }
                if configured
                    .allowed_client_kinds
                    .as_ref()
                    .is_some_and(|kinds| !kinds.contains(request.client_kind()))
                {
                    return Err(TransportError::EndpointClientKindDenied);
                }
                if state.leases.contains_key(endpoint) {
                    return Err(TransportError::EndpointInUse);
                }
                Some(EndpointLease {
                    endpoint: endpoint.clone(),
                    epoch: node.fresh("epoch"),
                })
            }
        };
        // The session is built BEFORE the lease is recorded, so a refusal
        // here leaves no lease held by a session that never opened.
        let record = lease
            .as_ref()
            .map(|l| (l.endpoint.clone(), l.epoch.clone()));
        let session = LocalDataSession::new(
            session_id.clone(),
            self.0.peer.clone(),
            request.client_kind(),
            lease,
            request.capabilities().iter().copied(),
            state.queue_bound,
        )
        .map_err(|_| TransportError::InvalidArgument)?;
        if let Some((endpoint, epoch)) = record {
            state.leases.insert(
                endpoint,
                Lease {
                    epoch,
                    session: session_id.clone(),
                    client_kind: request.client_kind().to_owned(),
                },
            );
        }
        // Owed the node's state from the start, as a connection is sent
        // it on connect.
        let owed = Queues {
            state: Some(state_event(state.health)),
            ..Queues::default()
        };
        state.sessions.insert(session_id, owed);
        Ok(FakeSession {
            node: Arc::clone(node),
            session,
            closed: false,
        })
    }
}

/// One open session of a [`FakeNode`]. Dropping it ends it, as `close`
/// does: its lease released, its joins left.
pub struct FakeSession {
    node: Arc<Node>,
    session: LocalDataSession,
    closed: bool,
}

impl FakeSession {
    fn require(&self, capability: DataCapability) -> Result<(), TransportError> {
        if self.session.holds(capability) {
            Ok(())
        } else {
            Err(TransportError::CapabilityDenied)
        }
    }

    /// Whether this session's lease is still the live one: a revoked or
    /// replaced lease sends nothing.
    fn lease_is_live(state: &State, lease: &EndpointLease) -> bool {
        state
            .leases
            .get(&lease.endpoint)
            .is_some_and(|live| live.epoch == lease.epoch)
    }

    fn end(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        let mut state = lock(&self.node.state);
        let id = self.session.session_id();
        state.leases.retain(|_, lease| &lease.session != id);
        state.sessions.remove(id);
    }
}

impl Drop for FakeSession {
    fn drop(&mut self) {
        self.end();
    }
}

impl DataSessionPort for FakeSession {
    fn session(&self) -> &LocalDataSession {
        &self.session
    }

    async fn join(&self, channel: ChannelId) -> Result<(), TransportError> {
        self.require(DataCapability::Commands)?;
        let mut state = self.node.running()?;
        let queues = state
            .sessions
            .get_mut(self.session.session_id())
            .ok_or(TransportError::BackendUnavailable)?;
        queues.joins.insert(channel);
        Ok(())
    }

    async fn leave(&self, channel: ChannelId) -> Result<(), TransportError> {
        self.require(DataCapability::Commands)?;
        let mut state = self.node.running()?;
        if let Some(queues) = state.sessions.get_mut(self.session.session_id()) {
            queues.joins.remove(&channel);
        }
        Ok(())
    }

    async fn broadcast(
        &self,
        channel: ChannelId,
        message: BroadcastMessageV1,
    ) -> Result<(), TransportError> {
        self.require(DataCapability::Commands)?;
        {
            let state = self.node.running()?;
            let joined = state
                .sessions
                .get(self.session.session_id())
                .is_some_and(|q| q.joins.contains(&channel));
            if !joined {
                return Err(TransportError::ChannelNotJoined);
            }
        }
        // Accepted locally; the other node delivers it to its joined
        // sessions, and the publisher's own node does not echo it.
        if let Ok(remote) = self.node.trusted_remote() {
            remote.deliver_broadcast(&self.node.peer, &channel, &message);
        }
        Ok(())
    }

    async fn send_direct(
        &self,
        destination: DirectDestination,
        message_id: MessageId,
        payload: Payload,
    ) -> Result<EndpointId, TransportError> {
        let source = self.session.authorize_direct_send()?.clone();
        let Some(lease) = self.session.endpoint_lease() else {
            return Err(TransportError::EndpointNotRegistered);
        };
        let live = {
            let state = self.node.running()?;
            Self::lease_is_live(&state, lease)
        };
        if !live {
            return Err(TransportError::EndpointNotRegistered);
        }
        if let Some(injected) = lock(&self.node.injected).pop_front() {
            return Err(injected);
        }
        let remote = self.node.remote()?;
        if destination.peer != remote.peer {
            return Err(TransportError::PeerUnknown);
        }
        let remote = self.node.trusted_remote()?;
        let accepted = remote.admit_direct(
            &self.node.peer,
            &source,
            destination.endpoint,
            message_id,
            payload,
        );
        if accepted.is_ok()
            && let Some(queues) = lock(&self.node.state)
                .sessions
                .get_mut(self.session.session_id())
        {
            queues.routes.insert(destination.peer);
        }
        accepted
    }

    async fn events(&self, max: usize) -> Result<Vec<SessionEvent>, TransportError> {
        self.require(DataCapability::Events)?;
        let mut state = self.node.running()?;
        let Some(queues) = state.sessions.get_mut(self.session.session_id()) else {
            return Ok(Vec::new());
        };
        let mut taken = Vec::new();
        if max > 0
            && let Some(state) = queues.state.take()
        {
            taken.push(SessionEvent::Local(state));
        }
        while taken.len() < max {
            if let Some(notice) = queues.notices.pop_front() {
                taken.push(SessionEvent::Local(notice));
            } else if let Some(direct) = queues.direct.pop_front() {
                // A message TAKEN is a route, as the runtime records it
                // (`SessionNotices::drained_from`): not one merely queued.
                queues.routes.insert(direct.source_peer.clone());
                taken.push(SessionEvent::Direct(direct));
            } else if let Some(broadcast) = queues.broadcast.pop_front() {
                queues.routes.insert(broadcast.source_peer.clone());
                taken.push(SessionEvent::Broadcast(broadcast));
            } else if let Some((peer, (previous, current, reason_class, observed_at))) =
                queues.paths.pop_first()
            {
                // The ordinary lane, after every message.
                taken.push(SessionEvent::Local(LocalSessionEvent::PeerPathChanged {
                    peer,
                    previous,
                    current,
                    reason_class,
                    observed_at,
                }));
            } else {
                break;
            }
        }
        Ok(taken)
    }

    async fn ready(&self) -> Result<(), TransportError> {
        self.require(DataCapability::Events)?;
        std::future::poll_fn(|cx| {
            let mut state = lock(&self.node.state);
            if state.stopped {
                return Poll::Ready(Ok(()));
            }
            match state.sessions.get_mut(self.session.session_id()) {
                Some(queues) if !queues.holds_anything() => {
                    // One entry per waiting task: a task polled again
                    // replaces its own rather than adding one.
                    queues.wakers.retain(|w| !w.will_wake(cx.waker()));
                    queues.wakers.push(cx.waker().clone());
                    Poll::Pending
                }
                // Something waits, or the session has gone.
                _ => Poll::Ready(Ok(())),
            }
        })
        .await
    }

    async fn query_endpoints(
        &self,
        peer: TransportIdentity,
    ) -> Result<EndpointDirectoryV1, TransportError> {
        self.require(DataCapability::EndpointsQuery)?;
        drop(self.node.running()?);
        let remote = self.node.remote()?;
        if peer != remote.peer {
            return Err(TransportError::PeerUnknown);
        }
        let remote = self.node.trusted_remote()?;
        let state = remote.reachable()?;
        if !state.trusted.contains(&self.node.peer) {
            return Err(TransportError::PeerUnreachable);
        }
        let endpoints = state
            .endpoints
            .values()
            .filter(|e| e.enabled && e.advertise && state.leases.contains_key(&e.id))
            .map(|e| e.id.clone())
            .collect();
        Ok(EndpointDirectoryV1 {
            generated_at_ms: wall_ms(),
            ttl_ms: DIRECTORY_TTL_MS,
            endpoints,
        })
    }

    async fn close(mut self) -> Result<(), TransportError> {
        let stopped = lock(&self.node.state).stopped;
        self.end();
        if stopped {
            Err(TransportError::BackendUnavailable)
        } else {
            Ok(())
        }
    }
}

impl AdminBinding for FakeNode {
    type Admin = FakeAdmin;

    async fn admin(
        &self,
        capabilities: BTreeSet<AdminCapability>,
    ) -> Result<FakeAdmin, TransportError> {
        drop(self.0.running()?);
        Ok(FakeAdmin {
            node: Arc::clone(&self.0),
            port: LocalAdminPort::new(self.0.fresh("admin"), capabilities),
        })
    }
}

/// One open administrative port of a [`FakeNode`]. It never holds a lease.
pub struct FakeAdmin {
    node: Arc<Node>,
    port: LocalAdminPort,
}

impl FakeAdmin {
    fn require(&self, capability: AdminCapability) -> Result<(), TransportError> {
        if self.port.holds(capability) {
            Ok(())
        } else {
            Err(TransportError::CapabilityDenied)
        }
    }
}

impl AdminPort for FakeAdmin {
    fn port(&self) -> &LocalAdminPort {
        &self.port
    }

    async fn status(&self) -> Result<AdminStatus, TransportError> {
        self.require(AdminCapability::Status)?;
        let state = self.node.running()?;
        Ok(AdminStatus {
            health: state.health,
            peer: self.node.peer.clone(),
            connectivity: summary(),
            active_leases: state.leases.len(),
            pre_auth: None,
            ingress: None,
        })
    }

    async fn leases(&self) -> Result<Vec<EndpointAdminView>, TransportError> {
        self.require(AdminCapability::Endpoints)?;
        let state = self.node.running()?;
        Ok(state
            .endpoints
            .values()
            .map(|e| EndpointAdminView {
                endpoint: e.id.clone(),
                enabled: e.enabled,
                default: state.default.as_ref() == Some(&e.id),
                lease: state.leases.get(&e.id).map(|lease| LeaseRecord {
                    endpoint: e.id.clone(),
                    epoch: lease.epoch.clone(),
                    client_kind: lease.client_kind.clone(),
                    session_id: Some(lease.session.as_str().to_owned()),
                }),
            })
            .collect())
    }

    async fn revoke_endpoint(&self, endpoint: EndpointId) -> Result<(), TransportError> {
        self.require(AdminCapability::Endpoints)?;
        let mut state = self.node.running()?;
        Node::revoke(&mut state, &endpoint);
        Ok(())
    }

    async fn set_endpoint_enabled(
        &self,
        endpoint: EndpointId,
        enabled: bool,
    ) -> Result<Option<Generation>, TransportError> {
        self.require(AdminCapability::Endpoints)?;
        let mut state = self.node.running()?;
        let configured = state
            .endpoints
            .get_mut(&endpoint)
            .ok_or(TransportError::EndpointUnknown)?;
        configured.enabled = enabled;
        if enabled {
            return Ok(None);
        }
        // Disabling the default clears it, and enabling restores nothing.
        if state.default.as_ref() == Some(&endpoint) {
            state.default = None;
        }
        Ok(Node::revoke(&mut state, &endpoint))
    }

    async fn set_default_endpoint(
        &self,
        endpoint: Option<EndpointId>,
    ) -> Result<(), TransportError> {
        self.require(AdminCapability::Endpoints)?;
        let mut state = self.node.running()?;
        if let Some(endpoint) = &endpoint {
            let configured = state
                .endpoints
                .get(endpoint)
                .ok_or(TransportError::EndpointUnknown)?;
            if !configured.enabled {
                return Err(TransportError::EndpointDisabled);
            }
        }
        state.default = endpoint;
        Ok(())
    }

    async fn shutdown(&self, grace: Duration) -> Result<(), TransportError> {
        self.require(AdminCapability::Shutdown)?;
        let mut state = self.node.running()?;
        state.shutdown_requests.push(grace);
        Ok(())
    }

    async fn trust(&self) -> Result<TrustAdminView, TransportError> {
        self.require(AdminCapability::Trust)?;
        let state = self.node.running()?;
        Ok(TrustAdminView {
            local_peer: Some(self.node.peer.clone()),
            allowed: state
                .trusted
                .iter()
                .map(|peer| TrustedPeer {
                    peer: peer.clone(),
                    persisted: true,
                    source: TrustSource::Configured,
                })
                .collect(),
        })
    }

    /// One row per trusted peer, in order. The fake has no dial gate, so
    /// nothing is ever in backoff or quarantined; the paired node, while
    /// trusted, reads connected.
    async fn peers(&self) -> Result<Vec<PeerGateView>, TransportError> {
        self.require(AdminCapability::Status)?;
        let paired = self.node.remote().ok().map(|remote| remote.peer.clone());
        let state = self.node.running()?;
        Ok(state
            .trusted
            .iter()
            .map(|peer| {
                let connected = paired.as_ref() == Some(peer);
                PeerGateView {
                    peer: peer.clone(),
                    connected,
                    backoff_until: None,
                    quarantined_until: None,
                    last_outcome: connected.then_some(PeerOutcome::Connected),
                }
            })
            .collect())
    }

    /// The refusals the runtime's policy makes (the local peer, a new
    /// peer at the ceiling) and its no-ops; revoking the paired node is
    /// owed to every session as `PeerDisconnected` with the `policy`
    /// reason, bounded as any notice.
    async fn set_trust(
        &self,
        peer: TransportIdentity,
        allowed: bool,
    ) -> Result<(), TransportError> {
        self.require(AdminCapability::Trust)?;
        // The only peer that can hold a connection here is the pair's
        // other node; a revoked peer that never could is told to nobody.
        let paired = self.node.remote().is_ok_and(|remote| remote.peer == peer);
        let mut state = self.node.running()?;
        if allowed {
            if peer == self.node.peer
                || (!state.trusted.contains(&peer) && state.trusted.len() >= MAX_ALLOWED_PEERS)
            {
                return Err(TransportError::InvalidArgument);
            }
            state.trusted.insert(peer);
            return Ok(());
        }
        if !state.trusted.remove(&peer) || !paired {
            return Ok(());
        }
        let bound = state.queue_bound;
        for queues in state.sessions.values_mut() {
            if queues.notices.len() >= bound {
                queues.notices.pop_front();
            }
            queues
                .notices
                .push_back(LocalSessionEvent::PeerDisconnected {
                    peer: peer.clone(),
                    reason_class: "policy".into(),
                });
            queues.wake();
        }
        Ok(())
    }
}

/// `PeerTrustPolicy::MAX_ALLOWED_PEERS`, which this crate does not depend
/// on: the same ceiling, so a client sees the same refusal.
const MAX_ALLOWED_PEERS: usize = 4096;
