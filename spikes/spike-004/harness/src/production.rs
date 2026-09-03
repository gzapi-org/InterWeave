// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! The PRODUCTION gate, unmodified, with a relay client behind it.
//!
//! Everything else in this harness runs [`crate::gate::InstrumentedGate`]
//! — a gate shaped the way Stage 11 would have to build one, with
//! attribution. That is a proposal, and measuring a proposal proves
//! only that the proposal works.
//!
//! This module measures the thing that ships. `OutboundAdmission` is
//! taken from `interweave-transport-libp2p` by path, unchanged, and put
//! in front of a real `relay::client::Behaviour`. Whatever it does to
//! the reservation dial is what production does today.
//!
//! F1 says the answer is "refuses it", and says so from reading:
//! the pending hook builds its `DialRequest` with
//! `origin: DialOrigin::KademliaQuery` because Kademlia is the only
//! behaviour that can currently dial, `KademliaQuery.is_data_plane()`
//! is true, and `ConnectionPolicy::admit` refuses a data-plane origin
//! for a `ConnectivityInfrastructureOnly` peer. R6 is that chain run
//! rather than argued.

use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::OutboundAdmission;
use interweave_transport_libp2p::outbound_gate::InFlightTickets;
use interweave_transport_runtime::{ConnectionManager, ConnectionPolicy, TrustSources};
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::core::Endpoint;
use libp2p::core::transport::PortUse;
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler, THandlerInEvent,
    THandlerOutEvent, ToSwarm,
};
use libp2p::{Multiaddr, PeerId, Swarm, identify, identity, noise, relay, tcp, yamux};

/// One call into the wrapped gate's pending-outbound hook, and what it
/// answered.
///
/// THE REASON THIS EXISTS is the Swarm's own handling of a denied
/// behaviour dial. `Swarm::dial` builds `DialError::Denied`, hands it
/// to the behaviour as `FromSwarm::DialFailure`, and returns it — and
/// the caller for a behaviour-emitted `ToSwarm::Dial` is
/// `if let Ok(()) = self.dial(opts)` (libp2p-swarm 0.47.1 lib.rs:1098).
/// The `Err` is discarded: no `SwarmEvent::Dialing`, and no
/// `SwarmEvent::OutgoingConnectionError`. So the refusal is
/// unobservable from outside, and measuring it means standing where
/// the decision is made.
#[derive(Debug, Clone)]
pub struct Decision {
    pub connection_id: ConnectionId,
    pub peer: Option<PeerId>,
    /// `None` when the gate admitted the dial; the rendered
    /// `ConnectionDenied` when it refused.
    pub refusal: Option<String>,
}

/// The decisions a `ProductionNode`'s gate made, shared with the node.
#[derive(Debug, Clone, Default)]
pub struct Decisions {
    inner: Arc<Mutex<Vec<Decision>>>,
}

impl Decisions {
    fn record(&self, decision: Decision) {
        #[expect(clippy::unwrap_used, reason = "a poisoned spike is a failed spike")]
        self.inner.lock().unwrap().push(decision);
    }

    /// Every decision, in the order the gate made them.
    #[must_use]
    pub fn all(&self) -> Vec<Decision> {
        #[expect(clippy::unwrap_used, reason = "a poisoned spike is a failed spike")]
        self.inner.lock().unwrap().clone()
    }

    /// Refusals only, rendered.
    #[must_use]
    pub fn refusals(&self) -> Vec<String> {
        self.all().into_iter().filter_map(|d| d.refusal).collect()
    }

    /// Admissions only.
    #[must_use]
    pub fn admissions(&self) -> usize {
        self.all().iter().filter(|d| d.refusal.is_none()).count()
    }
}

/// A transparent wrapper that records what the inner gate decided.
///
/// It changes nothing: every method forwards, and the verdict returned
/// is the inner gate's own. It is the `Attributing` pattern from
/// [`crate::attribute`] pointed at a different question — there, which
/// behaviour asked; here, what the production gate answered.
pub struct Observing<B> {
    inner: B,
    decisions: Decisions,
}

