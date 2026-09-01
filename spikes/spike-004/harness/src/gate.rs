// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! The production root admission, instrumented, with attribution.
//!
//! This is SPIKE-003's gate with one thing added and one thing
//! removed. Added: the origin of a behaviour dial is looked up in
//! [`Attribution`] instead of being assumed to be Kademlia. Removed:
//! the `DenyUnadmitted` mode, because Stage 10 settled that question —
//! a behaviour dial is admitted through `PolicySnapshot::admit`, and
//! what SPIKE-004 has to answer is whether it can be admitted under the
//! RIGHT origin now that four behaviours can originate one.
//!
//! It asks `ConnectionManager::admit` (through a `SnapshotHandle`)
//! rather than `ConnectionPolicy::admit` directly, for the reason
//! SPIKE-003 recorded: the policy answers trust, backoff, quarantine
//! and drain, while the pending-dial and connection CEILINGS live one
//! layer up in the manager, which is also what mints the ticket that
//! reserves them. A gate that called the policy would refuse an
//! untrusted peer correctly and let every dial past the limits.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Instant;

use interweave_transport_api::TransportIdentity;
use interweave_transport_runtime::{
    ConnectionManager, ConnectionSlot, DialOrigin, DialRequest, DialTicket, SnapshotHandle,
};
use libp2p::Multiaddr;
use libp2p::PeerId;
use libp2p::core::Endpoint;
use libp2p::core::transport::PortUse;
use libp2p::swarm::dummy;
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler, THandlerInEvent,
    THandlerOutEvent, ToSwarm,
};

use crate::attribute::Attribution;

/// What the gate saw.
#[derive(Debug, Clone, Default)]
pub struct DialLedger {
    inner: Arc<Mutex<LedgerInner>>,
}

#[derive(Debug, Default)]
struct LedgerInner {
    /// Dials carrying a root admission ticket (the command path).
    admitted_by_ticket: u64,
    /// Dials with no ticket — behaviour-originated, by definition.
    behaviour_originated: u64,
    /// Of those, how many were allowed, by the origin they were
    /// admitted UNDER.
    allowed_by_origin: BTreeMap<&'static str, u64>,
    /// Of those, why the rest were refused.
    refusals: BTreeMap<String, u64>,
    /// Addresses the pending hook was handed, per dial — F9 says this
    /// is empty for a behaviour dial, and the spike records it rather
    /// than repeating the claim.
    pending_address_counts: Vec<usize>,
    /// Addresses seen at the ESTABLISHED hook, where they do exist.
    established_addresses: Vec<String>,
}

impl DialLedger {
    fn lock(&self) -> std::sync::MutexGuard<'_, LedgerInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[must_use]
    pub fn behaviour_originated(&self) -> u64 {
        self.lock().behaviour_originated
    }

    #[must_use]
    pub fn admitted_by_ticket(&self) -> u64 {
        self.lock().admitted_by_ticket
    }

    #[must_use]
    pub fn allowed_by_origin(&self) -> BTreeMap<&'static str, u64> {
        self.lock().allowed_by_origin.clone()
    }

    #[must_use]
    pub fn refusals(&self) -> BTreeMap<String, u64> {
        self.lock().refusals.clone()
    }

    #[must_use]
    pub fn pending_address_counts(&self) -> Vec<usize> {
        self.lock().pending_address_counts.clone()
    }

    #[must_use]
    pub fn established_addresses(&self) -> Vec<String> {
        self.lock().established_addresses.clone()
    }
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

/// The root gate, as the product would run it once attribution exists.
pub struct InstrumentedGate {
    ledger: DialLedger,
    admission: SnapshotHandle,
    attribution: Attribution,
    /// Tickets held for in-flight behaviour dials, so the slots they
    /// reserved are released when the dial settles rather than leaking
    /// one ceiling slot per probe.
    held: Arc<Mutex<BTreeMap<ConnectionId, DialTicket>>>,
    /// Connection reservations kept for behaviour dials that
    /// ESTABLISHED. A `DialTicket`'s `Drop` releases the pending slot
    /// AND the connection it may become, so converting it through
    /// `record_success` is what keeps `max_connections` meaningful for
    /// behaviour-originated connections.
    connections: Arc<Mutex<BTreeMap<ConnectionId, ConnectionSlot>>>,
    /// The manager, for the SETTLE path only — never for the decision,
    /// which ADR-0011 forbids blocking on inside the Swarm poll.
    manager: Arc<Mutex<ConnectionManager>>,
    started: Instant,
}

impl InstrumentedGate {
    #[must_use]
    pub fn new(
        admission: SnapshotHandle,
        manager: Arc<Mutex<ConnectionManager>>,
        attribution: Attribution,
    ) -> Self {
        Self {
            ledger: DialLedger::default(),
            admission,
            attribution,
            held: Arc::new(Mutex::new(BTreeMap::new())),
            connections: Arc::new(Mutex::new(BTreeMap::new())),
            manager,
            started: Instant::now(),
        }
    }

