// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The instrumented dial boundary.
//!
//! # Why this exists rather than reading `SwarmEvent::Dialing`
//!
//! SPIKE-003's brief forbids inferring behaviour-originated dial volume
//! from the absence of ordinary scheduler calls. `SwarmEvent::Dialing`
//! reports that a dial happened, not who asked for it, and a
//! `ToSwarm::Dial` emitted by Kademlia is indistinguishable there from
//! one the application made.
//!
//! libp2p routes EVERY dial through
//! `NetworkBehaviour::handle_pending_outbound_connection`, synchronously
//! inside `Swarm::dial`, which is exactly the hook the production
//! [`OutboundAdmission`] uses. This behaviour is that hook with
//! counters and a switchable policy, so the same code path that carries
//! the product's guarantee carries the measurement.
//!
//! # The two modes, and why both are measured
//!
//! `Mode::DenyUnadmitted` is what production does TODAY: any dial whose
//! connection id carries no root admission ticket is refused. That is
//! correct while nothing behaviour-originated dials, and Stage 10 is the
//! change that makes something.
//!
//! `Mode::PolicyAdmit` is the Stage 10 proposal: a behaviour-originated
//! dial is admitted through the real
//! [`PolicySnapshot::admit`](interweave_transport_runtime::PolicySnapshot)
//! under `DialOrigin::KademliaQuery`, so trust, per-peer backoff,
//! shutdown state and the pending/connection limits all apply to it.
//! Measuring only the first would say the gate refuses everything;
//! measuring only the second would say nothing about what ships now.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use interweave_transport_api::TransportIdentity;
use interweave_transport_runtime::{
    DialDenial, DialOrigin, DialRequest, DialTicket, SnapshotHandle,
};
use interweave_transport_libp2p::outbound_gate::AdmittedDials;
use libp2p::PeerId;
use libp2p::core::transport::PortUse;
use libp2p::core::{Endpoint, Multiaddr};
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler, THandlerInEvent,
    THandlerOutEvent, ToSwarm, dummy,
};

/// How the gate answers a dial carrying no root admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Production today: refuse it.
    DenyUnadmitted,
    /// Stage 10's proposal: admit it through the real policy.
    PolicyAdmit,
}

/// What the gate saw, by origin.
#[derive(Debug, Clone, Default)]
pub struct DialLedger {
    inner: Arc<Mutex<LedgerInner>>,
}

#[derive(Debug, Default)]
struct LedgerInner {
    /// Dials carrying a root admission ticket.
    admitted_by_ticket: u64,
    /// Dials with no ticket — behaviour-originated, by definition.
    behaviour_originated: u64,
    /// Of those, how many the gate allowed.
    behaviour_allowed: u64,
    /// Of those, why the rest were refused.
    refusals: BTreeMap<String, u64>,
    /// The peers behaviour-originated dials were aimed at.
    behaviour_targets: Vec<PeerId>,
}

impl DialLedger {
    fn lock(&self) -> std::sync::MutexGuard<'_, LedgerInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Dials that came through the root admission.
    #[must_use]
    pub fn admitted_by_ticket(&self) -> u64 {
        self.lock().admitted_by_ticket
    }

    /// Dials a `NetworkBehaviour` originated.
    #[must_use]
    pub fn behaviour_originated(&self) -> u64 {
        self.lock().behaviour_originated
    }

    /// Behaviour-originated dials the gate let through.
    #[must_use]
    pub fn behaviour_allowed(&self) -> u64 {
        self.lock().behaviour_allowed
    }

    /// How many behaviour dials were refused for each reason.
    #[must_use]
    pub fn refusals(&self) -> BTreeMap<String, u64> {
        self.lock().refusals.clone()
    }

    /// Every peer a behaviour dial was aimed at, in order.
    #[must_use]
    pub fn behaviour_targets(&self) -> Vec<PeerId> {
        self.lock().behaviour_targets.clone()
    }

    /// Forget everything, so one experiment cannot read another's count.
    pub fn reset(&self) {
        *self.lock() = LedgerInner::default();
    }
}

/// The instrumented gate.
pub struct InstrumentedGate {
    admitted: AdmittedDials,
    ledger: DialLedger,
    mode: Mode,
    /// THE WHOLE ROOT ADMISSION, not its policy half.
    ///
    /// `PolicySnapshot::admit` answers trust, backoff, quarantine and
    /// drain; the pending-dial and connection CEILINGS are enforced one
    /// layer up, in `ConnectionManager::admit`, which is also what mints
    /// the ticket that reserves them. A gate calling the policy directly
    /// therefore refuses an untrusted or backed-off peer correctly and
    /// lets every dial past the limits — which is exactly the release
    /// criterion this spike has to satisfy, so it asks the manager.
    ///
    /// Through a `SnapshotHandle` rather than a locked manager, because
    /// this runs synchronously inside the Swarm poll: ADR-0011's rule is
    /// that the gate must not block on the policy. The handle also
    /// retries a `PolicySuperseded` refusal, which makes a trust
    /// revision landing mid-dial a reload rather than a spurious denial.
    admission: SnapshotHandle,
    /// Tickets held for in-flight behaviour dials, so the slots they
    /// reserved are released when the dial settles rather than leaking
    /// one ceiling slot per query.
    held: Arc<Mutex<BTreeMap<ConnectionId, DialTicket>>>,
    now_ms: u64,
}

