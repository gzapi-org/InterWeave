// Copyright 2018 Parity Technologies (UK) Ltd.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the "Software"),
// to deal in the Software without restriction, including without limitation
// the rights to use, copy, modify, merge, publish, distribute, sublicense,
// and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

mod iface;
mod socket;
mod timer;

use std::{
    cmp,
    collections::{
        HashSet, VecDeque,
        hash_map::{Entry, HashMap},
    },
    convert::Infallible,
    fmt,
    future::Future,
    io,
    net::IpAddr,
    pin::Pin,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::Instant,
};

use futures::{Stream, StreamExt, channel::mpsc};
use if_watch::IfEvent;
use libp2p_core::{Endpoint, Multiaddr, transport::PortUse};
use libp2p_identity::PeerId;
use libp2p_swarm::{
    ConnectionDenied, ConnectionId, ListenAddresses, NetworkBehaviour, THandler, THandlerInEvent,
    THandlerOutEvent, ToSwarm, behaviour::FromSwarm, dummy,
};
use smallvec::SmallVec;

use self::iface::InterfaceState;
use crate::{
    Config,
    behaviour::{socket::AsyncSocket, timer::Builder},
};

/// INTERWEAVE PATCH (ADR-0053 rule 7): what the bounds dropped, shared so
/// it can be read outside the task that polls the behaviour. Counts only,
/// never an address.
#[derive(Debug, Default)]
pub struct DropCounts {
    records_evicted: AtomicU64,
    records_refused: AtomicU64,
    discovered_dropped: AtomicU64,
    packets_dropped: AtomicU64,
    queries_unanswered: AtomicU64,
    failures_dropped: AtomicU64,
}

impl DropCounts {
    /// Records evicted to make room when a bound of the store was hit
    /// (rule 2). Each is reported as expired unless the same batch added
    /// it, since a batch is reported netted: a record added and evicted
    /// within one batch is counted here and reported not at all.
    pub fn records_evicted(&self) -> u64 {
        self.records_evicted.load(Ordering::Relaxed)
    }
    /// Records refused because, within the bound they hit -- the peer
    /// bound or one peer's address bound -- they would have been the
    /// soonest to expire (rule 2).
    pub fn records_refused(&self) -> u64 {
        self.records_refused.load(Ordering::Relaxed)
    }
    /// Pairs an interface dropped because its queue was full (rule 2).
    pub fn discovered_dropped(&self) -> u64 {
        self.discovered_dropped.load(Ordering::Relaxed)
    }
    /// Packets an interface dropped because its send buffer was full
    /// (rule 2).
    pub fn packets_dropped(&self) -> u64 {
        self.packets_dropped.load(Ordering::Relaxed)
    }
    /// Queries not answered because the interface sent, or failed to
    /// send, the same answer less than a second before, or still holds it
    /// queued (rule 4).
    pub fn queries_unanswered(&self) -> u64 {
        self.queries_unanswered.load(Ordering::Relaxed)
    }
    /// `Event::InterfaceFailed` reports lost because the channel that
    /// carries them from an interface to the behaviour was full (rule 5).
    pub fn failures_dropped(&self) -> u64 {
        self.failures_dropped.load(Ordering::Relaxed)
    }

    pub(crate) fn count(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn discovered_dropped_counter(&self) -> &AtomicU64 {
        &self.discovered_dropped
    }
    pub(crate) fn packets_dropped_counter(&self) -> &AtomicU64 {
        &self.packets_dropped
    }
    pub(crate) fn queries_unanswered_counter(&self) -> &AtomicU64 {
        &self.queries_unanswered
    }
    pub(crate) fn failures_dropped_counter(&self) -> &AtomicU64 {
        &self.failures_dropped
    }
}

/// An abstraction to allow for compatibility with various async runtimes.
pub trait Provider: 'static {
    /// The Async Socket type.
    type Socket: AsyncSocket;
    /// The Async Timer type.
    type Timer: Builder + Stream;
    /// The IfWatcher type.
    type Watcher: Stream<Item = std::io::Result<IfEvent>> + fmt::Debug + Unpin;