impl<B> Observing<B> {
    pub fn new(inner: B, decisions: Decisions) -> Self {
        Self { inner, decisions }
    }
}

impl<B: NetworkBehaviour> NetworkBehaviour for Observing<B> {
    type ConnectionHandler = B::ConnectionHandler;
    type ToSwarm = B::ToSwarm;

    fn handle_established_inbound_connection(
        &mut self,
        id: ConnectionId,
        peer: PeerId,
        local: &Multiaddr,
        remote: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.inner
            .handle_established_inbound_connection(id, peer, local, remote)
    }

    fn handle_established_outbound_connection(
        &mut self,
        id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role: Endpoint,
        port: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.inner
            .handle_established_outbound_connection(id, peer, addr, role, port)
    }

    fn handle_pending_inbound_connection(
        &mut self,
        id: ConnectionId,
        local: &Multiaddr,
        remote: &Multiaddr,
    ) -> Result<(), ConnectionDenied> {
        self.inner
            .handle_pending_inbound_connection(id, local, remote)
    }

    fn handle_pending_outbound_connection(
        &mut self,
        id: ConnectionId,
        peer: Option<PeerId>,
        addresses: &[Multiaddr],
        role: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        let verdict = self
            .inner
            .handle_pending_outbound_connection(id, peer, addresses, role);
        self.decisions.record(Decision {
            connection_id: id,
            peer,
            // THE CAUSE CHAIN, not the rendering. `ConnectionDenied`'s
            // own `Display` is the bare string "connection denied" —
            // everything the gate wrote about WHY lives in
            // `Error::source`, so a refusal logged the obvious way says
            // nothing at all. Walking the chain is the only way to see
            // the gate's own words, and that this is necessary is
            // itself part of F8.
            refusal: verdict.as_ref().err().map(|denied| {
                let mut rendered = format!("{denied}");
                let mut source = std::error::Error::source(denied);
                while let Some(cause) = source {
                    rendered.push_str(&format!(": {cause}"));
                    source = cause.source();
                }
                rendered
            }),
        });
        verdict
    }

    fn on_swarm_event(&mut self, event: FromSwarm<'_>) {
        self.inner.on_swarm_event(event);
    }

    fn on_connection_handler_event(
        &mut self,
        peer: PeerId,
        id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        self.inner.on_connection_handler_event(peer, id, event);
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        self.inner.poll(cx)
    }
}

/// Identify plus a relay client, behind the production gate.
#[derive(NetworkBehaviour)]
pub struct ProductionBehaviour {
    /// FIRST, exactly as `SubstrateBehaviour` orders it. The
    /// `Observing` wrapper forwards every call and decides nothing.
    pub outbound: Observing<OutboundAdmission>,
    pub identify: identify::Behaviour,
    pub relay_client: relay::client::Behaviour,
}

/// A node running the shipped gate.
pub struct ProductionNode {
    pub swarm: Swarm<ProductionBehaviour>,
    /// Dial failures and listener events seen, by rendering.
    ///
    /// Diagnostic only. R6.9's claim is about the EVENT VARIANT, and a
    /// rendering can change; see [`Self::outgoing_errors`].
    pub failures: Vec<String>,
    /// `SwarmEvent::OutgoingConnectionError` occurrences, counted by
    /// variant rather than matched by string.
    ///
    /// F8 says a gate refusal of a behaviour dial emits no outgoing
    /// connection error at all. Asserting that by searching renderings
    /// for `Dial error` would pass for any error rendered differently,
    /// which is the opposite of what an absence claim needs.
    pub outgoing_errors: usize,
    /// Dial attempts the Swarm actually started.
    pub dialing: usize,
    /// Everything else, for diagnosis.
    pub other: Vec<String>,
    pub connected: usize,
    /// What the production gate itself answered, which the Swarm event
    /// stream does not report.
    pub decisions: Decisions,
    /// HELD, not dropped, and read by nothing — which is the point.
    ///
    /// `SnapshotHandle::is_current` upgrades a weak
    /// reference to the manager and refuses when it is gone, so a node
    /// that built a gate and let the manager fall out of scope refuses
    /// every dial with `PolicySuperseded` — regardless of trust, which
    /// makes the control look identical to the subject. That is
    /// exactly how this experiment first misread itself.
    #[expect(
        dead_code,
        reason = "held so the gate's snapshot stays current; R6.7 and R6.8 fail if it is dropped"
    )]
    pub manager: ConnectionManager,
}