    #[must_use]
    pub fn ledger(&self) -> DialLedger {
        self.ledger.clone()
    }

    #[must_use]
    pub fn pending_behaviour_dials(&self) -> usize {
        self.held.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    #[must_use]
    pub fn held_connections(&self) -> usize {
        self.connections
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn record_refusal(&self, why: impl Into<String>) {
        let mut l = self.ledger.lock();
        *l.refusals.entry(why.into()).or_insert(0) += 1;
    }
}

impl NetworkBehaviour for InstrumentedGate {
    type ConnectionHandler = dummy::ConnectionHandler;
    type ToSwarm = std::convert::Infallible;

    fn handle_established_inbound_connection(
        &mut self,
        _id: ConnectionId,
        _peer: PeerId,
        _local: &Multiaddr,
        _remote: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(dummy::ConnectionHandler)
    }

    fn handle_established_outbound_connection(
        &mut self,
        _id: ConnectionId,
        _peer: PeerId,
        addr: &Multiaddr,
        _role: Endpoint,
        _port: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        // WHERE THE ADDRESS EXISTS. The pending hook is handed an empty
        // list for a behaviour dial (F9), so an address-scoped decision
        // can only be made here. Recorded rather than asserted.
        self.ledger.lock().established_addresses.push(addr.to_string());
        Ok(dummy::ConnectionHandler)
    }

    fn handle_pending_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: Option<PeerId>,
        addresses: &[Multiaddr],
        _role: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        // ATTRIBUTION FIRST, and consuming — a note names one dial.
        // A dial with no note is the failure this experiment hunts:
        // production would call it `KademliaQuery`, which is
        // data-plane, and refuse it for an infrastructure-only peer.
        let attributed = self.attribution.resolve(connection_id);

        {
            let mut l = self.ledger.lock();
            l.behaviour_originated += 1;
            l.pending_address_counts.push(addresses.len());
        }

        let Some(origin) = attributed else {
            self.record_refusal("no attribution: origin unknown");
            return Err(ConnectionDenied::new(std::io::Error::other(
                "a behaviour dial with no announced origin cannot be classified",
            )));
        };

        let Some(target) = peer else {
            self.record_refusal("dial names no peer");
            return Err(ConnectionDenied::new(std::io::Error::other(
                "a behaviour dial that names no peer cannot be classified",
            )));
        };
        let identity = TransportIdentity::parse(target.to_base58())
            .expect("a libp2p PeerId is a canonical identity");

        let request = DialRequest {
            peer: Some(identity),
            // EMPTY ON PURPOSE, and it is what F9 is about: the address
            // does not exist yet at this hook, so a placeholder is
            // honest and `handle_established_outbound_connection` is
            // where the address-scoped question gets asked.
            address: String::new(),
            origin,
        };

        match self.admission.admit(&request, self.now_ms()) {
            Ok(ticket) => {
                self.held
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(connection_id, ticket);
                let mut l = self.ledger.lock();
                *l.allowed_by_origin.entry(label(origin)).or_insert(0) += 1;
                Ok(Vec::new())
            }
            Err(denial) => {
                self.record_refusal(format!("{denial:?}"));
                Err(ConnectionDenied::new(std::io::Error::other(format!(
                    "root dial admission refused: {denial:?}"
                ))))
            }
        }
    }

    fn on_swarm_event(&mut self, event: FromSwarm<'_>) {
        let now = self.now_ms();
        match event {
            FromSwarm::ConnectionEstablished(e) => {
                let ticket = self
                    .held
                    .lock()
                    .unwrap_or_else(|x| x.into_inner())
                    .remove(&e.connection_id);
                if let Some(ticket) = ticket {
                    let slot = self
                        .manager
                        .lock()
                        .unwrap_or_else(|x| x.into_inner())
                        .record_success(ticket, now);
                    self.connections
                        .lock()
                        .unwrap_or_else(|x| x.into_inner())
                        .insert(e.connection_id, slot);
                }
            }
            FromSwarm::ConnectionClosed(e) => {
                self.connections
                    .lock()
                    .unwrap_or_else(|x| x.into_inner())
                    .remove(&e.connection_id);
            }
            FromSwarm::DialFailure(e) => {
                let ticket = self
                    .held
                    .lock()
                    .unwrap_or_else(|x| x.into_inner())
                    .remove(&e.connection_id);
                if let Some(ticket) = ticket {
                    self.manager
                        .lock()
                        .unwrap_or_else(|x| x.into_inner())
                        .record_failure(ticket, now);
                }
            }
            _ => {}
        }
    }

    fn on_connection_handler_event(
        &mut self,
        _peer: PeerId,
        _id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        libp2p::core::util::unreachable(event);
    }

    fn poll(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        Poll::Pending
    }
}