    type TaskHandle: Abort;

    /// Create a new instance of the `IfWatcher` type.
    fn new_watcher() -> Result<Self::Watcher, std::io::Error>;

    #[track_caller]
    fn spawn(task: impl Future<Output = ()> + Send + 'static) -> Self::TaskHandle;
}

#[allow(unreachable_pub)] // Not re-exported.
pub trait Abort {
    fn abort(self);
}

/// The type of a [`Behaviour`] using the `tokio` implementation.
#[cfg(feature = "tokio")]
pub mod tokio {
    use std::future::Future;

    use if_watch::tokio::IfWatcher;
    use tokio::task::JoinHandle;

    use super::Provider;
    use crate::behaviour::{Abort, socket::tokio::TokioUdpSocket, timer::tokio::TokioTimer};

    #[doc(hidden)]
    pub enum Tokio {}

    impl Provider for Tokio {
        type Socket = TokioUdpSocket;
        type Timer = TokioTimer;
        type Watcher = IfWatcher;
        type TaskHandle = JoinHandle<()>;

        fn new_watcher() -> Result<Self::Watcher, std::io::Error> {
            IfWatcher::new()
        }

        fn spawn(task: impl Future<Output = ()> + Send + 'static) -> Self::TaskHandle {
            tokio::spawn(task)
        }
    }

    impl Abort for JoinHandle<()> {
        fn abort(self) {
            JoinHandle::abort(&self)
        }
    }

    pub type Behaviour = super::Behaviour<Tokio>;
}

/// A `NetworkBehaviour` for mDNS. Automatically discovers peers on the local network and adds
/// them to the topology.
#[derive(Debug)]
pub struct Behaviour<P>
where
    P: Provider,
{
    /// InterfaceState config.
    config: Config,

    /// Iface watcher.
    if_watch: P::Watcher,

    /// Handles to tasks running the mDNS queries.
    if_tasks: HashMap<IpAddr, P::TaskHandle>,

    query_response_receiver: mpsc::Receiver<(PeerId, Multiaddr, Instant)>,
    query_response_sender: mpsc::Sender<(PeerId, Multiaddr, Instant)>,

    /// List of nodes that we have discovered, the address, and when their TTL expires.
    ///
    /// Each combination of `PeerId` and `Multiaddr` can only appear once, but the same `PeerId`
    /// can appear multiple times.
    discovered_nodes: SmallVec<[(PeerId, Multiaddr, Instant); 8]>,

    /// Future that fires when the TTL of at least one node in `discovered_nodes` expires.
    ///
    /// `None` if `discovered_nodes` is empty.
    closest_expiration: Option<P::Timer>,

    /// The current set of listen addresses.
    ///
    /// This is shared across all interface tasks using an [`RwLock`].
    /// The [`Behaviour`] updates this upon new [`FromSwarm`]
    /// events where as [`InterfaceState`]s read from it to answer inbound mDNS queries.
    listen_addresses: Arc<RwLock<ListenAddresses>>,

    local_peer_id: PeerId,

    /// Pending behaviour events to be emitted.
    pending_events: VecDeque<ToSwarm<Event, Infallible>>,

    /// INTERWEAVE PATCH (ADR-0053 rule 2): how many records each peer
    /// holds in `discovered_nodes`, kept in step with it, so the peer and
    /// per-peer address bounds are read without a scan.
    peer_records: HashMap<PeerId, usize>,

    /// INTERWEAVE PATCH (ADR-0053 rules 5, 7): interface failures, from
    /// the interface tasks, and the shared drop counts.
    failure_receiver: mpsc::Receiver<(IpAddr, String)>,
    failure_sender: mpsc::Sender<(IpAddr, String)>,
    drop_counts: Arc<DropCounts>,
    /// INTERWEAVE PATCH (ADR-0053 rule 5): whether `WatcherFailed` has
    /// been reported since the watcher last worked.
    watcher_failed: bool,
    /// INTERWEAVE PATCH (ADR-0053 rule 5): the watcher returned `Err` on
    /// two consecutive polls and is no longer polled. Final for this
    /// behaviour: nothing here recovers it. Recovery is the runtime's
    /// rebuild of the behaviour (ADR-0053 rule 5), which is not built
    /// yet, so a dead watcher lasts until the process restarts.
    watcher_dead: bool,
}

impl<P> Behaviour<P>
where
    P: Provider,
{
    /// Builds a new `Mdns` behaviour.
    pub fn new(config: Config, local_peer_id: PeerId) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel(10); // Chosen arbitrarily.
        // INTERWEAVE PATCH (ADR-0053 rule 5): bounded like the one above.
        let (failure_sender, failure_receiver) = mpsc::channel(8);

        Ok(Self {
            config,
            if_watch: P::new_watcher()?,
            if_tasks: Default::default(),
            query_response_receiver: rx,
            query_response_sender: tx,
            discovered_nodes: Default::default(),
            closest_expiration: Default::default(),
            listen_addresses: Default::default(),
            local_peer_id,
            pending_events: Default::default(),
            peer_records: Default::default(),
            failure_receiver,
            failure_sender,
            drop_counts: Default::default(),
            watcher_failed: false,
            watcher_dead: false,
        })
    }