impl ProductionNode {
    /// Build one whose only authorization for `relay` is
    /// infrastructure, which is what a relay is.
    #[must_use]
    pub fn new(infrastructure: &[TransportIdentity]) -> Self {
        Self::with_trust(&[], infrastructure)
    }

    /// The same node with an explicit data-plane allowlist, for the
    /// control: identical in every way except the class the relay is
    /// authorized under.
    #[must_use]
    pub fn with_trust(
        data_plane: &[TransportIdentity],
        infrastructure: &[TransportIdentity],
    ) -> Self {
        let keypair = identity::Keypair::generate_ed25519();

        let mut manager = ConnectionManager::new(ConnectionPolicy::new(32, 32), 32);
        let _ = manager.set_trust(
            TrustSources::new(
                PeerTrustPolicy::new(data_plane.iter().cloned()).expect("small allowlist"),
                InfrastructureSet::new(infrastructure.iter().cloned()).expect("small"),
            ),
            &[],
        );
        let decisions = Decisions::default();
        let outbound = Observing::new(
            OutboundAdmission::new(
                manager.handle(),
                InFlightTickets::default(),
                tokio::time::Instant::now(),
            ),
            decisions.clone(),
        );

        let swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                tcp::Config::default().nodelay(true),
                noise::Config::new,
                yamux::Config::default,
            )
            .expect("tcp")
            .with_relay_client(noise::Config::new, yamux::Config::default)
            .expect("relay client transport")
            .with_behaviour(|key, relay_client| ProductionBehaviour {
                outbound,
                identify: identify::Behaviour::new(identify::Config::new(
                    "/interweave-spike/1.0.0".to_owned(),
                    key.public(),
                )),
                relay_client,
            })
            .expect("behaviour")
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(30)))
            .build();

        Self {
            swarm,
            failures: Vec::new(),
            outgoing_errors: 0,
            connected: 0,
            dialing: 0,
            other: Vec::new(),
            decisions,
            manager,
        }
    }

    /// Listen on loopback and return the bound address.
    pub async fn listen(&mut self) -> Multiaddr {
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/0".parse().expect("loopback");
        self.swarm.listen_on(addr).expect("listen accepted");
        loop {
            if let libp2p::swarm::SwarmEvent::NewListenAddr { address, .. } =
                futures::StreamExt::select_next_some(&mut self.swarm).await
            {
                return address;
            }
        }
    }

    /// Drain whatever the swarm has ready, recording refusals.
    pub fn drain(&mut self, cx: &mut std::task::Context<'_>) {
        use futures::StreamExt;
        while let std::task::Poll::Ready(Some(event)) = self.swarm.poll_next_unpin(cx) {
            match event {
                libp2p::swarm::SwarmEvent::OutgoingConnectionError { error, .. } => {
                    self.outgoing_errors += 1;
                    self.failures.push(format!("{error}"));
                }
                libp2p::swarm::SwarmEvent::ConnectionEstablished { .. } => {
                    self.connected += 1;
                }
                libp2p::swarm::SwarmEvent::Dialing { .. } => {
                    self.dialing += 1;
                }
                libp2p::swarm::SwarmEvent::ListenerError { error, .. } => {
                    self.failures.push(format!("listener: {error}"));
                }
                libp2p::swarm::SwarmEvent::ListenerClosed { reason, .. } => {
                    self.failures.push(format!("listener closed: {reason:?}"));
                }
                other => {
                    self.other.push(format!("{other:?}"));
                }
            }
        }
    }
}
