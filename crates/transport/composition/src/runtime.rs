// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The composed runtime: the substrate and the discovery providers driven
//! by one task, behind the neutral [`TransportRuntime`] surface.
//!
//! ONE TASK OWNS THE SUBSTRATE, because its event stream has one reader:
//! a Kademlia event is the provider's, an mDNS event the mDNS provider's,
//! a peer event the consumer's, and whichever reads the stream must hand
//! each to its owner. The handle talks to that task over a bounded
//! request channel; a request the task can no longer answer is answered
//! `BackendUnavailable`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use interweave_local_client_api::Generation;
use interweave_profile_config::{ProfileConfig, ProfilePaths};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{
    Component, ComponentHealth, ConnectivitySummary, HealthReport, LocalIdentity, PathChangeReason,
    PeerPath, PeerSummary, TransportCapabilities, TransportError, TransportEvent,
    TransportIdentity, TransportRuntime,
};
use interweave_transport_libp2p::{PathChange, RuntimeStatus, SwarmEvent, SwarmRuntime};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::discovery::{Discovery, DiscoveryDiagnostics};
use crate::notices::{PeerNoticeDiagnostics, PeerNotices};
use crate::session::InProcessBinding;
use crate::translate::{CompositionError, translate};

/// How a runtime is composed beyond what the profile says.
///
/// [`CompositionOptions::from_profile`] is the production construction:
/// the listen addresses, the peer cache and the client queue bound come
/// from the profile and its paths. The fields stay public for tests,
/// which override them (a port 0 listener, a scratch cache).
#[derive(Debug, Clone)]
pub struct CompositionOptions {
    /// Addresses to listen on at start (`transport.listen.addresses`).
    pub listen: Vec<String>,
    /// The peer cache's file (`ProfilePaths::peer_cache_file`), required
    /// when the profile enables `peer-cache`.
    pub peer_cache_file: Option<PathBuf>,
    /// Each endpoint's and each channel's delivery queue bound
    /// (`TRANSPORT.md` §Backpressure: 256 per client).
    pub queue_bound: usize,
    /// The runtime-to-consumer event queue (`TRANSPORT.md`: 1024).
    pub event_capacity: usize,
    /// How often the discovery providers are drained and swept.
    pub discovery_interval: Duration,
}

impl CompositionOptions {
    /// The options a profile states (plan §16 (13)): its listen addresses,
    /// its peer cache under its own paths, and `ipc.client_event_queue`
    /// as each client's delivery queue bound; the rest are the defaults.
    #[must_use]
    pub fn from_profile(profile: &ProfileConfig, paths: &ProfilePaths) -> Self {
        Self {
            listen: profile.transport.listen.addresses.clone(),
            peer_cache_file: Some(paths.peer_cache_file()),
            queue_bound: usize::try_from(profile.ipc.client_event_queue).unwrap_or(usize::MAX),
            ..Self::default()
        }
    }
}

impl Default for CompositionOptions {
    fn default() -> Self {
        Self {
            listen: Vec::new(),
            peer_cache_file: None,
            queue_bound: 256,
            event_capacity: 1024,
            discovery_interval: Duration::from_secs(1),
        }
    }
}

/// Diagnostics for a local operator: the substrate's status surface and
/// discovery's state. Outside the neutral contract, so it names backend
/// types; `TransportRuntime` does not.
#[derive(Debug, Clone)]
pub struct Diagnostics {
    /// The substrate's status surface (plan §15).
    pub substrate: RuntimeStatus,
    /// The discovery providers and the manager.
    pub discovery: DiscoveryDiagnostics,
    /// Neutral events dropped: the consumer's queue was full, or -- at
    /// shutdown -- the substrate's unread backlog ran past its bound.
    pub events_dropped: u64,
    /// The in-process sessions' peer-notice registry.
    pub peer_notices: PeerNoticeDiagnostics,
}

/// An admin port's request that the runtime's owner shut it down
/// (`AdminPort::shutdown`): the runtime never stops itself on one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownRequest {
    /// The asking port's id: which authority asked, for the owner's log.
    pub port: Generation,
    /// The grace the port asked for.
    pub grace: Duration,
}