    /// INTERWEAVE PATCH (ADR-0053 rule 7): the shared drop counts.
    pub fn drop_counts(&self) -> Arc<DropCounts> {
        self.drop_counts.clone()
    }

    /// INTERWEAVE PATCH (ADR-0053 rule 2): make room for a new record of
    /// `peer` expiring at `expiration`, within whichever bound it hits.
    /// Returns `false` when the record should be refused instead: what
    /// would make room expires no sooner than it. Evicted pairs are
    /// appended to `evicted` and counted.
    ///
    /// - `peer` already holds MAX_ADDRESSES_PER_DISCOVERED_PEER records:
    ///   its own soonest-expiring record makes room, freeing the address
    ///   slot the provider would free -- while none of that peer's records
    ///   is one the learn-site boundary refused (lib.rs,
    ///   MAX_DISCOVERED_PEERS).
    /// - `peer` is new and MAX_DISCOVERED_PEERS peers are held: the peer
    ///   that would leave soonest -- the one whose LAST record expires
    ///   first -- goes, every record of it, freeing the peer slot.
    fn make_room_for(
        &mut self,
        peer: PeerId,
        expiration: Instant,
        evicted: &mut Vec<(PeerId, Multiaddr)>,
    ) -> bool {
        let held = self.peer_records.get(&peer).copied().unwrap_or(0);
        if held >= crate::MAX_ADDRESSES_PER_DISCOVERED_PEER {
            let (index, soonest) = self
                .discovered_nodes
                .iter()
                .enumerate()
                .filter(|(_, (p, _, _))| *p == peer)
                .min_by_key(|(_, (_, _, expires))| *expires)
                .map(|(i, (_, _, expires))| (i, *expires))
                .expect("a peer at its address bound holds records");
            if soonest >= expiration {
                return false;
            }
            let (gone_peer, gone_addr, _) = self.discovered_nodes.swap_remove(index);
            forget_record(&mut self.peer_records, &gone_peer);
            DropCounts::count(&self.drop_counts.records_evicted);
            evicted.push((gone_peer, gone_addr));
            return true;
        }
        if held == 0 && self.peer_records.len() >= crate::MAX_DISCOVERED_PEERS {
            let mut leaves: HashMap<PeerId, Instant> = HashMap::new();
            for (p, _, expires) in &self.discovered_nodes {
                let last = leaves.entry(*p).or_insert(*expires);
                *last = cmp::max(*last, *expires);
            }
            let (victim, soonest) = leaves
                .into_iter()
                .min_by_key(|(_, last)| *last)
                .expect("a full store holds peers");
            if soonest >= expiration {
                return false;
            }
            let drop_counts = &self.drop_counts;
            self.discovered_nodes.retain(|(p, a, _)| {
                if *p == victim {
                    DropCounts::count(&drop_counts.records_evicted);
                    evicted.push((*p, a.clone()));
                    return false;
                }
                true
            });
            self.peer_records.remove(&victim);
        }
        true
    }

