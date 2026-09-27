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

use interweave_profile_config::ProfileConfig;
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{
    Component, ComponentHealth, ConnectivitySummary, HealthReport, LocalIdentity, PathChangeReason,
    PeerPath, PeerSummary, TransportCapabilities, TransportError, TransportEvent,
    TransportIdentity, TransportRuntime,
};
use interweave_transport_libp2p::{PathChange, RuntimeStatus, SwarmEvent, SwarmRuntime};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::discovery::{Discovery, DiscoveryDiagnostics};
use crate::translate::{CompositionError, translate};

/// How a runtime is composed beyond what the profile says.
#[derive(Debug, Clone)]
pub struct CompositionOptions {
    /// Addresses to listen on at start. The profile's `transport.listen`
    /// block is not modelled yet, so the embedder supplies them.
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
    /// Neutral events dropped because the consumer's queue was full.
    pub events_dropped: u64,
}

enum Request {
    Health(oneshot::Sender<HealthReport>),
    Connectivity(oneshot::Sender<Option<ConnectivitySummary>>),
    Peers(oneshot::Sender<Vec<PeerSummary>>),
    Diagnostics(oneshot::Sender<Option<Diagnostics>>),
    Shutdown(oneshot::Sender<()>),
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
}

/// Wall-clock milliseconds, for `observed_at` and the summary's stamp.
fn wall_ms() -> u64 {
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
        let composition = translate(profile, &local, options.queue_bound)?;
        let started = tokio::time::Instant::now();
        let clock = move || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
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

        let (requests, request_rx) = mpsc::channel(64);
        let (event_tx, events) = mpsc::channel(options.event_capacity.max(1));
        let dropped = Arc::new(AtomicU64::new(0));
        let driver = Driver {
            swarm,
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
            requests,
            events,
            task: Some(task),
            dropped,
        })
    }

    /// The addresses bound at start, as the substrate reported them.
    #[must_use]
    pub fn listening(&self) -> &[String] {
        &self.listening
    }

    /// Neutral events dropped because the consumer's queue was full.
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

    async fn ask<T>(
        &self,
        request: impl FnOnce(oneshot::Sender<T>) -> Request,
    ) -> Result<T, TransportError> {
        let (reply, answer) = oneshot::channel();
        self.requests
            .send(request(reply))
            .await
            .map_err(|_| TransportError::BackendUnavailable)?;
        answer.await.map_err(|_| TransportError::BackendUnavailable)
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

    async fn shutdown(mut self) -> Result<(), TransportError> {
        let _ = self.ask(Request::Shutdown).await;
        if let Some(task) = self.task.take() {
            task.await.map_err(|_| TransportError::Internal)?;
        }
        Ok(())
    }
}

struct Driver {
    swarm: SwarmRuntime,
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
        loop {
            tokio::select! {
                event = self.swarm.next_event() => match event {
                    Some(event) => self.on_swarm_event(event).await,
                    None => break,
                },
                request = self.requests.recv() => match request {
                    Some(Request::Shutdown(reply)) => {
                        shutdown_reply = Some(reply);
                        break;
                    }
                    Some(request) => self.answer(request).await,
                    None => break,
                },
                _ = tick.tick() => self.discovery_round().await,
            }
        }
        let now = (self.clock)();
        self.discovery.shutdown(now);
        let _ = self.swarm.shutdown().await;
        if let Some(reply) = shutdown_reply {
            let _ = reply.send(());
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
            SwarmEvent::Disconnected { peer } => {
                self.paths.remove(&peer);
                self.emit(TransportEvent::PeerDisconnected { peer, observed_at });
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
            Request::Health(reply) => {
                let _ = reply.send(HealthReport::from_components(vec![
                    ComponentHealth {
                        component: Component::Transport,
                        health: interweave_transport_api::Health::Healthy,
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
                }));
            }
            Request::Shutdown(reply) => {
                let _ = reply.send(());
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