pub(crate) enum Request {
    Health(oneshot::Sender<HealthReport>),
    Connectivity(oneshot::Sender<Option<ConnectivitySummary>>),
    Peers(oneshot::Sender<Vec<PeerSummary>>),
    Diagnostics(oneshot::Sender<Option<Diagnostics>>),
    /// Answered once the substrate has stopped, with the events dropped
    /// over the runtime's whole life -- the last value, which nothing can
    /// read from the runtime afterwards (#139 review N1).
    Shutdown(Duration, oneshot::Sender<u64>),
}

/// Ask the driver over `requests`; a driver that has gone answers
/// `BackendUnavailable`.
pub(crate) async fn ask_driver<T>(
    requests: &mpsc::Sender<Request>,
    request: impl FnOnce(oneshot::Sender<T>) -> Request,
) -> Result<T, TransportError> {
    let (reply, answer) = oneshot::channel();
    requests
        .send(request(reply))
        .await
        .map_err(|_| TransportError::BackendUnavailable)?;
    answer.await.map_err(|_| TransportError::BackendUnavailable)
}

/// A transport runtime composed from one validated profile.
pub struct ComposedRuntime {
    identity: LocalIdentity,
    capabilities: TransportCapabilities,
    listening: Vec<String>,
    requests: mpsc::Sender<Request>,
    events: mpsc::Receiver<TransportEvent>,
    task: Option<JoinHandle<()>>,
    dropped: Arc<AtomicU64>,
    sessions: InProcessBinding,
    shutdown_requests: watch::Receiver<Option<ShutdownRequest>>,
}

/// Discovery's clock: wall-clock milliseconds read once, at start, then
/// advanced by the monotonic clock.
struct AnchoredClock {
    wall_at_start: u64,
    started: tokio::time::Instant,
}

impl AnchoredClock {
    fn start() -> Self {
        Self {
            wall_at_start: wall_ms(),
            started: tokio::time::Instant::now(),
        }
    }

    fn now_ms(&self) -> u64 {
        anchored(self.wall_at_start, self.started.elapsed())
    }
}

/// Fold the substrate's shutdown backlog overflow into the runtime's
/// dropped-event counter (`the_shutdown_backlog_is_added_to_the_dropped_total`
/// tests the addition). That `Driver::run` calls it before answering
/// `stop()` is read in the code, not tested: overflowing the backlog takes
/// more than four times the event capacity left unread at shutdown, which
/// no test here produces (#144 re-review, finding 1).
fn count_backlog(dropped: &AtomicU64, backlog_dropped: usize) {
    let added = u64::try_from(backlog_dropped).unwrap_or(u64::MAX);
    dropped.fetch_add(added, Ordering::Relaxed);
}