    /// Returns true if the given `PeerId` is in the list of nodes discovered through mDNS.
    #[deprecated(note = "Use `discovered_nodes` iterator instead.")]
    pub fn has_node(&self, peer_id: &PeerId) -> bool {
        self.discovered_nodes().any(|p| p == peer_id)
    }

    /// Returns the list of nodes that we have discovered through mDNS and that are not expired.
    pub fn discovered_nodes(&self) -> impl ExactSizeIterator<Item = &PeerId> {
        self.discovered_nodes.iter().map(|(p, _, _)| p)
    }

    /// INTERWEAVE PATCH (ADR-0053 rule 8): every record the store holds,
    /// with the instant it expires -- what the runtime's refresh (rule 10)
    /// re-pushes, since `discovered_nodes` yields peer ids alone. A record
    /// whose expiry has passed but that the next `poll` has not yet swept
    /// is included; the caller compares the expiry with its own clock.
    pub fn discovered_records(
        &self,
    ) -> impl ExactSizeIterator<Item = (&PeerId, &Multiaddr, Instant)> {
        self.discovered_nodes.iter().map(|(p, a, e)| (p, a, *e))
    }

    /// Expires a node before the ttl.
    #[deprecated(note = "Unused API. Will be removed in the next release.")]
    pub fn expire_node(&mut self, peer_id: &PeerId) {
        let now = Instant::now();
        for (peer, _addr, expires) in &mut self.discovered_nodes {
            if peer == peer_id {
                *expires = now;
            }
        }
        self.closest_expiration = Some(P::Timer::at(now));
    }
}

/// INTERWEAVE PATCH (ADR-0053 rule 2): one record of `peer` left the
/// store; a peer with none left leaves the count map.
fn forget_record(peer_records: &mut HashMap<PeerId, usize>, peer: &PeerId) {
    if let Some(n) = peer_records.get_mut(peer) {
        *n -= 1;
        if *n == 0 {
            peer_records.remove(peer);
        }
    }
}

