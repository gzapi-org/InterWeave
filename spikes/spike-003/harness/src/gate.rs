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

/// What the gate believes is driving dials right now.
///
/// libp2p does NOT tell a behaviour which query caused a dial: the hook
/// receives a connection id, a peer and (for a behaviour dial) nothing
/// else. So per-class attribution — which the release criterion requires
/// — cannot be read off the dial. It has to come from the provider,
/// which knows what it started, and the gate attributes each dial to the
/// classes that were in flight when it happened.
///
/// That is exact when one class is active and a SET when several are,
/// which is a real limit rather than a modelling choice; K23 measures
/// both cases and the record states it.
#[derive(Debug, Clone, Default)]
pub struct ActiveClasses {
    inner: Arc<Mutex<BTreeMap<&'static str, usize>>>,
}

impl ActiveClasses {
    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<&'static str, usize>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The provider started a query of this class.
    pub fn started(&self, class: &'static str) {
        *self.lock().entry(class).or_insert(0) += 1;
    }

    /// One finished.
    pub fn finished(&self, class: &'static str) {
        if let Some(n) = self.lock().get_mut(class) {
            *n = n.saturating_sub(1);
        }
    }

    /// The classes in flight, as a stable label.
    #[must_use]
    pub fn label(&self) -> String {
        let live: Vec<&str> = self
            .lock()
            .iter()
            .filter(|(_, n)| **n > 0)
            .map(|(c, _)| *c)
            .collect();
        if live.is_empty() {
            "none".to_owned()
        } else {
            live.join("+")
        }
    }
}

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
    /// Behaviour dials by the query class(es) in flight when each
    /// happened. `none` means no query the provider told us about was
    /// running, which is itself evidence — that is the library's own
    /// work, and the brief requires it be counted rather than assumed
    /// absent.
    by_class: BTreeMap<String, u64>,
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
    /// The address each in-flight behaviour dial actually used, learned
    /// at the established hook — the first moment it exists.
    used_address: BTreeMap<ConnectionId, String>,
    /// Established connections closed because they could not be
    /// accounted for.
    unaccounted_closed: u64,
    /// Addresses a failed dial exhausted that could NOT be scored,
    /// because settling one needs a ticket and a ticket needs capacity.
    unsettled_addresses: u64,
    /// Address-scoped denials the dial hook declined to act on because
    /// it had no address to act about. Each is re-asked at the
    /// established hook.
    deferred_address_denials: u64,
    /// Behaviour dials refused because the RESERVATION could not be
    /// taken without answering an address question the hook had no
    /// address for. Fail-closed, and an availability cost.
    placeholder_blocked: u64,
    /// The OTHER addresses a failed multi-address dial exhausted. The
    /// ticket settles one; these are scored individually.
    also_failed: BTreeMap<ConnectionId, Vec<String>>,
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

    /// Behaviour-originated dial volume BY QUERY CLASS.
    #[must_use]
    pub fn by_class(&self) -> BTreeMap<String, u64> {
        self.lock().by_class.clone()
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

    /// Established connections closed because settlement could not
    /// account for them.
    #[must_use]
    pub fn unaccounted_closed(&self) -> u64 {
        self.lock().unaccounted_closed
    }

    /// Address-scoped denials deferred from the dial hook to the
    /// established hook, where the address exists.
    #[must_use]
    pub fn deferred_address_denials(&self) -> u64 {
        self.lock().deferred_address_denials
    }

    /// Behaviour dials refused because the reservation could not be
    /// separated from an address decision the hook could not make.
    #[must_use]
    pub fn placeholder_blocked(&self) -> u64 {
        self.lock().placeholder_blocked
    }

    /// Exhausted addresses that could not be scored for want of a
    /// ticket. Zero on every ordinary run; non-zero says the ceiling
    /// was too tight for the settlement to complete, which is F15.
    #[must_use]
    pub fn unsettled_addresses(&self) -> u64 {
        self.lock().unsettled_addresses
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
    /// What the provider says it is running, for attributing dials.
    classes: ActiveClasses,
    /// Connections that must be closed because they could not be
    /// accounted for. Drained by `poll`, which is the only place a
    /// `NetworkBehaviour` may ask the Swarm to do anything.
    close: Arc<Mutex<Vec<(PeerId, ConnectionId)>>>,
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
            classes: ActiveClasses::default(),
            close: Arc::new(Mutex::new(Vec::new())),
            started: Instant::now(),
        }
    }

    /// The provider's declaration of what it is running.
    #[must_use]
    pub fn classes(&self) -> ActiveClasses {
        self.classes.clone()
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
            let label = self.classes.label();
            let mut l = self.ledger.lock();
            l.behaviour_originated += 1;
            *l.by_class.entry(label).or_insert(0) += 1;
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
        // WHICH DENIALS THIS HOOK MAY ACT ON depends on whether it has a
        // real address. With none — the ordinary behaviour-dial shape —
        // the probe carries `""`, and an ADDRESS-SCOPED denial about a
        // placeholder is a denial about nothing: `AddressQuarantined`
        // cannot apply to an address that does not exist, and
        // `PolicyStateFull` reports that the address TABLE has no room
        // for a new entry, which under address-state pressure would
        // refuse every Kademlia dial including ones whose real address
        // is already known-good and needs no new entry.
        //
        // Both are evaluable at the established hook, where the address
        // is real. Peer-scoped and global denials — trust, backoff,
        // drain, the ceilings — are exactly the ones this hook CAN
        // decide, and it decides them.
        let placeholder = addresses.is_empty();
        for candidate in &candidates {
            let probe = DialRequest {
                peer: Some(identity.clone()),
                address: candidate.clone(),
                origin: DialOrigin::KademliaQuery,
            };
            // The probe's ticket is dropped immediately: this asks
            // whether the dial is admissible, and the reservation for
            // the dial itself is taken once, below.
            match self.admission.admit(&probe, now) {
                Ok(_) => {}
                Err(
                    DialDenial::AddressQuarantined | DialDenial::PolicyStateFull,
                ) if placeholder => {
                    // Deferred to the established hook, which will have
                    // the address this is pretending to be about.
                    self.ledger.lock().deferred_address_denials += 1;
                }
                Err(denial) => {
                    self.record_refusal(&denial_name(denial));
                    return Err(ConnectionDenied::new(std::io::Error::other(format!(
                        "kademlia dial refused: {denial:?} for {candidate}"
                    ))));
                }
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
        //
        // THE DEFERRAL ABOVE CANNOT EXTEND HERE, and that is a finding
        // rather than an oversight. `admit` decides policy AND takes the
        // reservation in one call, so there is no way to obtain a ticket
        // while declining to answer the address question. Deferring the
        // denial would mean admitting the dial with no ticket — the
        // ceilings then bound nothing, which is the failure mode F8 and
        // F11 exist to prevent — so the only available answer is to
        // refuse.
        //
        // The consequence is worth stating plainly: when the address
        // table is full of live quarantines, behaviour dials stop
        // entirely, including ones whose real address is already
        // known-good. That is fail-CLOSED, so it is the safe direction,
        // but it is a real availability cost and F16 does not remove it
        // — it only stops the PROBE from adding a second, unnecessary
        // refusal on top. Stage 10 needs an admission that can reserve
        // capacity without deciding an address it has not been given.
        let decision = self.admission.admit(&request, self.now_ms());
        if placeholder
            && matches!(
                decision,
                Err(DialDenial::AddressQuarantined | DialDenial::PolicyStateFull)
            )
        {
            self.ledger.lock().placeholder_blocked += 1;
        }
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
        let used = normalize(addr);
        // REMEMBERED FOR SETTLEMENT. The ticket this dial holds was
        // minted with an empty placeholder address, because at
        // admission there was none — see F9. Settling with it records
        // every Kademlia route against one empty address-policy entry,
        // so the address that was actually used has to be carried
        // forward from here, which is the first moment it exists.
        self.ledger
            .lock()
            .used_address
            .insert(connection_id, used.clone());
        let request = DialRequest {
            peer: Some(identity),
            address: used,
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
            // CAPACITY ONLY. `PolicyStateFull` is NOT capacity in this
            // sense: here the request carries the REAL address, so it
            // says the address table cannot take an entry for the route
            // this connection actually used — which is the fail-closed
            // address bound, and precisely the address-scoped decision
            // the dial hook deferred to here. Discarding it would let
            // the connection through in the one place that can judge it.
            Err(
                DialDenial::TooManyPendingDials
                | DialDenial::ConnectionLimitReached
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
            FromSwarm::DialFailure(e) => {
                // A DIAL THAT NEVER ESTABLISHED still names its
                // addresses — `DialError::Transport` carries one entry
                // per address attempted. Without this the failure is
                // settled against the empty placeholder, so an address
                // that refused the connection is never scored and never
                // enters the address book: exactly the case the
                // established hook cannot reach, because there is no
                // established connection.
                if let libp2p::swarm::DialError::Transport(attempts) = e.error
                    && !attempts.is_empty()
                {
                    // EVERY attempted address, not just the first. A
                    // multi-address dial exhausts them in turn and
                    // `DialError::Transport` carries one entry each;
                    // recording only the first leaves the rest unscored
                    // and immediately retryable, which is the same
                    // "looks checked, checks nothing" shape as F9.
                    let mut l = self.ledger.lock();
                    l.used_address
                        .insert(e.connection_id, normalize(&attempts[0].0));
                    l.also_failed.insert(
                        e.connection_id,
                        attempts[1..].iter().map(|(a, _)| normalize(a)).collect(),
                    );
                }
                Some((e.connection_id, false))
            }
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
                    let used = self.ledger.lock().used_address.remove(&id);
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
                        // THE ADDRESS THAT WAS USED, not the placeholder.
                        // `record_success` feeds `ticket.address()` to the
                        // address policy and the address book, so settling
                        // the placeholder marks an empty string as the
                        // peer's known-good route and leaves the real one
                        // unrecorded. The ticket binds its address at
                        // admission and a behaviour dial has none then, so
                        // the settlement re-mints against the real address
                        // — F12.
                        let peer = ticket.peer().cloned();
                        if let Some(ticket) = resettle(&m, ticket, used.as_deref(), now) {
                            let slot = m.record_success(ticket, now);
                            drop(m);
                            self.connections
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .insert(id, slot);
                            self.ledger.lock().retained += 1;
                        } else {
                            // NOTHING TO SETTLE, so nothing holds this
                            // connection's slot — and the connection is
                            // already established. Leaving it open means
                            // a live connection outside `max_connections`
                            // and with no address accounting, which is
                            // the ceiling failing open. Closing it is the
                            // only answer available: the alternative,
                            // keeping an unsettleable ticket, would
                            // reserve capacity nothing can release.
                            drop(m);
                            self.ledger.lock().unaccounted_closed += 1;
                            if let Some(p) = peer.and_then(|p| p.as_str().parse::<PeerId>().ok()) {
                                self.close
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .push((p, id));
                            }
                        }
                    } else {
                        // AND THE CONNECTION GOES. Releasing the ticket
                        // returns the reservation but leaves the
                        // connection live — established, unauthorized,
                        // and outside the manager's accounting, which is
                        // the same fail-open shape as an unsettleable
                        // settlement. The peer's authority was withdrawn
                        // between admission and the completed handshake;
                        // there is nothing to keep it for.
                        let peer = ticket.peer().cloned();
                        m.record_authorization_withdrawn(ticket, now);
                        drop(m);
                        self.ledger.lock().withdrawn += 1;
                        if let Some(p) = peer.and_then(|p| p.as_str().parse::<PeerId>().ok()) {
                            self.close
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .push((p, id));
                        }
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
                    let now = self.now_ms();
                    let (used, others) = {
                        let mut l = self.ledger.lock();
                        (
                            l.used_address.remove(&id),
                            l.also_failed.remove(&id).unwrap_or_default(),
                        )
                    };
                    let peer = ticket.peer().cloned();
                    // EVERY TICKET MINTED BEFORE ANY IS SETTLED.
                    //
                    // The ordering is load-bearing and was wrong. The
                    // first `record_failure` advances PEER backoff when
                    // no known-good alternative remains — which is the
                    // ordinary case, since every candidate of this dial
                    // just failed. Every subsequent `admit` then returns
                    // `PeerBackoff`, so the remaining addresses were
                    // never settled and stayed unscored: the exact
                    // outcome the multi-address fix was written to
                    // prevent. K18.7 could not see it because its
                    // topology keeps a good route alive, which suppresses
                    // peer backoff.
                    //
                    // Admitting first takes one reservation per address
                    // of a single dial — bounded by that dial's candidate
                    // list — and holds them only for as long as the
                    // settlement loop below.
                    let m = self.manager.lock().unwrap_or_else(|e| e.into_inner());
                    let mut tickets: Vec<DialTicket> = Vec::new();
                    let mut unsettled = 0_u64;
                    if let Some(fresh) = resettle(&m, ticket, used.as_deref(), now) {
                        tickets.push(fresh);
                    } else {
                        unsettled += 1;
                    }
                    if let Some(peer) = peer {
                        for other in others {
                            let request = DialRequest {
                                peer: Some(peer.clone()),
                                address: other,
                                origin: DialOrigin::KademliaQuery,
                            };
                            match m.handle().admit(&request, now) {
                                Ok(t) => tickets.push(t),
                                // COUNTED, NOT SWALLOWED. Pre-minting
                                // solves the peer-backoff coupling and
                                // buys a new dependency: settlement now
                                // needs one spare pending-dial and
                                // connection slot per address. Under a
                                // tight ceiling the first ticket takes
                                // the last slot and the rest are
                                // refused, leaving those routes
                                // unscored — the same silent omission
                                // in a different disguise.
                                //
                                // There is no way out inside the current
                                // API: recording an address failure
                                // requires a ticket, and a ticket
                                // requires passing the policy that this
                                // very failure has just changed.
                                // Sequential settlement hits the
                                // backoff, batched settlement hits the
                                // ceiling. So the shortfall is COUNTED
                                // and F15 asks Stage 10 for an
                                // address-scoped failure API that needs
                                // no admission.
                                Err(_) => unsettled += 1,
                            }
                        }
                    }
                    drop(m);
                    if unsettled > 0 {
                        self.ledger.lock().unsettled_addresses += unsettled;
                    }
                    let mut m = self.manager.lock().unwrap_or_else(|e| e.into_inner());
                    for t in tickets {
                        m.record_failure(t, now);
                    }
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
        // The only place a behaviour may act on the Swarm. A connection
        // that could not be accounted for is closed here rather than
        // left open outside the ceiling.
        if let Some((peer_id, connection)) = self
            .close
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop()
        {
            return Poll::Ready(ToSwarm::CloseConnection {
                peer_id,
                connection: libp2p::swarm::CloseConnection::One(connection),
            });
        }
        Poll::Pending
    }
}

/// Re-mint a settlement ticket against the address the dial USED.
///
/// A `DialTicket` binds its address when it is admitted, and a
/// behaviour dial has no address then (F9) — so the held ticket carries
/// an empty placeholder, and settling it records every Kademlia route
/// against one empty address-policy entry: the real address never
/// becomes known-good, never enters the address book, and a failure on
/// it is never scored.
///
/// Re-admitting against the real address is the workaround available
/// through the production API as it stands. It briefly takes a second
/// reservation, which is why the ORIGINAL is dropped first. If the
/// re-mint is refused — a ceiling, or a policy that changed under it —
/// the placeholder is settled instead: an imprecise settlement is worse
/// than none, but losing the accounting entirely is worse still.
///
/// F12 records the underlying gap: Stage 10 needs either a re-bindable
/// ticket or a settlement API that takes the address, because this is a
/// workaround and not a design.
fn resettle(
    manager: &ConnectionManager,
    ticket: DialTicket,
    used: Option<&str>,
    now_ms: u64,
) -> Option<DialTicket> {
    let Some(used) = used else {
        return Some(ticket);
    };
    if ticket.address() == used {
        return Some(ticket);
    }
    let Some(peer) = ticket.peer().cloned() else {
        return Some(ticket);
    };
    let request = DialRequest {
        peer: Some(peer),
        address: used.to_owned(),
        origin: DialOrigin::KademliaQuery,
    };
    // The original goes back FIRST, so the re-mint is not competing
    // with the reservation it is replacing.
    drop(ticket);
    // `None` when the re-mint is refused — a ceiling, or a policy that
    // changed under it. The reservation is already released by the drop
    // above, so nothing leaks; what is lost is this dial's address
    // accounting, which is better than recording it against an address
    // the dial did not use.
    manager.handle().admit(&request, now_ms).ok()
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