/// The anchor plus the monotonic time since it was read.
fn anchored(wall_at_start: u64, elapsed: Duration) -> u64 {
    wall_at_start.saturating_add(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}

/// Wall-clock milliseconds, for `observed_at` and the summary's stamp.
pub(crate) fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

impl ComposedRuntime {
    /// Validate and translate `profile`, start the substrate with its
    /// blocks switched on, install its endpoints and channels, listen,
    /// compose its discovery providers, and start driving them.
    ///
    /// # Errors
    /// The profile's violations, a translator's refusal, the substrate's,
    /// or a provider's.
    pub async fn start(
        identity: &ProfileIdentity,
        profile: &ProfileConfig,
        options: CompositionOptions,
    ) -> Result<Self, CompositionError> {
        let local = identity
            .transport_identity()
            .map_err(|_| CompositionError::Translation("the identity has no transport identity"))?;
        // REFUSED HERE, not in the driver: `tokio::time::interval` panics on
        // a zero period, and inside the spawned task that panic would
        // leave the caller holding a runtime whose driver is already gone.
        if options.discovery_interval.is_zero() {
            return Err(CompositionError::Translation(
                "discovery_interval must be greater than zero",
            ));
        }
        let composition = translate(profile, &local, options.queue_bound)?;
        // A WALL-CLOCK ANCHOR ADVANCED BY THE MONOTONIC CLOCK. The peer
        // cache persists these timestamps and compares them against its
        // TTL after a restart, so the origin must survive the process:
        // a process-relative one made a month-old record fresh on every
        // start (#137 review R1). And it must keep advancing: holding
        // back after a system clock step froze every expiry and schedule
        // for the step's length (#137 re-review N2). One wall reading
        // at start, then `Instant` time, gives both. The whole discovery
        // layer shares it, since the cache's stamps reach the manager.
        let clock = AnchoredClock::start();
        let clock = move || clock.now_ms();
        let discovery = Discovery::new(
            composition.discovery,
            options.peer_cache_file.as_deref(),
            composition.peer_trust,
            profile.trust.allowed_peers.clone(),
            &local,
            clock(),
        )?;
        let swarm = SwarmRuntime::start(identity, composition.substrate, composition.trust)
            .map_err(CompositionError::Substrate)?;
        let configured = async {
            swarm
                .configure_direct(composition.direct)
                .await
                .map_err(CompositionError::Substrate)?;
            swarm
                .configure_broadcast(composition.broadcast)
                .await
                .map_err(CompositionError::Substrate)?;
            let mut listening = Vec::with_capacity(options.listen.len());
            for address in &options.listen {
                let address = address.parse().map_err(|_| {
                    CompositionError::Translation("a listen address is not a multiaddr")
                })?;
                let bound = swarm
                    .listen(address)
                    .await
                    .map_err(CompositionError::Substrate)?;
                listening.push(bound.to_string());
            }
            Ok(listening)
        }
        .await;
        // A START THAT FAILS AFTER THE SUBSTRATE STARTED STOPS IT before
        // returning. Dropped instead, the substrate's task is only
        // scheduled for abort, so a listener already bound here could
        // still hold its port when the caller retries
        // (`a_failed_start_has_released_its_listeners_when_it_returns`).
        let listening = match configured {
            Ok(listening) => listening,
            Err(e) => {
                let _ = swarm.shutdown().await;
                return Err(e);
            }
        };

        // The session binding talks to the substrate directly, before the
        // driver takes it: a session's exchange is never the driver's to
        // wait on.
        let (requests, request_rx) = mpsc::channel(64);
        let (shutdown_tx, shutdown_requests) = watch::channel(None);
        let notices = PeerNotices::default();
        let sessions = InProcessBinding::new(
            swarm.commander(),
            // `start` is async, so this is the runtime the substrate runs
            // on: where a session's teardown goes when it cannot be queued.
            tokio::runtime::Handle::current(),
            options.queue_bound,
            requests.downgrade(),
            local.clone(),
            Arc::new(shutdown_tx),
            notices.clone(),
        );
        let (event_tx, events) = mpsc::channel(options.event_capacity.max(1));
        let dropped = Arc::new(AtomicU64::new(0));
        let driver = Driver {
            swarm,
            notices,
            discovery,
            requests: request_rx,
            events: event_tx,
            dropped: Arc::clone(&dropped),
            paths: BTreeMap::new(),
            last_summary: None,
            clock: Box::new(clock),
        };
        let task = tokio::spawn(driver.run(options.discovery_interval));
        Ok(Self {
            identity: LocalIdentity {
                peer: local,
                // No rotation exists in this build: the one identity a
                // runtime starts with is its first epoch.
                identity_epoch: 1,
            },
            capabilities: composition.capabilities,
            listening,
            sessions,
            requests,
            events,
            task: Some(task),
            dropped,
            shutdown_requests,
        })
    }

    /// The direct in-process `LocalDataSession` binding (plan §15 (3)):
    /// every session opened through it, and its admin facade, share this
    /// runtime's substrate.
    #[must_use]
    pub fn sessions(&self) -> InProcessBinding {
        self.sessions.clone()
    }

    /// The addresses bound at start, as the substrate reported them.
    #[must_use]
    pub fn listening(&self) -> &[String] {
        &self.listening
    }

    /// Neutral events dropped so far because the consumer's queue was
    /// full. The shutdown backlog's count is added by [`stop`](Self::stop),
    /// which returns the total.
    #[must_use]
    pub fn events_dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// The operator's diagnostics.
    ///
    /// # Errors
    /// `BackendUnavailable` once the runtime has stopped.
    pub async fn diagnostics(&self) -> Result<Diagnostics, TransportError> {
        self.ask(Request::Diagnostics)
            .await?
            .ok_or(TransportError::BackendUnavailable)
    }

    /// Resolves with the first shutdown an admin port asked for. The
    /// runtime does not act on it: its owner -- the composition root --
    /// awaits this beside its own signals and calls [`stop`](Self::stop).
    /// `None` only if nothing remains that could ask.
    pub async fn shutdown_requested(&self) -> Option<ShutdownRequest> {
        let mut requests = self.shutdown_requests.clone();
        requests
            .wait_for(Option::is_some)
            .await
            .ok()
            .and_then(|pending| pending.clone())
    }

    /// Stop the runtime, returning the neutral events it dropped over its
    /// whole life, the substrate's shutdown backlog included: the one
    /// reading of that count nothing can take afterwards.
    ///
    /// # Errors
    /// `Internal` if the driver task panicked.
    pub async fn stop(self) -> Result<u64, TransportError> {
        self.stop_within(interweave_transport_libp2p::runtime::SHUTDOWN_GRACE)
            .await
    }

    /// [`stop`](Self::stop), letting exchanges already in flight settle for
    /// `grace` -- the grace an admin port's shutdown named
    /// ([`ShutdownRequest::grace`]).
    ///
    /// # Errors
    /// As [`stop`](Self::stop).
    pub async fn stop_within(mut self, grace: Duration) -> Result<u64, TransportError> {
        let answered = self.ask(|reply| Request::Shutdown(grace, reply)).await;
        if let Some(task) = self.task.take() {
            task.await.map_err(|_| TransportError::Internal)?;
        }
        // A driver that ended before answering has still counted into the
        // shared counter; read it rather than report nothing.
        Ok(answered.unwrap_or_else(|_| self.dropped.load(Ordering::Relaxed)))
    }

    async fn ask<T>(
        &self,
        request: impl FnOnce(oneshot::Sender<T>) -> Request,
    ) -> Result<T, TransportError> {
        ask_driver(&self.requests, request).await
    }
}

impl TransportRuntime for ComposedRuntime {
    fn local_identity(&self) -> LocalIdentity {
        self.identity.clone()
    }

    fn capabilities(&self) -> TransportCapabilities {
        self.capabilities.clone()
    }

    async fn health(&self) -> Result<HealthReport, TransportError> {
        self.ask(Request::Health).await
    }

    async fn connectivity(&self) -> Result<ConnectivitySummary, TransportError> {
        self.ask(Request::Connectivity)
            .await?
            .ok_or(TransportError::BackendUnavailable)
    }

    async fn peers(&self) -> Result<Vec<PeerSummary>, TransportError> {
        self.ask(Request::Peers).await
    }

    async fn next_event(&mut self) -> Option<TransportEvent> {
        self.events.recv().await
    }

    async fn shutdown(self) -> Result<(), TransportError> {
        self.stop().await.map(drop)
    }
}

struct Driver {
    swarm: SwarmRuntime,
    /// Every in-process session holding `events` is owed each disconnect.
    notices: PeerNotices,
    discovery: Discovery,
    requests: mpsc::Receiver<Request>,
    events: mpsc::Sender<TransportEvent>,
    dropped: Arc<AtomicU64>,
    /// Each connected logical peer's path, from the substrate's per-peer
    /// events; bounded by the substrate's connection ceiling.
    paths: BTreeMap<TransportIdentity, PeerPath>,
    /// The summary last announced, compared without its timestamp.
    last_summary: Option<ConnectivitySummary>,
    clock: Box<dyn Fn() -> u64 + Send>,
}

impl Driver {
    async fn run(mut self, interval: Duration) {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut shutdown_reply = None;
        // The default, until a shutdown request names its own.
        let mut shutdown_grace = interweave_transport_libp2p::runtime::SHUTDOWN_GRACE;
        loop {
            tokio::select! {
                event = self.swarm.next_event() => match event {
                    Some(event) => self.on_swarm_event(event).await,
                    None => break,
                },
                request = self.requests.recv() => match request {
                    Some(Request::Shutdown(grace, reply)) => {
                        shutdown_grace = grace;
                        shutdown_reply = Some(reply);
                        break;
                    }
                    Some(request) => self.answer(request).await,
                    None => break,
                },
                _ = tick.tick() => self.discovery_round().await,
            }
        }
        // THE SUBSTRATE STOPS FIRST, AND WHAT IT SAID IS READ BEFORE THE
        // LAST WRITE. `select!` picks among ready branches at random, so a
        // shutdown asked right after `PeerConnected` could win over the
        // `RouteConfirmed` queued behind it, and the final flush then
        // wrote a cache that never saw the route (#137 re-review N1).
        // Draining what was ready and THEN stopping still lost what the
        // substrate emitted in between; `SwarmRuntime::shutdown` returns
        // what nobody read -- the backlog whole, oldest first, up to four
        // times the event capacity, counting anything past that -- so
        // discovery sees it before its final flush (`shutdown_returns_the_events_nobody_read` in the
        // libp2p crate; end to end,
        // `a_reached_peer_survives_a_restart_through_the_peer_cache`).
        // Only discovery reads them: the consumer is told nothing more
        // once the runtime is shutting down.
        let unread = match self.swarm.shutdown_within(shutdown_grace).await {
            Ok(report) => {
                // What the report could not keep is counted with the
                // other events this runtime dropped, not discarded
                // unsaid (#139 review F10).
                count_backlog(&self.dropped, report.dropped);
                report.events
            }
            Err(_) => Vec::new(),
        };
        let now = (self.clock)();
        for event in &unread {
            let _ = self.discovery.on_swarm_event(event, now);
        }
        self.discovery.shutdown(now);
        if let Some(reply) = shutdown_reply {
            let _ = reply.send(self.dropped.load(Ordering::Relaxed));
        }
    }

    fn emit(&self, event: TransportEvent) {
        if self.events.try_send(event).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    async fn on_swarm_event(&mut self, event: SwarmEvent) {
        let now = (self.clock)();
        if self.discovery.on_swarm_event(&event, now) {
            return;
        }
        let observed_at = wall_ms();
        match event {
            SwarmEvent::Connected { peer, path } => {
                self.paths.insert(peer.clone(), path);
                self.emit(TransportEvent::PeerConnected {
                    peer,
                    path,
                    observed_at,
                });
            }
            SwarmEvent::PeerPathChanged {
                peer,
                previous,
                current,
                reason,
            } => {
                self.paths.insert(peer.clone(), current);
                self.emit(TransportEvent::PeerPathChanged {
                    peer,
                    previous,
                    current,
                    reason: match reason {
                        PathChange::DirectEstablished => PathChangeReason::DirectEstablished,
                        PathChange::HolePunched => PathChangeReason::Dcutr,
                        PathChange::DirectLost => PathChangeReason::DirectLost,
                    },
                    observed_at,
                });
                self.announce_connectivity().await;
            }
            SwarmEvent::Disconnected { peer, reason } => {
                self.paths.remove(&peer);
                self.notices.disconnected(&peer, reason);
                self.emit(TransportEvent::PeerDisconnected {
                    peer,
                    reason_class: reason,
                    observed_at,
                });
                self.announce_connectivity().await;
            }
            SwarmEvent::ConnectivityChanged { .. }
            | SwarmEvent::RelayReservationChanged { .. }
            | SwarmEvent::RelayStandingChanged { .. }
            | SwarmEvent::HolePunch { .. }
            | SwarmEvent::NetworkChanged { .. } => self.announce_connectivity().await,
            _ => {}
        }
    }

    /// `ConnectivityChanged` when the summary moved: an edge
    /// notification, not a history (`CONNECTIVITY.md` §5), so a summary
    /// equal to the last one but for its timestamp is not announced.
    async fn announce_connectivity(&mut self) {
        let Ok(status) = self.swarm.status(None).await else {
            return;
        };
        let summary = status.connectivity;
        let unchanged = self.last_summary.as_ref().is_some_and(|last| {
            ConnectivitySummary {
                updated_at: summary.updated_at,
                ..last.clone()
            } == summary
        });
        if !unchanged {
            self.last_summary = Some(summary.clone());
            self.emit(TransportEvent::ConnectivityChanged { summary });
        }
    }

    async fn answer(&mut self, request: Request) {
        match request {
            // THE TRANSPORT COMPONENT IS ASKED, not assumed: healthy when
            // the substrate answers its status, unavailable when it cannot.
            // This driver answering proved only that the DRIVER runs; a
            // substrate that had ended could still be reported healthy in
            // the window before the driver saw its event stream close
            // (the owner's review of c283e375, P3-1). A task that has
            // stopped cannot answer at all -- the caller then gets
            // `BackendUnavailable`. No test reaches the window itself: the
            // substrate cannot be ended under a live driver from outside.
            Request::Health(reply) => {
                let transport = if self.swarm.status(None).await.is_ok() {
                    interweave_transport_api::Health::Healthy
                } else {
                    interweave_transport_api::Health::Unavailable
                };
                let _ = reply.send(HealthReport::from_components(vec![
                    ComponentHealth {
                        component: Component::Transport,
                        health: transport,
                    },
                    ComponentHealth {
                        component: Component::Discovery,
                        health: self.discovery.health(),
                    },
                ]));
            }
            Request::Connectivity(reply) => {
                let summary = self.swarm.status(None).await.ok().map(|s| s.connectivity);
                let _ = reply.send(summary);
            }
            Request::Peers(reply) => {
                let _ = reply.send(
                    self.paths
                        .iter()
                        .map(|(peer, path)| PeerSummary {
                            peer: peer.clone(),
                            path: *path,
                        })
                        .collect(),
                );
            }
            Request::Diagnostics(reply) => {
                let substrate = self.swarm.status(None).await.ok();
                let _ = reply.send(substrate.map(|substrate| Diagnostics {
                    substrate,
                    discovery: self.discovery.diagnostics(),
                    events_dropped: self.dropped.load(Ordering::Relaxed),
                    peer_notices: self.notices.diagnostics(),
                }));
            }
            Request::Shutdown(_, reply) => {
                let _ = reply.send(self.dropped.load(Ordering::Relaxed));
            }
        }
    }

    /// Drain the providers, run Kademlia's schedule, and hand the book
    /// what changed -- through `learn`, the peer's door.
    async fn discovery_round(&mut self) {
        let now = (self.clock)();
        for command in self.discovery.kademlia_commands(now) {
            let _ = self.swarm.kademlia(command).await;
        }
        self.discovery.pump(now);
        for (peer, addresses) in self.discovery.changed_candidates(now) {
            let _ = self.swarm.learn(peer, addresses).await;
        }
        // AND DIALS THE PEERS THIS PROFILE WANTS a data-plane connection
        // to and has none with, on discovery's account
        // (`DiscoveryReconnect`); a peer in backoff is the gate's to
        // refuse.
        let paths = &self.paths;
        for peer in self
            .discovery
            .reconnect_targets(now, |p| paths.contains_key(p))
        {
            let _ = self.swarm.reconnect(peer).await;
        }
        self.discovery.flush(now);
    }
}

#[cfg(test)]
mod tests {
    use super::{anchored, count_backlog};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    /// The backlog the substrate could not keep is added to what the
    /// consumer's queue already dropped, and the total is what is read.
    #[test]
    fn the_shutdown_backlog_is_added_to_the_dropped_total() {
        let dropped = AtomicU64::new(3);
        count_backlog(&dropped, 5);
        assert_eq!(dropped.load(Ordering::Relaxed), 8);
        count_backlog(&dropped, 0);
        assert_eq!(
            dropped.load(Ordering::Relaxed),
            8,
            "an empty backlog adds nothing"
        );
    }

    /// The clock is the start's wall reading plus monotonic time: it
    /// moves only forward with elapsed time (a system clock step after
    /// start cannot reach it), and a later start's reading continues
    /// from a later anchor, so persisted stamps compare across restarts.
    #[test]
    fn discovery_time_is_the_anchor_plus_monotonic_time() {
        let anchor = 1_790_000_000_000;
        assert_eq!(anchored(anchor, Duration::ZERO), anchor);
        assert_eq!(anchored(anchor, Duration::from_secs(90)), anchor + 90_000);
        assert!(anchored(anchor, Duration::from_millis(1)) > anchored(anchor, Duration::ZERO));
        let restarted_anchor = anchor + 3_600_000;
        assert!(
            anchored(restarted_anchor, Duration::ZERO) > anchored(anchor, Duration::from_secs(90)),
            "a restart an hour later reads later than the old process's last stamp"
        );
    }
}
