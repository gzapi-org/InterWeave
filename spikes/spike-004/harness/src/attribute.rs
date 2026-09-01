// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! Attributing a behaviour-originated dial to the behaviour that made
//! it.
//!
//! # The problem this measures
//!
//! `OutboundAdmission::handle_pending_outbound_connection` is handed a
//! `ConnectionId`, an `Option<PeerId>` and an EMPTY address list. It is
//! not told which behaviour asked. Production gets away with that today
//! because Kademlia is the only behaviour that can originate a dial, so
//! "no ticket" and "Kademlia" are the same set — a fact a test in
//! `outbound_gate.rs` pins by parsing the root manifest's libp2p
//! feature list, precisely so that it FAILS when Stage 11 adds a
//! second dialling behaviour.
//!
//! Stage 11 adds three. Without attribution every AutoNAT probe, relay
//! reservation and hole-punch would be admitted as
//! `DialOrigin::KademliaQuery`, which is data-plane — so an
//! infrastructure-only relay would be refused (`is_data_plane()` is
//! true for KademliaQuery and `ConnectionPolicy::admit` refuses a
//! data-plane origin for that class), and the whole reachability stack
//! would fail closed against exactly the peers it exists to use.
//!
//! # The mechanism under test
//!
//! A behaviour computes its own `ConnectionId` — `DialOpts::build`
//! allocates it and `opts.connection_id()` reads it back, which is how
//! `libp2p-autonat`'s server correlates its dial-back
//! (`v2/server/behaviour.rs`: `let conn_id = opts.connection_id();`
//! then `ToSwarm::Dial`). So a wrapper that sees its inner behaviour's
//! `ToSwarm::Dial` on the way past can record `connection_id -> origin`
//! BEFORE the Swarm acts on it.
//!
//! Whether that ordering actually holds — whether the note is always
//! written before the gate reads it — is not something to assume. It is
//! what experiment E2 measures, by counting attributed dials against
//! behaviour-originated dials and failing on any gap.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use interweave_transport_runtime::DialOrigin;
use libp2p::Multiaddr;
use libp2p::core::Endpoint;
use libp2p::core::transport::PortUse;
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler, THandlerInEvent,
    THandlerOutEvent, ToSwarm,
};
use libp2p::PeerId;

/// What each behaviour-originated dial was for, keyed by the
/// `ConnectionId` the originating behaviour minted.
#[derive(Debug, Clone, Default)]
pub struct Attribution {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug, Default)]
struct Inner {
    /// Live notes: a dial announced but not yet seen by the gate.
    notes: BTreeMap<ConnectionId, DialOrigin>,
    /// Every note ever written, by origin — so a note the gate consumed
    /// is still countable afterwards.
    announced: BTreeMap<&'static str, u64>,
    /// Notes the gate looked for and found.
    resolved: BTreeMap<&'static str, u64>,
    /// Dials the gate met with no note at all. THE FAILURE MODE: each
    /// one is a dial production would misattribute to Kademlia.
    unattributed: u64,
    /// Who each announced dial was aimed at, by origin. Needed because
    /// "no circuit dial happened" cannot be shown from origin counts
    /// alone: a relay-client dial TO the relay and one TOWARD a
    /// destination are the same origin under a regressed classifier.
    targets: BTreeMap<&'static str, Vec<PeerId>>,
}

fn label(origin: DialOrigin) -> &'static str {
    match origin {
        DialOrigin::Manual => "manual",
        DialOrigin::ConnectionManager => "connection-manager",
        DialOrigin::DiscoveryReconnect => "discovery-reconnect",
        DialOrigin::KademliaQuery => "kademlia-query",
        DialOrigin::RelayReservation => "relay-reservation",
        DialOrigin::RelayCircuit => "relay-circuit",
        DialOrigin::AutonatProbe => "autonat-probe",
        DialOrigin::DcutrHolePunch => "dcutr-hole-punch",
    }
}

impl Attribution {
    /// Record that `origin` is about to dial `peer` under `id`.
    pub fn announce(&self, id: ConnectionId, origin: DialOrigin, peer: Option<PeerId>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.notes.insert(id, origin);
        *inner.announced.entry(label(origin)).or_insert(0) += 1;
        if let Some(peer) = peer {
            inner
                .targets
                .entry(label(origin))
                .or_default()
                .push(peer);
        }
    }