impl<P> NetworkBehaviour for Behaviour<P>
where
    P: Provider,
{
    type ConnectionHandler = dummy::ConnectionHandler;
    type ToSwarm = Event;

    fn handle_established_inbound_connection(
        &mut self,
        _: ConnectionId,
        _: PeerId,
        _: &Multiaddr,
        _: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(dummy::ConnectionHandler)
    }

    fn handle_pending_outbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        maybe_peer: Option<PeerId>,
        _addresses: &[Multiaddr],
        _effective_role: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        let Some(peer_id) = maybe_peer else {
            return Ok(vec![]);
        };

        Ok(self
            .discovered_nodes
            .iter()
            .filter(|(peer, _, _)| peer == &peer_id)
            .map(|(_, addr, _)| addr.clone())
            .collect())
    }

    fn handle_established_outbound_connection(
        &mut self,
        _: ConnectionId,
        _: PeerId,
        _: &Multiaddr,
        _: Endpoint,
        _: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(dummy::ConnectionHandler)
    }

    fn on_connection_handler_event(
        &mut self,
        _: PeerId,
        _: ConnectionId,
        ev: THandlerOutEvent<Self>,
    ) {
        libp2p_core::util::unreachable(ev)
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        self.listen_addresses
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .on_swarm_event(&event);
    }

    #[tracing::instrument(level = "trace", name = "NetworkBehaviour::poll", skip(self, cx))]
    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        loop {
            // Check for pending events and emit them.
            if let Some(event) = self.pending_events.pop_front() {
                return Poll::Ready(event);
            }

            // Poll ifwatch.
            // INTERWEAVE PATCH (ADR-0053 rule 5): note what was queued, so
            // an InterfaceFailed pushed below is emitted now rather than
            // on some unrelated later wake.
            let queued_before = self.pending_events.len();
            // INTERWEAVE PATCH (ADR-0053 rule 5): a dead watcher is not
            // polled at all -- if-watch 3.2.2 returns `Err` on every poll
            // once its netlink connection has ended, and polling it again
            // would spin here and never return what was queued.
            let mut errors_in_row = 0_u8;
            while !self.watcher_dead {
                let Poll::Ready(Some(event)) = Pin::new(&mut self.if_watch).poll_next(cx) else {
                    break;
                };
                // INTERWEAVE PATCH (ADR-0053 rule 5): a working watcher
                // re-arms the once-until-recovered report below.
                if event.is_ok() {
                    self.watcher_failed = false;
                    errors_in_row = 0;
                }
                match event {
                    Ok(IfEvent::Up(inet)) => {
                        let addr = inet.addr();
                        if addr.is_loopback() {
                            continue;
                        }
                        if addr.is_ipv4() && self.config.enable_ipv6
                            || addr.is_ipv6() && !self.config.enable_ipv6
                        {
                            continue;
                        }
                        if let Entry::Vacant(e) = self.if_tasks.entry(addr) {
                            match InterfaceState::<P::Socket, P::Timer>::new(
                                addr,
                                self.config.clone(),
                                self.local_peer_id,
                                self.listen_addresses.clone(),
                                self.query_response_sender.clone(),
                                self.failure_sender.clone(),
                                self.drop_counts.clone(),
                            ) {
                                Ok(iface_state) => {
                                    e.insert(P::spawn(iface_state));
                                }
                                Err(err) => {
                                    tracing::error!("failed to create `InterfaceState`: {}", err);
                                    // INTERWEAVE PATCH (ADR-0053 rule 5): a
                                    // failed bind or multicast join is an
                                    // event, not only a log line.
                                    self.pending_events.push_back(ToSwarm::GenerateEvent(
                                        Event::InterfaceFailed {
                                            address: addr,
                                            reason: err.to_string(),
                                        },
                                    ));
                                }
                            }
                        }
                    }
                    Ok(IfEvent::Down(inet)) => {
                        if let Some(handle) = self.if_tasks.remove(&inet.addr()) {
                            tracing::info!(instance=%inet.addr(), "dropping instance");

                            handle.abort();
                        }
                    }
                    Err(err) => {
                        tracing::error!("if watch returned an error: {}", err);
                        // INTERWEAVE PATCH (ADR-0053 rule 5): the watcher's
                        // own failure is an event, ONCE until it recovers.
                        // if-watch 3.2.2 can return Err on every poll after
                        // its netlink connection ends; an event per Err
                        // would grow `pending_events` without bound, and
                        // polling on would spin, hence the stop below.
                        if !self.watcher_failed {
                            self.watcher_failed = true;
                            self.pending_events.push_back(ToSwarm::GenerateEvent(
                                Event::WatcherFailed {
                                    reason: err.to_string(),
                                },
                            ));
                        }
                        // Two in a row is dead: stop, and keep serving the
                        // interfaces already up.
                        errors_in_row = errors_in_row.saturating_add(1);
                        if errors_in_row >= 2 {
                            self.watcher_dead = true;
                            tracing::error!("if watch failed twice in a row; no longer polled");
                        }
                    }
                }
            }
            if self.pending_events.len() > queued_before {
                continue;
            }
            // INTERWEAVE PATCH (ADR-0053 rule 5): what an interface task
            // reports on its way out -- a receive error that ends it, a
            // send error it skips -- becomes an event too.
            let mut failed = false;
            while let Poll::Ready(Some((address, reason))) =
                self.failure_receiver.poll_next_unpin(cx)
            {
                self.pending_events
                    .push_back(ToSwarm::GenerateEvent(Event::InterfaceFailed {
                        address,
                        reason,
                    }));
                failed = true;
            }
            if failed {
                continue;
            }
            // Emit discovered event.
            let mut discovered = Vec::new();
            // INTERWEAVE PATCH (ADR-0053 rule 2): records evicted at a bound
            // of the store -- the peer bound or one peer's address bound --
            // reported as expired, unless this batch added them, so the
            // provider retracts what this crate no longer holds.
            let mut evicted = Vec::new();
            // INTERWEAVE PATCH (ADR-0053 rule 2): each pair's FIRST
            // transition in this batch -- `true` for an add, `false` for an
            // eviction -- which says whether it was held before the batch.
            let mut first_is_add: HashMap<(PeerId, Multiaddr), bool> = HashMap::new();

            while let Poll::Ready(Some((peer, addr, expiration))) =
                self.query_response_receiver.poll_next_unpin(cx)
            {
                if let Some((_, _, cur_expires)) = self
                    .discovered_nodes
                    .iter_mut()
                    .find(|(p, a, _)| *p == peer && *a == addr)
                {
                    *cur_expires = cmp::max(*cur_expires, expiration);
                } else {
                    // INTERWEAVE PATCH (ADR-0053 rule 2): the store takes
                    // the provider's SHAPE -- at most MAX_DISCOVERED_PEERS
                    // peers, MAX_ADDRESSES_PER_DISCOVERED_PEER addresses
                    // each -- so, while no record the workspace's
                    // learn-site boundary refuses is held, every one held is
                    // one the provider would keep and each eviction frees
                    // the slot the provider would free. A refused record
                    // takes a slot here and none there (lib.rs,
                    // MAX_DISCOVERED_PEERS). Within the bound hit, the
                    // soonest to go makes room, unless the new record would
                    // go sooner still, in which case it is refused.
                    let already = evicted.len();
                    let room = self.make_room_for(peer, expiration, &mut evicted);
                    for pair in &evicted[already..] {
                        first_is_add.entry(pair.clone()).or_insert(false);
                    }
                    if !room {
                        DropCounts::count(&self.drop_counts.records_refused);
                        continue;
                    }
                    first_is_add.entry((peer, addr.clone())).or_insert(true);
                    *self.peer_records.entry(peer).or_insert(0) += 1;
                    // INTERWEAVE PATCH (ADR-0053 rule 8): the peer id, not
                    // the address. This line runs BEFORE the workspace's
                    // address-class boundary, so the address it printed
                    // could be one that boundary refuses (ADR-0052 rule 5),
                    // and under a flood it was one line per record.
                    tracing::info!(%peer, "discovered peer on an address");
                    self.discovered_nodes.push((peer, addr.clone(), expiration));
                    discovered.push((peer, addr.clone()));

                    self.pending_events
                        .push_back(ToSwarm::NewExternalAddrOfPeer {
                            peer_id: peer,
                            address: addr,
                        });
                }
            }

            // INTERWEAVE PATCH (ADR-0053 rule 2): the batch is NETTED
            // before it is reported, by each pair's state at its two ends: a
            // pair not held before the batch (its first transition an add)
            // and held after is discovered; a pair held before (its first
            // transition an eviction) and not held after is expired;
            // anything else is neither, however many times it moved in
            // between. A set-based netting that dropped every pair seen on
            // both sides was right only for an even number of transitions,
            // and one response repeating a peer across PTRs makes three
            // (#112, the automated review's P1 and the blind review's N5 on
            // 34fd3ad). Each pair reported once.
            if !evicted.is_empty() {
                let held_now: HashSet<(PeerId, Multiaddr)> = self
                    .discovered_nodes
                    .iter()
                    .filter(|(p, a, _)| first_is_add.contains_key(&(*p, a.clone())))
                    .map(|(p, a, _)| (*p, a.clone()))
                    .collect();
                let mut seen = HashSet::new();
                discovered.retain(|pair| {
                    first_is_add.get(pair) == Some(&true)
                        && held_now.contains(pair)
                        && seen.insert(pair.clone())
                });
                let mut seen = HashSet::new();
                evicted.retain(|pair| {
                    first_is_add.get(pair) == Some(&false)
                        && !held_now.contains(pair)
                        && seen.insert(pair.clone())
                });
            }
            if !discovered.is_empty() || !evicted.is_empty() {
                if !discovered.is_empty() {
                    let event = Event::Discovered(discovered);
                    // Push to the front of the queue so that the behavior event is reported
                    // before the individual discovered addresses.
                    self.pending_events
                        .push_front(ToSwarm::GenerateEvent(event));
                }
                // INTERWEAVE PATCH (ADR-0053 rule 2): the evictions go in
                // FRONT of the discovery that caused them. The provider
                // holds the same shape as this store, so it must see the
                // room made before the record that takes it, or it
                // refuses the new record and then drops the old one.
                if !evicted.is_empty() {
                    self.pending_events
                        .push_front(ToSwarm::GenerateEvent(Event::Expired(evicted)));
                }
                continue;
            }
            // Emit expired event.
            let now = Instant::now();
            let mut closest_expiration = None;
            let mut expired = Vec::new();
            let peer_records = &mut self.peer_records;
            self.discovered_nodes.retain(|(peer, addr, expiration)| {
                if *expiration <= now {
                    // INTERWEAVE PATCH (ADR-0053 rule 8): as above.
                    tracing::info!(%peer, "expired peer on an address");
                    expired.push((*peer, addr.clone()));
                    // INTERWEAVE PATCH (ADR-0053 rule 2): keep the per-peer
                    // count in step with the store.
                    forget_record(peer_records, peer);
                    return false;
                }
                closest_expiration =
                    Some(closest_expiration.unwrap_or(*expiration).min(*expiration));
                true
            });
            if !expired.is_empty() {
                let event = Event::Expired(expired);
                self.pending_events.push_back(ToSwarm::GenerateEvent(event));
                continue;
            }
            if let Some(closest_expiration) = closest_expiration {
                let mut timer = P::Timer::at(closest_expiration);
                let _ = Pin::new(&mut timer).poll_next(cx);

                self.closest_expiration = Some(timer);
            }

            return Poll::Pending;
        }
    }
}

