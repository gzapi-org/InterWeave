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
use std::time::Instant;

use interweave_transport_api::TransportIdentity;
use interweave_transport_runtime::{
    ConnectionClass, ConnectionManager, ConnectionSlot, DialDenial, DialOrigin, DialRequest,
    DialTicket, SnapshotHandle,
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
    /// Behaviour connections retained after reclassification.
    retained: u64,
    /// Behaviour connections dropped because authority had been
    /// withdrawn between admission and the completed handshake.
    withdrawn: u64,
    /// How many candidate addresses each behaviour dial offered the
    /// hook. Zero means the hook cannot see where the dial is going.
    offered_addresses: Vec<usize>,
    /// Behaviour connections refused on the address they actually used.
    address_refusals: u64,
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

    /// Behaviour connections kept after reclassification at settlement.
    #[must_use]
    pub fn retained(&self) -> u64 {
        self.lock().retained
    }

    /// Behaviour connections refused at settlement because trust had
    /// been withdrawn since admission.
    #[must_use]
    pub fn withdrawn(&self) -> u64 {
        self.lock().withdrawn
    }

    /// Candidate-address counts, one per behaviour dial.
    #[must_use]
    pub fn offered_addresses(&self) -> Vec<usize> {
        self.lock().offered_addresses.clone()
    }

    /// Behaviour connections refused on the address they actually used.
    #[must_use]
    pub fn address_refusals(&self) -> u64 {
        self.lock().address_refusals
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
    /// Connection slots held for behaviour dials that ESTABLISHED, until
    /// the connection closes.
    ///
    /// A `DialTicket` reserves a pending slot AND the connection it may
    /// become; its `Drop` releases both. So dropping the ticket when a
    /// dial succeeds returns the connection reservation immediately, and
    /// `max_connections` then counts no behaviour-originated connection
    /// at all — the ceiling exists and bounds nothing. `record_success`
    /// converts the ticket into a `ConnectionSlot` that keeps the
    /// reservation, which is why the manager is here as well as the
    /// handle.
    connections: Arc<Mutex<BTreeMap<ConnectionId, ConnectionSlot>>>,
    /// The manager, for the SETTLE path only.
    ///
    /// The dial DECISION never takes this lock — ADR-0011 forbids the
    /// gate blocking on the policy inside the Swarm poll, and that is
    /// what `admission` is for. `on_swarm_event` is not that path: it
    /// reports an outcome that has already happened, and recording it is
    /// what the manager's own API requires `&mut` for.
    manager: Arc<Mutex<ConnectionManager>>,
    /// The gate's clock origin.
    ///
    /// `now_ms` used to be a field pinned at zero, so every admission
    /// and every settlement was timestamped at the same instant: a
    /// backoff recorded at 0 with a 30-second delay expired at 30_000
    /// and the clock never reached it, which made `PeerBackoff`
    /// permanent instead of temporary. Every experiment that asserted
    /// the immediate refusal still passed. Elapsed real time is what the
    /// runtime hands the policy, so it is what this hands it too.
    started: Instant,
}

impl InstrumentedGate {
    /// Build a gate in `mode` over one manager.
    #[must_use]
    pub fn new(
        mode: Mode,
        admission: SnapshotHandle,
        manager: Arc<Mutex<ConnectionManager>>,
    ) -> Self {
        Self {
            admitted: AdmittedDials::default(),
            ledger: DialLedger::default(),
            mode,
            admission,
            held: Arc::new(Mutex::new(BTreeMap::new())),
            connections: Arc::new(Mutex::new(BTreeMap::new())),
            manager,
            started: Instant::now(),
        }
    }

    /// The gate's current clock reading, for an experiment that needs
    /// to assert the clock ADVANCES rather than trusting that it does.
    #[must_use]
    pub fn clock_ms(&self) -> u64 {
        self.now_ms()
    }

    /// Milliseconds since this gate was built.
    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Behaviour dials currently IN FLIGHT — the live count, not a total.
    ///
    /// This is `SnapshotResult::pending_behaviour_dials`. The ledger's
    /// `behaviour_originated` is cumulative and would report settled
    /// dials as pending, which is a materially wrong diagnostic rather
    /// than an imprecise one.
    #[must_use]
    pub fn pending_behaviour_dials(&self) -> usize {
        self.held.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Connections this gate holds a reservation for.
    #[must_use]
    pub fn held_connections(&self) -> usize {
        self.connections
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
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

        // WHAT THIS HOOK CAN AND CANNOT DECIDE — finding F9.
        //
        // For a behaviour-originated dial libp2p calls this hook with an
        // EMPTY address list: the hook exists so each behaviour can
        // CONTRIBUTE addresses, and the union is dialled after it
        // returns. Measured, not assumed — K21 records the count the
        // hook was given, and it is zero.
        //
        // So peer-scoped and global policy can be decided here — trust,
        // per-peer backoff, drain, the pending and connection ceilings —
        // and ADDRESS-scoped policy cannot, because the address does not
        // exist yet. A gate that checked `addresses.first()` was not
        // merely incomplete; it was reading an empty list and admitting
        // every quarantined route.
        //
        // The address check therefore moves to
        // `handle_established_outbound_connection`, which is handed the
        // address that was actually used. That is after TCP connect and
        // before the handler exists, so a quarantined route costs a
        // connect and is then refused — later than production's check,
        // and the only place a behaviour dial has one at all.
        //
        // The loop below stays because a dial that DOES arrive with
        // candidates must still cross the policy, and it is written
        // all-or-nothing since the hook cannot restrict the set.
        let now = self.now_ms();
        self.ledger.lock().offered_addresses.push(addresses.len());
        // NORMALIZED THE WAY THE POLICY IS KEYED. A behaviour dial's
        // candidate arrives as `/ip4/…/tcp/…/p2p/<peer>` — a query
        // result carries the peer component — while the address book and
        // the quarantine map are keyed by the bare transport address,
        // which is what `AdmittedDial` binds. Passing the suffixed form
        // to `admit` looks up an address the policy has never seen, so
        // every quarantine silently misses and the dial is admitted on a
        // route the policy had suppressed. Finding F10.
        let candidates: Vec<String> = if addresses.is_empty() {
            vec![String::new()]
        } else {
            addresses.iter().map(|a| normalize(a)).collect()
        };
        for candidate in &candidates {
            let probe = DialRequest {
                peer: Some(identity.clone()),
                address: candidate.clone(),
                origin: DialOrigin::KademliaQuery,
            };
            // The probe's ticket is dropped immediately: this asks
            // whether the ADDRESS is admissible, and the reservation for
            // the dial itself is taken once, below.
            if let Err(denial) = self.admission.admit(&probe, now) {
                self.record_refusal(&denial_name(denial));
                return Err(ConnectionDenied::new(std::io::Error::other(format!(
                    "kademlia dial refused: {denial:?} for {candidate}"
                ))));
            }
        }
        let request = DialRequest {
            peer: Some(identity),
            address: candidates
                .first()
                .cloned()
                .unwrap_or_default(),
            origin: DialOrigin::KademliaQuery,
        };
        // THE MANAGER, so the ceilings are consulted and the ticket that
        // reserves them exists. The class is not passed: the manager
        // classifies from its own trust policy, which is the point —
        // a caller cannot assert a class it does not have.
        let decision = self.admission.admit(&request, self.now_ms());
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

    /// The ADDRESS check for a behaviour dial, which
    /// `handle_pending_outbound_connection` could not make.
    ///
    /// # Errors
    /// [`ConnectionDenied`] when the address this dial actually used is
    /// one the policy suppresses. Only behaviour dials are checked here:
    /// an admitted dial bound its address into the `DialOpts` and was
    /// decided before it was made.
    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        _role_override: Endpoint,
        _port_use: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        let ours = self
            .held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&connection_id);
        if !ours {
            return Ok(dummy::ConnectionHandler);
        }
        let Ok(identity) = TransportIdentity::parse(peer.to_base58()) else {
            return Ok(dummy::ConnectionHandler);
        };
        let request = DialRequest {
            peer: Some(identity),
            address: normalize(addr),
            origin: DialOrigin::KademliaQuery,
        };
        // A PROBE, and the CAPACITY answers must be discarded from it.
        // `admit` decides policy AND takes a reservation, and this dial
        // already holds one — its ticket. So a probe issued while the
        // ceiling is full is refused for capacity that this very dial is
        // occupying, and refusing on that would deny every behaviour
        // connection at a tight ceiling. Measured, not predicted: it
        // broke K19.9, where `max_connections` is one.
        //
        // What this hook is for is the ADDRESS, plus the late checks
        // that can genuinely have changed since admission. Capacity is
        // not one of them; it was decided when the ticket was minted.
        let verdict = self.admission.admit(&request, self.now_ms());
        match verdict {
            Ok(probe) => {
                drop(probe);
                Ok(dummy::ConnectionHandler)
            }
            Err(
                DialDenial::TooManyPendingDials
                | DialDenial::ConnectionLimitReached
                | DialDenial::PolicyStateFull
                | DialDenial::PolicySuperseded,
            ) => Ok(dummy::ConnectionHandler),
            Err(denial) => {
                self.record_refusal(&denial_name(denial));
                self.ledger.lock().address_refusals += 1;
                Err(ConnectionDenied::new(std::io::Error::other(format!(
                    "kademlia connection refused on its address: {denial:?}"
                ))))
            }
        }
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
        match settled {
            Some((id, true)) => {
                // ESTABLISHED: the ticket becomes a `ConnectionSlot`,
                // which KEEPS the connection reservation. Dropping the
                // ticket here instead would return it, and
                // `max_connections` would count no behaviour-originated
                // connection at all.
                if let Some(ticket) = self
                    .held
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id)
                {
                    let now = self.now_ms();
                    let mut m = self.manager.lock().unwrap_or_else(|e| e.into_inner());
                    // RECLASSIFIED at settlement, not trusted from
                    // admission. Trust can be revoked between the
                    // admission and the completed handshake, and
                    // `record_success` would then retain a connection
                    // under authority that no longer exists. The
                    // production settlement path reclassifies for
                    // exactly this race and has a distinct method for
                    // it.
                    let still_authorized = ticket
                        .peer()
                        .is_some_and(|p| m.classify(p) == ConnectionClass::DataPlaneTrusted);
                    if still_authorized {
                        let slot = m.record_success(ticket, now);
                        drop(m);
                        self.connections
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(id, slot);
                        self.ledger.lock().retained += 1;
                    } else {
                        m.record_authorization_withdrawn(ticket, now);
                        self.ledger.lock().withdrawn += 1;
                    }
                }
            }
            Some((id, false)) => {
                // FAILED, or a connection this gate held closing. A
                // failed dial goes back through the manager so backoff
                // and the reconnect schedule see it; a closing
                // connection returns its slot.
                if let Some(ticket) = self
                    .held
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id)
                {
                    self.manager
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .record_failure(ticket, self.now_ms());
                }
                if let Some(slot) = self
                    .connections
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id)
                {
                    self.manager
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .record_connection_closed(slot);
                }
            }
            None => {}
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

/// Strip the `/p2p/<peer>` component, leaving the transport address.
///
/// The policy is keyed by the address a dial actually connects to, and
/// production reaches it through `AdmittedDial`, which binds the peer
/// into `DialOpts` separately from the address. A behaviour dial's
/// candidates arrive with the peer already appended, so without this the
/// two halves of the same system key the same route differently.
fn normalize(address: &Multiaddr) -> String {
    let stripped: Multiaddr = address
        .iter()
        .filter(|p| !matches!(p, libp2p::multiaddr::Protocol::P2p(_)))
        .collect();
    stripped.to_string()
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