    /// Peers dialled under `origin`.
    #[must_use]
    pub fn targets(&self, origin: DialOrigin) -> Vec<PeerId> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .targets
            .get(label(origin))
            .cloned()
            .unwrap_or_default()
    }

    /// The origin of the dial `id`, if a behaviour announced it.
    ///
    /// Consuming, because a `ConnectionId` names one dial attempt and a
    /// note that outlived its dial would attribute the NEXT one.
    #[must_use]
    pub fn resolve(&self, id: ConnectionId) -> Option<DialOrigin> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match inner.notes.remove(&id) {
            Some(origin) => {
                *inner.resolved.entry(label(origin)).or_insert(0) += 1;
                Some(origin)
            }
            None => {
                inner.unattributed += 1;
                None
            }
        }
    }

    /// Dials announced, by origin.
    #[must_use]
    pub fn announced(&self) -> BTreeMap<&'static str, u64> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .announced
            .clone()
    }

    /// Dials the gate resolved, by origin.
    #[must_use]
    pub fn resolved(&self) -> BTreeMap<&'static str, u64> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .resolved
            .clone()
    }

    /// Dials the gate met with no note. Every one is a misattribution
    /// in production.
    #[must_use]
    pub fn unattributed(&self) -> u64 {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .unattributed
    }

    /// Forget a note whose dial never reached the gate.
    ///
    /// Review finding on PR #69: the Swarm can reject an emitted
    /// `ToSwarm::Dial` BEFORE calling the pending hook — a false
    /// `PeerCondition` is the ordinary way — and then the gate never
    /// resolves the note. Left alone it stays in `notes` for the life
    /// of the process, one entry per rejected dial, which is the
    /// unbounded map this mechanism must not become. `DialFailure`
    /// carries the same `ConnectionId`, so the wrapper that wrote the
    /// note is also told when to drop it.
    ///
    /// Not counted as unattributed: the gate never met this dial, so
    /// nothing was misattributed. Silent by design, and R2.8 is what
    /// notices if the cleanup stops working.
    pub fn forget(&self, id: ConnectionId) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .notes
            .remove(&id);
    }

    /// Notes written and never consumed — a dial the Swarm dropped
    /// before the gate saw it, or a leak in this mechanism.
    #[must_use]
    pub fn outstanding(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .notes
            .len()
    }
}

/// How a wrapper decides the origin of one dial.
///
/// A FUNCTION OF THE DIAL, not a constant per behaviour. Review finding
/// on PR #69: the first version gave each wrapper one fixed origin,
/// which cannot be right for `relay::client::Behaviour` — the same
/// behaviour is responsible for reservations AND circuits, and
/// `DialOrigin` distinguishes them (`RelayReservation` vs
/// `RelayCircuit`) precisely because the production policy may treat
/// them differently. A constant meant `RelayCircuit` could never reach
/// the gate at all.
///
/// The classifier is handed what the announcement can actually see:
/// `DialOpts::get_peer_id`. It is NOT handed the addresses —
/// `DialOpts::get_addresses` is `pub(crate)` in libp2p-swarm 0.47.1, so
/// a `/p2p-circuit` suffix is not a signal available here. That
/// constraint is a finding in itself, and it is why production's
/// classifier will have to key on what it CONFIGURED — which peers are
/// its relays — rather than on what the dial looks like.
pub type Classifier = Arc<dyn Fn(Option<PeerId>) -> DialOrigin + Send + Sync>;

/// A classifier that always answers the same way.
#[must_use]
pub fn always(origin: DialOrigin) -> Classifier {
    Arc::new(move |_| origin)
}

/// Wraps one behaviour and announces the dials it emits.
///
/// Transparent in every other respect: the same events, the same
/// handlers, the same `ToSwarm` — the only thing it does is write the
/// note on the way past.
pub struct Attributing<B> {
    inner: B,
    classify: Classifier,
    attribution: Attribution,
}

impl<B> Attributing<B> {
    pub fn new(inner: B, classify: Classifier, attribution: Attribution) -> Self {
        Self {
            inner,
            classify,
            attribution,
        }
    }

    pub fn inner_mut(&mut self) -> &mut B {
        &mut self.inner
    }

    pub fn inner_ref(&self) -> &B {
        &self.inner
    }
}

impl<B: NetworkBehaviour> NetworkBehaviour for Attributing<B> {
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
        self.inner.handle_pending_inbound_connection(id, local, remote)
    }

    fn handle_pending_outbound_connection(
        &mut self,
        id: ConnectionId,
        peer: Option<PeerId>,
        addresses: &[Multiaddr],
        role: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        self.inner
            .handle_pending_outbound_connection(id, peer, addresses, role)
    }

    fn on_swarm_event(&mut self, event: FromSwarm<'_>) {
        if let FromSwarm::DialFailure(failure) = &event {
            self.attribution.forget(failure.connection_id);
        }
        // A dial that failed never reaches the gate if the Swarm
        // rejected it before the pending hook, so the note it left
        // has to be dropped here or it is never dropped at all.
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
        let polled = self.inner.poll(cx);
        // THE NOTE IS WRITTEN HERE, before the Swarm ever sees the
        // dial. `DialOpts` already carries the `ConnectionId` the
        // Swarm will use — `opts.connection_id()` is the same value
        // `handle_pending_outbound_connection` is later handed — so
        // this is a correlation, not a guess.
        if let Poll::Ready(ToSwarm::Dial { opts }) = &polled {
            let peer = opts.get_peer_id();
            let origin = (self.classify)(peer);
            self.attribution
                .announce(opts.connection_id(), origin, peer);
        }
        polled
    }
}