impl InstrumentedGate {
    /// Build a gate in `mode` over one manager's admission handle.
    #[must_use]
    pub fn new(mode: Mode, admission: SnapshotHandle) -> Self {
        Self {
            admitted: AdmittedDials::default(),
            ledger: DialLedger::default(),
            mode,
            admission,
            held: Arc::new(Mutex::new(BTreeMap::new())),
            now_ms: 0,
        }
    }

    /// Tickets this gate is holding for in-flight behaviour dials.
    ///
    /// Each one is reserving a pending-dial slot and a connection slot,
    /// so an experiment that wants to know whether a ceiling is actually
    /// consumed can ask.
    #[must_use]
    pub fn held_tickets(&self) -> usize {
        self.held.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// The counters this gate writes.
    #[must_use]
    pub fn ledger(&self) -> DialLedger {
        self.ledger.clone()
    }

    /// The admission set, shared with whatever issues tickets.
    #[must_use]
    pub fn admitted(&self) -> AdmittedDials {
        self.admitted.clone()
    }

    fn record_refusal(&self, reason: &str) {
        *self
            .ledger
            .lock()
            .refusals
            .entry(reason.to_owned())
            .or_default() += 1;
    }
}

impl NetworkBehaviour for InstrumentedGate {
    type ConnectionHandler = dummy::ConnectionHandler;
    type ToSwarm = std::convert::Infallible;

    fn handle_pending_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: Option<PeerId>,
        addresses: &[Multiaddr],
        _effective_role: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        if self.admitted.take(connection_id) {
            self.ledger.lock().admitted_by_ticket += 1;
            return Ok(Vec::new());
        }

        // NO TICKET means no root admission issued this dial, which is
        // the definition of behaviour-originated. Counted before any
        // decision, so a refusal is still a measured dial.
        {
            let mut l = self.ledger.lock();
            l.behaviour_originated += 1;
            if let Some(p) = peer {
                l.behaviour_targets.push(p);
            }
        }

        if self.mode == Mode::DenyUnadmitted {
            self.record_refusal("no root dial admission");
            return Err(ConnectionDenied::new(std::io::Error::other(
                "outbound connections require a root dial admission",
            )));
        }

        // STAGE 10's SHAPE: the same `PolicySnapshot::admit` an ordinary
        // dial passes, with the origin naming Kademlia so the denial an
        // operator sees says which subsystem asked.
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
            address: addresses
                .first()
                .map_or_else(String::new, std::string::ToString::to_string),
            origin: DialOrigin::KademliaQuery,
        };
        // THE MANAGER, so the ceilings are consulted and the ticket that
        // reserves them exists. The class is not passed: the manager
        // classifies from its own trust policy, which is the point —
        // a caller cannot assert a class it does not have.
        let decision = self.admission.admit(&request, self.now_ms);
        match decision {
            Ok(ticket) => {
                self.ledger.lock().behaviour_allowed += 1;
                // HELD, not dropped. Dropping it here would release the
                // pending and connection slots it just reserved, and the
                // ceilings would bound nothing.
                self.held
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(connection_id, ticket);
                Ok(Vec::new())
            }
            Err(denial) => {
                self.record_refusal(&denial_name(denial));
                Err(ConnectionDenied::new(std::io::Error::other(format!(
                    "kademlia dial refused: {denial:?}"
                ))))
            }
        }
    }

    fn handle_established_inbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        _peer: PeerId,
        _local_addr: &Multiaddr,
        _remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(dummy::ConnectionHandler)
    }

    fn handle_established_outbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        _peer: PeerId,
        _addr: &Multiaddr,
        _role_override: Endpoint,
        _port_use: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(dummy::ConnectionHandler)
    }

    /// Release a behaviour dial's ticket when the dial settles.
    ///
    /// Without this every query permanently consumes a pending-dial slot
    /// and the ceiling reaches zero after a few rounds — the ceilings
    /// would then "work" for the wrong reason, which is worse than not
    /// enforcing them.
    fn on_swarm_event(&mut self, event: FromSwarm<'_>) {
        let settled = match event {
            FromSwarm::ConnectionEstablished(e) => Some((e.connection_id, true)),
            FromSwarm::ConnectionClosed(e) => Some((e.connection_id, false)),
            FromSwarm::DialFailure(e) => Some((e.connection_id, false)),
            _ => None,
        };
        if let Some((id, _connected)) = settled {
            // DROPPED, which is what releases the reservation: an
            // unsettled `DialTicket` returns its pending slot and its
            // connection slot in `Drop`. The manager's `record_*`
            // methods need `&mut`, which a `NetworkBehaviour` hook does
            // not have — and they exist to update backoff and the
            // reconnect schedule, neither of which this gate is
            // measuring. What it measures is the CEILING, and the
            // ceiling is exactly what the drop restores.
            drop(
                self.held
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id),
            );
        }
    }

    fn on_connection_handler_event(
        &mut self,
        _peer: PeerId,
        _connection: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        match event {}
    }

    fn poll(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        Poll::Pending
    }
}

/// A stable name per denial, so the ledger keys do not depend on Debug.
fn denial_name(d: DialDenial) -> String {
    match d {
        DialDenial::ShuttingDown => "shutting down",
        DialDenial::Unauthorized => "unauthorized",
        DialDenial::NotAuthorizedForDataPlane => "not authorized for the data plane",
        DialDenial::PeerBackoff => "peer backoff",
        DialDenial::AddressQuarantined => "address quarantined",
        DialDenial::TooManyPendingDials => "too many pending dials",
        DialDenial::ConnectionLimitReached => "connection limit reached",
        DialDenial::PolicySuperseded => "policy superseded",
        DialDenial::PolicyStateFull => "policy state full",
    }
    .to_owned()
}