/// Event that can be produced by the `Mdns` behaviour.
#[derive(Debug, Clone)]
pub enum Event {
    /// Discovered nodes through mDNS.
    Discovered(Vec<(PeerId, Multiaddr)>),

    /// The given combinations of `PeerId` and `Multiaddr` have expired.
    ///
    /// Each discovered record has a time-to-live. When this TTL expires and the address hasn't
    /// been refreshed, we remove it from the list and emit it as an `Expired` event.
    Expired(Vec<(PeerId, Multiaddr)>),

    /// INTERWEAVE PATCH (ADR-0053 rule 5): an interface cannot discover.
    /// Its bind or multicast join failed when it came up, a receive
    /// error ended its task, or a send failed. `address` is this node's
    /// own interface address, never a peer's. Whether to re-create the
    /// interface is the caller's decision, not this crate's.
    InterfaceFailed {
        /// This node's interface address.
        address: IpAddr,
        /// The operating system's error.
        reason: String,
    },

    /// INTERWEAVE PATCH (ADR-0053 rule 5): the interface watcher itself
    /// reported an error after start, so interfaces coming and going may
    /// no longer be seen. Reported once, and again only after the watcher
    /// has worked since; it names no interface. A watcher that fails on two
    /// consecutive polls is dead and not polled again, so its report is
    /// final for this behaviour: recovery would be rebuilding it (ADR-0053
    /// rule 5), which nothing does yet.
    WatcherFailed {
        /// The watcher's error.
        reason: String,
    },
}
