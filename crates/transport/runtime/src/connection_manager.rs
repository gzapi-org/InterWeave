// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The root connection funnel: who may dial, when, and how it is counted.
//!
//! # The gate cannot call the manager
//!
//! ADR-0011 is explicit that `ConnectionManager` publishes an
//! **atomically readable policy snapshot** to the Swarm task, and that
//! the gate **must not block on async policy calls while the Swarm is
//! being polled**. That single sentence decides the shape of this
//! module. The obvious design — a gate that asks the manager on each
//! dial — is the one the architecture rules out, because the Swarm poll
//! loop cannot await a policy answer without stalling every connection
//! it is already driving.
//!
//! So the manager owns mutable state and publishes immutable
//! [`PolicySnapshot`]s. The gate holds a [`SnapshotHandle`], loads the
//! current snapshot, and decides locally.
//!
//! # Policy is eventually consistent; resources are exact
//!
//! Those are different guarantees and conflating them would be a bug in
//! either direction.
//!
//! A snapshot is a photograph. Between publication and use, a peer's
//! backoff may have advanced or its trust may have been revoked, so an
//! admission can be made against slightly stale policy — bounded by how
//! promptly the manager republishes, which is what ADR-0011's "promptly"
//! asks for and why [`PolicySnapshot::revision`] exists to make staleness
//! observable rather than invisible.
//!
//! The *resource* bounds cannot work that way. If two dials are admitted
//! concurrently against a snapshot that says "31 of 32 pending", the
//! limit has been exceeded and no later reconciliation un-spends the
//! memory. The pending count is therefore a shared atomic that both the
//! gate and the manager increment, and the ceiling is checked against
//! the live value rather than the photographed one.
//!
//! # A dial that cannot be accounted for is not admitted
//!
//! [`PolicySnapshot::admit`] returns a [`DialTicket`], and the ticket
//! holds the pending-dial slot it reserved. Dropping it without settling
//! releases the slot. That is what makes the count self-correcting: a
//! caller who admits a dial and then loses it cannot leak the slot,
//! because there is no path that admits without producing a ticket and
//! no way to hold a ticket without eventually dropping it.
//!
//! The backend is expected to require a `DialTicket` to reach
//! `Swarm::dial`, which is the structural half of "root admission is the
//! only policy authority for outbound Swarm dials" — a caller cannot
//! forget to ask, because it cannot call without the answer.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use interweave_transport_api::TransportIdentity;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};

use crate::connection_policy::{
    ConnectionClass, ConnectionPolicy, DialDenial, DialOrigin, DialRequest,
};

/// Who this profile trusts, and for what.
///
/// The two sets are SEPARATE authorities and are kept separate here for
/// the reason ADR-0036 states: infrastructure authorization is not a
/// weaker data-plane trust, it is a different permission. Folding them
/// into one set -- or into one ordered scale -- is how a relay this
/// profile uses for reachability becomes a peer it will exchange
/// application messages with.
#[derive(Debug, Clone, Default)]
pub struct TrustSources {
    /// Peers authorized for the application data plane.
    pub peers: PeerTrustPolicy,
    /// Peers authorized for reachability control only.
    pub infrastructure: InfrastructureSet,
}

impl TrustSources {
    /// The class this profile grants `peer`, right now.
    ///
    /// Data-plane trust is checked first and wins, because it is the
    /// broader authority: a peer in both sets may do everything the
    /// infrastructure set would have permitted. A peer in neither is
    /// [`ConnectionClass::Unauthorized`], which is the DEFAULT answer --
    /// an empty configuration admits nobody (ADR-0012), and there is no
    /// constructor here that says otherwise.
    #[must_use]
    pub fn classify(&self, peer: &TransportIdentity) -> ConnectionClass {
        if self.peers.decide(peer).is_allowed() {
            ConnectionClass::DataPlaneTrusted
        } else if self.infrastructure.permits_control_connection(peer) {
            ConnectionClass::ConnectivityInfrastructureOnly
        } else {
            ConnectionClass::Unauthorized
        }
    }
}

/// An immutable view of connection policy, safe to read from the Swarm.
///
/// Cheap to clone (one `Arc` bump) and answers admission without a lock
/// on anything the manager mutates.
#[derive(Debug)]
pub struct PolicySnapshot {
    policy: ConnectionPolicy,
    trust: Arc<TrustSources>,
    revision: u64,
    /// Live pending-dial count, SHARED with the manager.
    ///
    /// Not a field of the photographed policy: see the module note on
    /// why resources are exact and policy is not.
    pending: Arc<AtomicUsize>,
    max_pending_dials: usize,
    /// Live established-connection count, SHARED with the manager.
    ///
    /// A connection is a resource, so it obeys the resource rule and
    /// not the policy one: reserved when the dial is admitted, held by
    /// the ticket, and carried over to the connection when it
    /// establishes. Counting only at establishment let every dial
    /// admitted before the first one connected observe a count of zero,
    /// so a ceiling of one admitted as many concurrent dials as the
    /// pending budget allowed.
    connections: Arc<AtomicUsize>,
    max_connections: usize,
    /// Live drain state, SHARED with the manager.
    ///
    /// Photographed, it was the one piece of policy a holder could keep
    /// admitting against after it had been revoked. Draining is not the
    /// kind of policy that may be eventually consistent: the whole point
    /// of it is that no new dial starts.
    shutting_down: Arc<AtomicBool>,
    /// The revision the manager has currently published, SHARED.
    ///
    /// What bounds staleness to zero rather than to "however long a
    /// holder keeps its `Arc`". See [`Self::admit`].
    published_revision: Arc<AtomicU64>,
}

impl PolicySnapshot {
    /// Which publication this is.
    ///
    /// Monotonic. A holder that compares it against
    /// [`ConnectionManager::revision`] can tell it is deciding on stale
    /// policy; nothing here forces it to care, because a slightly stale
    /// *policy* decision is permitted and a stale *resource* decision is
    /// not possible.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// The class this profile grants `peer`, as photographed.
    ///
    /// Photographed rather than live, and that is the correct side of
    /// the resource/policy split: a classification is policy, so it may
    /// be one publication stale, and a snapshot that is stale refuses
    /// outright rather than deciding (see [`Self::admit`]). What it must
    /// never be is ASSUMED, which is what a hardcoded
    /// `DataPlaneTrusted` at the call site amounted to.
    #[must_use]
    pub fn classify(&self, peer: &TransportIdentity) -> ConnectionClass {
        self.trust.classify(peer)
    }

    /// Decide one outbound dial and reserve its slot.
    ///
    /// # Errors
    /// Returns the [`DialDenial`] that applied. A denial reserves
    /// nothing, and in particular a denied behaviour-originated dial
    /// leaves retry state untouched — ADR-0011 requires that a refused
    /// autonomous dial cannot become a way to clear another origin's
    /// backoff.
    pub fn admit(&self, request: &DialRequest, now_ms: u64) -> Result<DialTicket, DialDenial> {
        // CLASSIFIED HERE, not asserted by the caller. The class used to
        // be a parameter, which made every call site an authority on
        // what a peer is authorized for -- and the substrate's only
        // call site passed a hardcoded `DataPlaneTrusted`, so the trust
        // policy was consulted by nobody and an empty allowlist admitted
        // everyone. A caller cannot pass a class it does not have,
        // because there is nowhere to pass one.
        //
        // A dial that names no peer is `Unauthorized` for the same
        // reason: there is no identity to authorize, and admitting what
        // cannot be classified is how the classification stops meaning
        // anything.
        let class = match &request.peer {
            Some(peer) => self.trust.classify(peer),
            None => ConnectionClass::Unauthorized,
        };

        // LIVE, not photographed. A holder that took a snapshot before
        // `begin_shutdown` would otherwise go on admitting dials for as
        // long as it kept the `Arc`, and draining would mean nothing.
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(DialDenial::ShuttingDown);
        }

        // SUPERSEDED SNAPSHOTS DO NOT DECIDE. Everything below reads the
        // photographed policy, so a retained `Arc` would answer with the
        // authorization, backoff and quarantine state of whenever it was
        // taken -- forever, and with no way for the manager to reach it.
        // Refusing here makes the tolerance for stale policy zero rather
        // than unbounded, and the refusal is recoverable: reload the
        // handle and ask again, which is what `SnapshotHandle::admit`
        // does.
        if self.revision != self.published_revision.load(Ordering::Acquire) {
            return Err(DialDenial::PolicySuperseded);
        }

        during_admit();

        // THE POLICY HALF, from the photograph. Backoff, class, origin,
        // address quarantine, accounting capacity.
        self.policy.admit(request, class, now_ms)?;

        // THE RESOURCE HALF, against the live count. A compare-exchange
        // loop rather than a fetch_add-then-check: adding first and
        // backing out on overflow means two concurrent admissions can
        // both observe the ceiling exceeded and both retreat, or worse,
        // a third sees a count above the limit that briefly existed.
        // Reserving only from a value that is under the limit means the
        // count is never above it, at any instant, for any observer.
        reserve(&self.pending, self.max_pending_dials)
            .map_err(|()| DialDenial::TooManyPendingDials)?;

        // THE CONNECTION IT WILL BECOME, reserved now. Held by the
        // ticket and handed to the connection on success, so the
        // ceiling counts what is on its way as well as what is open.
        let ticket = DialTicket {
            pending: Arc::clone(&self.pending),
            connections: Arc::clone(&self.connections),
            peer: request.peer.clone(),
            address: request.address.clone(),
            origin: request.origin,
            settled: false,
            connection_kept: false,
        };
        if reserve(&self.connections, self.max_connections).is_err() {
            // `ticket` releases the pending slot as it drops here, and
            // holds no connection slot to release.
            let mut ticket = ticket;
            ticket.connection_kept = true;
            return Err(DialDenial::ConnectionLimitReached);
        }

        // REVALIDATED AFTER RESERVING, and this is not belt-and-braces.
        // The freshness check above happens before the policy read and
        // the two reservations; a publication landing in that window --
        // a quarantine, a revocation, a drain -- would have been decided
        // against by a snapshot that had already passed its only test.
        // Checking again once the slots are held means any publication
        // concurrent with the decision refuses it, and the rollback is
        // what makes the refusal free.
        if self.shutting_down.load(Ordering::Acquire)
            || self.revision != self.published_revision.load(Ordering::Acquire)
        {
            drop(ticket);
            return Err(DialDenial::PolicySuperseded);
        }

        Ok(ticket)
    }

    /// Established connections right now.
    #[must_use]
    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::Acquire)
    }

    /// Pending dials right now, across every holder of this snapshot.
    #[must_use]
    pub fn pending_dials(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }
}

// A seam at the point a publication would be missed.
//
// The window this closes is real but a few nanoseconds wide, so a test
// that tried to win the race by repetition would be a test that passes
// on a broken implementation whenever the machine is busy. The hook
// makes the interleaving exact: it fires once, between the freshness
// check and everything that depends on it, which is precisely where a
// concurrent publication does its damage.
//
// Compiled out of every non-test build.
#[cfg(test)]
thread_local! {
    static DURING_ADMIT: std::cell::RefCell<Option<Box<dyn Fn()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn during_admit() {
    // TAKEN, not borrowed across the call: the hook publishes, and
    // publishing must not re-enter a borrow this frame is holding.
    let hook = DURING_ADMIT.with(|h| h.borrow_mut().take());
    if let Some(f) = hook {
        f();
    }
}

#[cfg(not(test))]
const fn during_admit() {}

/// Take one unit of a bounded resource, or report that it is full.
///
/// A compare-exchange loop rather than a fetch_add-then-check: adding
/// first and backing out on overflow means two concurrent reservations
/// can both observe the ceiling exceeded and both retreat, or worse, a
/// third sees a count above the limit that briefly existed. Taking only
/// from a value that is under the limit means the count is never above
/// it, at any instant, for any observer.
fn reserve(counter: &AtomicUsize, ceiling: usize) -> Result<(), ()> {
    let mut current = counter.load(Ordering::Acquire);
    loop {
        if current >= ceiling {
            return Err(());
        }
        match counter.compare_exchange_weak(
            current,
            current + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return Ok(()),
            Err(seen) => current = seen,
        }
    }
}

/// One established connection's place under `max_connections`.
///
/// Released on drop, so a connection that goes away cannot leave its
/// slot behind however it went away -- an error path, a panic, or a
/// runtime dropped mid-flight.
#[derive(Debug)]
#[must_use = "dropping the slot releases it; hold it for the life of the connection"]
pub struct ConnectionSlot {
    connections: Arc<AtomicUsize>,
    released: bool,
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        if !self.released {
            self.connections.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// Permission to perform one outbound dial, holding its accounting slot.
///
/// `#[must_use]` because dropping it unexamined is how a caller admits a
/// dial and never makes it, and the slot would then be released with the
/// manager never learning the outcome. Dropping is SAFE — the slot comes
/// back — but it is silent, so the type says out loud that something is
/// expected to happen with it.
#[derive(Debug)]
#[must_use = "a ticket is permission to dial; drop it only if the dial is abandoned"]
pub struct DialTicket {
    pending: Arc<AtomicUsize>,
    connections: Arc<AtomicUsize>,
    peer: Option<TransportIdentity>,
    address: String,
    origin: DialOrigin,
    settled: bool,
    connection_kept: bool,
}

impl DialTicket {
    /// The peer this permission was granted for, if one was named.
    #[must_use]
    pub const fn peer(&self) -> Option<&TransportIdentity> {
        self.peer.as_ref()
    }

    /// The address this permission was granted for.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Why the dial was requested.
    #[must_use]
    pub const fn origin(&self) -> DialOrigin {
        self.origin
    }
}

impl Drop for DialTicket {
    fn drop(&mut self) {
        if !self.connection_kept {
            // The dial never became a connection, so the slot it was
            // holding for one goes back.
            self.connections.fetch_sub(1, Ordering::AcqRel);
        }
        if !self.settled {
            // Saturating in spirit: the count is only ever incremented
            // by a successful reservation, so it cannot underflow unless
            // a ticket is released twice — which `settled` prevents.
            self.pending.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// A handle the Swarm task holds to read current policy.
///
/// Cloneable and cheap. [`Self::load`] takes a read lock only long
/// enough to clone an `Arc` — never across a decision, never across an
/// await, and never while the manager is consulted. That is the
/// non-blocking property ADR-0011 asks for, expressed with `std` rather
/// than by adding a dependency for it.
#[derive(Debug, Clone)]
pub struct SnapshotHandle {
    current: Arc<RwLock<Arc<PolicySnapshot>>>,
}

impl SnapshotHandle {
    /// The current snapshot.
    ///
    /// Poisoning is RECOVERED rather than propagated, and that is safe
    /// here for a specific reason: the protected value is a single
    /// `Arc`, and publication replaces it in one move. There is no
    /// half-written snapshot to observe, so a panic elsewhere in the
    /// process must not also take out every dial decision. A lock whose
    /// contents cannot be torn has nothing to protect a reader from.
    #[must_use]
    pub fn load(&self) -> Arc<PolicySnapshot> {
        Arc::clone(&self.current.read().unwrap_or_else(|e| e.into_inner()))
    }

    /// Decide one dial against the CURRENT snapshot.
    ///
    /// The way callers should ask. `load().admit(..)` is still correct
    /// and still safe -- a superseded snapshot refuses rather than
    /// deciding -- but a publication landing between the load and the
    /// decision would surface as a `PolicySuperseded` refusal of a dial
    /// that nothing was actually wrong with. Reloading and asking again
    /// is the whole remedy, and it belongs here rather than in every
    /// call site.
    ///
    /// Bounded to [`ADMIT_RELOAD_ATTEMPTS`] tries, not a loop: the
    /// manager publishes on every recorded outcome, so an unbounded
    /// retry is a spin whose length a busy network chooses. Exhausting
    /// them refuses, which is the fail-closed direction.
    ///
    /// # Errors
    /// The [`DialDenial`] that applied, or [`DialDenial::PolicySuperseded`]
    /// if publication outran every attempt.
    pub fn admit(&self, request: &DialRequest, now_ms: u64) -> Result<DialTicket, DialDenial> {
        let mut last = DialDenial::PolicySuperseded;
        for _ in 0..ADMIT_RELOAD_ATTEMPTS {
            match self.load().admit(request, now_ms) {
                Err(DialDenial::PolicySuperseded) => last = DialDenial::PolicySuperseded,
                other => return other,
            }
        }
        Err(last)
    }
}

/// How many times [`SnapshotHandle::admit`] reloads before refusing.
///
/// Small on purpose. Each attempt costs a read lock and an atomic load,
/// and losing three races in a row means publication is saturating the
/// manager -- a condition a caller should be told about rather than
/// spin through.
pub const ADMIT_RELOAD_ATTEMPTS: usize = 3;

/// Default ceiling on peers awaiting a retry.
///
/// The retry table is a map keyed by peer, so it is state a remote
/// party influences by failing to connect. Bounded for the reason the
/// address book and the pre-auth source table are.
pub const DEFAULT_MAX_RETRY_ENTRIES: usize = 1_024;

/// One peer's scheduled reconnection attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Retry {
    due_at_ms: u64,
    attempts: u32,
}

/// A peer whose authorization was reduced by a trust change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revoked {
    /// The peer.
    pub peer: TransportIdentity,
    /// What it was authorized for.
    pub was: ConnectionClass,
    /// What it is authorized for now.
    pub now: ConnectionClass,
}

/// Whether `now` still permits everything `was` did.
///
/// Not an ordering on the enum, deliberately: ADR-0036 says
/// infrastructure authorization is a different permission rather than a
/// lesser one, so this answers one narrow question -- did anything the
/// peer was allowed to do stop being allowed -- and nothing else. A
/// `PartialOrd` derive would have made `Infrastructure < DataPlane`
/// available to every call site as a general fact, which it is not.
const fn permits(now: ConnectionClass, was: ConnectionClass) -> bool {
    matches!(
        (was, now),
        (ConnectionClass::Unauthorized, _)
            | (_, ConnectionClass::DataPlaneTrusted)
            | (
                ConnectionClass::ConnectivityInfrastructureOnly,
                ConnectionClass::ConnectivityInfrastructureOnly,
            )
    )
}

/// The root connection funnel.
///
/// Owns connection policy and publishes it; schedules reconnection; and
/// decides whether an inbound connection is kept. Pure state: no
/// sockets, no clock, no async. Time arrives as a parameter so every
/// bound is testable by enumeration rather than by waiting.
#[derive(Debug)]
pub struct ConnectionManager {
    policy: ConnectionPolicy,
    trust: Arc<TrustSources>,
    revision: u64,
    pending: Arc<AtomicUsize>,
    max_pending_dials: usize,
    connections: Arc<AtomicUsize>,
    max_connections: usize,
    shutting_down: Arc<AtomicBool>,
    published_revision: Arc<AtomicU64>,
    retries: std::collections::BTreeMap<TransportIdentity, Retry>,
    max_retry_entries: usize,
    published: Arc<RwLock<Arc<PolicySnapshot>>>,
}

impl ConnectionManager {
    /// Build a manager around a connection policy.
    #[must_use]
    pub fn new(policy: ConnectionPolicy, max_pending_dials: usize) -> Self {
        let pending = Arc::new(AtomicUsize::new(0));
        let connections = Arc::new(AtomicUsize::new(0));
        let max_connections = policy.max_connections;
        let shutting_down = Arc::new(AtomicBool::new(false));
        let published_revision = Arc::new(AtomicU64::new(0));
        let trust = Arc::new(TrustSources::default());
        let first = Arc::new(PolicySnapshot {
            policy: policy.clone(),
            trust: Arc::clone(&trust),
            revision: 0,
            pending: Arc::clone(&pending),
            max_pending_dials,
            connections: Arc::clone(&connections),
            max_connections,
            shutting_down: Arc::clone(&shutting_down),
            published_revision: Arc::clone(&published_revision),
        });
        Self {
            policy,
            trust,
            revision: 0,
            pending,
            max_pending_dials,
            connections,
            max_connections,
            shutting_down,
            published_revision,
            retries: std::collections::BTreeMap::new(),
            max_retry_entries: DEFAULT_MAX_RETRY_ENTRIES,
            published: Arc::new(RwLock::new(first)),
        }
    }

    /// A handle for the Swarm task.
    #[must_use]
    pub fn handle(&self) -> SnapshotHandle {
        SnapshotHandle {
            current: Arc::clone(&self.published),
        }
    }

    /// The revision currently published.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Republish, so holders see current policy.
    ///
    /// PROMPTLY is the word ADR-0011 uses, and it is the caller's job:
    /// every mutation below publishes before returning, so a holder is
    /// never more than one in-flight decision behind. A mutation that
    /// forgot to publish would leave the gate admitting against
    /// authorization that had been revoked, which is the one staleness
    /// that is not merely a timing detail.
    fn publish(&mut self) {
        self.revision = self.revision.saturating_add(1);
        let next = Arc::new(PolicySnapshot {
            policy: self.policy.clone(),
            trust: Arc::clone(&self.trust),
            revision: self.revision,
            pending: Arc::clone(&self.pending),
            max_pending_dials: self.max_pending_dials,
            connections: Arc::clone(&self.connections),
            max_connections: self.max_connections,
            shutting_down: Arc::clone(&self.shutting_down),
            published_revision: Arc::clone(&self.published_revision),
        });
        *self.published.write().unwrap_or_else(|e| e.into_inner()) = next;
        // AFTER the install, so the window between them is one in which
        // both the old and the new snapshot refuse rather than one in
        // which the old one is still trusted.
        self.published_revision
            .store(self.revision, Ordering::Release);
    }

    /// Replace the trust sources and publish them.
    ///
    /// Publishing is the whole mechanism: a revocation that did not
    /// publish would leave the gate admitting against authorization
    /// that had been withdrawn, and ADR-0012 requires a removal to take
    /// effect on connectivity rather than merely on the next
    /// configuration read.
    ///
    /// Returns the peers whose class DROPPED, so the caller can evict
    /// what they are no longer authorized to hold. Reported rather than
    /// acted on here because this crate owns no connections: a manager
    /// that pretended to close them would be a manager whose promise
    /// nothing kept.
    pub fn set_trust(&mut self, trust: TrustSources, live: &[TransportIdentity]) -> Vec<Revoked> {
        let previous = Arc::clone(&self.trust);
        self.trust = Arc::new(trust);
        self.publish();
        live.iter()
            .filter_map(|peer| {
                let was = previous.classify(peer);
                let now = self.trust.classify(peer);
                (was != now && !permits(now, was)).then(|| Revoked {
                    peer: peer.clone(),
                    was,
                    now,
                })
            })
            .collect()
    }

    /// The class this profile currently grants `peer`.
    #[must_use]
    pub fn classify(&self, peer: &TransportIdentity) -> ConnectionClass {
        self.trust.classify(peer)
    }

    /// Record an authenticated success and clear this peer's retry.
    ///
    /// Returns the connection slot the admission reserved, now owned by
    /// the connection itself. Holding it is what keeps the ceiling
    /// honest for the connection's whole life; dropping it says the
    /// connection is gone.
    pub fn record_success(&mut self, ticket: DialTicket, now_ms: u64) -> ConnectionSlot {
        if let Some(peer) = ticket.peer().cloned() {
            self.policy.record_success(&peer, ticket.address(), now_ms);
            self.retries.remove(&peer);
        }
        let slot = self.keep_connection(ticket);
        self.publish();
        slot
    }

    /// Record a failed dial and schedule the next attempt.
    ///
    /// A denied dial never reaches here: only an ADMITTED dial produces
    /// a ticket, so there is no path by which a refusal advances retry
    /// state. ADR-0011 requires exactly that, and expressing it through
    /// the ticket makes it structural rather than a rule to remember.
    pub fn record_failure(&mut self, ticket: DialTicket, now_ms: u64) {
        if let Some(peer) = ticket.peer().cloned() {
            // ONE delay, used for both. The address-scoped backoff and
            // the reconnect schedule disagreeing would mean the manager
            // retries a peer at a moment its own policy still refuses,
            // producing a denial the retry counter then treats as
            // another failure -- a peer talking itself into permanent
            // backoff without the remote end doing anything.
            let delay = self.retry_delay_ms(&peer);
            self.policy
                .record_address_failure(&peer, ticket.address(), now_ms, delay);
            self.schedule_retry(peer, now_ms, delay);
        }
        self.settle(ticket);
        self.publish();
    }

    /// Established connections right now, dialed and accepted.
    #[must_use]
    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::Acquire)
    }

    /// Record that the peer at this address authenticated a different
    /// identity.
    pub fn record_identity_mismatch(&mut self, ticket: DialTicket, now_ms: u64) -> bool {
        let mismatched = ticket.peer().cloned().is_some_and(|peer| {
            self.policy
                .record_identity_mismatch(&peer, ticket.address(), now_ms)
        });
        self.settle(ticket);
        self.publish();
        mismatched
    }

    fn settle(&self, mut ticket: DialTicket) {
        ticket.settled = true;
        self.pending.fetch_sub(1, Ordering::AcqRel);
        // `connection_kept` stays false, so the ticket's connection
        // reservation is released as it drops. A dial that failed holds
        // no connection.
    }

    /// Settle the dial and TRANSFER its connection reservation.
    fn keep_connection(&self, mut ticket: DialTicket) -> ConnectionSlot {
        ticket.connection_kept = true;
        let slot = ConnectionSlot {
            connections: Arc::clone(&self.connections),
            released: false,
        };
        self.settle(ticket);
        slot
    }

    /// Reserve a slot for an INBOUND connection, or refuse to keep it.
    ///
    /// Inbound arrives without an admission, so there is no ticket to
    /// carry the reservation. `None` means the ceiling is full or the
    /// runtime is draining, and the connection must be closed rather
    /// than kept: a bound that counts only what this node dialed is not
    /// a bound on what it holds open.
    pub fn admit_inbound(&mut self) -> Option<ConnectionSlot> {
        if self.shutting_down.load(Ordering::Acquire) {
            return None;
        }
        reserve(&self.connections, self.max_connections).ok()?;
        let slot = ConnectionSlot {
            connections: Arc::clone(&self.connections),
            released: false,
        };
        self.publish();
        Some(slot)
    }

    /// Record that an established connection has gone.
    ///
    /// Takes the slot rather than a count, so releasing it is the same
    /// act as saying it closed and neither can happen without the other.
    pub fn record_connection_closed(&mut self, slot: ConnectionSlot) {
        drop(slot);
        self.publish();
    }

    /// Whether an inbound connection from this peer is kept.
    ///
    /// ADR-0011: the same CURRENT authorization policy that governs
    /// outbound applies before an inbound data-plane connection is
    /// retained. Inbound is not a way in for a peer that outbound would
    /// refuse, and "it connected to us" is not an authorization.
    #[must_use]
    pub fn retain_inbound(&self, class: ConnectionClass) -> bool {
        !self.shutting_down.load(Ordering::Acquire)
            && matches!(class, ConnectionClass::DataPlaneTrusted)
    }

    /// Begin draining. Admission refuses from the next snapshot on.
    pub fn begin_shutdown(&mut self) {
        self.shutting_down.store(true, Ordering::Release);
        self.policy.shutting_down = true;
        self.publish();
    }

    /// Peers whose retry is due, soonest first.
    #[must_use]
    pub fn due_retries(&self, now_ms: u64) -> Vec<TransportIdentity> {
        let mut due: Vec<(&TransportIdentity, &Retry)> = self
            .retries
            .iter()
            .filter(|(_, r)| now_ms >= r.due_at_ms)
            .collect();
        due.sort_by_key(|(_, r)| r.due_at_ms);
        due.into_iter().map(|(p, _)| p.clone()).collect()
    }

    /// Peers awaiting a retry.
    #[must_use]
    pub fn scheduled_retries(&self) -> usize {
        self.retries.len()
    }

    /// The delay before this peer is retried, given what it has already
    /// cost.
    ///
    /// The cadence `CONNECTIVITY.md` states for a peer that is not yet
    /// verified: 30 seconds, exponential, bounded by five minutes. The
    /// numbers are restated here rather than referenced, and the test
    /// names the document, so a drift fails rather than becoming a
    /// discrepancy nobody compares.
    fn retry_delay_ms(&self, peer: &TransportIdentity) -> u64 {
        const BASE_MS: u64 = 30_000;
        const CEILING_MS: u64 = 5 * 60 * 1_000;
        let attempts = self.retries.get(peer).map_or(0, |r| r.attempts);
        // Shifted by a CLAMPED exponent. `1u64 << 64` is undefined-ish
        // in the sense that it panics in debug and wraps in release, and
        // an attempt counter is driven by how often a remote end refuses
        // to connect -- so the clamp is a bound on remote-influenced
        // arithmetic, not a tidiness.
        BASE_MS
            .saturating_mul(1u64 << attempts.min(8))
            .min(CEILING_MS)
    }

    fn schedule_retry(&mut self, peer: TransportIdentity, now_ms: u64, delay: u64) {
        let attempts = self.retries.get(&peer).map_or(0, |r| r.attempts);

        if !self.retries.contains_key(&peer) && self.retries.len() >= self.max_retry_entries {
            // Full. Forget the entry that will wait longest, because it
            // is the one whose loss costs the least — and refusing to
            // record the newest failure instead would mean the peer that
            // just failed is retried immediately and forever.
            if let Some(furthest) = self
                .retries
                .iter()
                .max_by_key(|(_, r)| r.due_at_ms)
                .map(|(p, _)| p.clone())
            {
                self.retries.remove(&furthest);
            }
        }

        self.retries.insert(
            peer,
            Retry {
                due_at_ms: now_ms.saturating_add(delay),
                attempts: attempts.saturating_add(1),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    const P2: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";

    fn peer(s: &str) -> TransportIdentity {
        TransportIdentity::parse(s).expect("valid identity")
    }

    /// A manager that trusts the two peers these tests dial.
    ///
    /// Stated rather than assumed: the class is no longer something a
    /// call site can pass, so a test that dials has to say who it
    /// trusts, exactly as the substrate does.
    fn manager(max_pending: usize) -> ConnectionManager {
        let mut m = ConnectionManager::new(ConnectionPolicy::new(64, 64), max_pending);
        let _ = m.set_trust(trusting(&[P1, P2], &[]), &[]);
        m
    }

    /// A manager that trusts nobody, which is the default configuration.
    fn untrusting(max_pending: usize) -> ConnectionManager {
        ConnectionManager::new(ConnectionPolicy::new(64, 64), max_pending)
    }

    /// A manager whose connection ceiling is the thing under test.
    fn manager_holding(max_connections: usize) -> ConnectionManager {
        let mut m = ConnectionManager::new(ConnectionPolicy::new(64, max_connections), 64);
        let _ = m.set_trust(trusting(&[P1, P2], &[]), &[]);
        m
    }

    fn request(peer_id: &str, address: &str) -> DialRequest {
        DialRequest {
            peer: Some(peer(peer_id)),
            address: address.to_owned(),
            origin: DialOrigin::ConnectionManager,
        }
    }

    #[test]
    fn the_pending_ceiling_holds_against_concurrent_admissions() {
        // THE ONE THING A SNAPSHOT CANNOT DO. Policy may be a moment
        // stale and nothing breaks; a resource bound read from a
        // photograph does break, because two holders of the same
        // snapshot both see "under the limit" and both admit. The count
        // is therefore shared and reserved with a compare-exchange, so
        // it is never above the ceiling for any observer at any instant.
        let m = manager(2);
        let snap_a = m.handle().load();
        let snap_b = m.handle().load();
        assert_eq!(snap_a.revision(), snap_b.revision(), "the same photograph");

        let t1 = snap_a
            .admit(&request(P1, "/ip4/10.0.0.1/tcp/1"), 0)
            .expect("first");
        // The SECOND holder sees the first holder's reservation, which
        // is the property under test: it did not photograph the count.
        let t2 = snap_b
            .admit(&request(P2, "/ip4/10.0.0.2/tcp/1"), 0)
            .expect("second");
        assert_eq!(snap_a.pending_dials(), 2);
        assert_eq!(
            snap_b.admit(&request(P1, "/ip4/10.0.0.3/tcp/1"), 0).err(),
            Some(DialDenial::TooManyPendingDials),
            "an older snapshot must not grant a slot that no longer exists"
        );

        // Releasing frees the slot for either holder.
        drop(t1);
        assert_eq!(snap_a.pending_dials(), 1);
        let t3 = snap_b
            .admit(&request(P1, "/ip4/10.0.0.3/tcp/1"), 0)
            .expect("the released slot is reusable");
        drop(t3);
        drop(t2);
    }

    #[test]
    fn an_abandoned_ticket_returns_its_slot() {
        // A caller who admits a dial and then loses it must not leak the
        // reservation. There is no path that admits without producing a
        // ticket, so `Drop` is the backstop that makes the count
        // self-correcting rather than a number that only grows.
        let m = manager(1);
        let snap = m.handle().load();
        {
            let _t = snap.admit(&request(P1, "/a"), 0).expect("admitted");
            assert_eq!(snap.pending_dials(), 1);
        }
        assert_eq!(snap.pending_dials(), 0, "the slot came back on drop");
        drop(
            snap.admit(&request(P1, "/a"), 0)
                .expect("and is usable again"),
        );
    }

    #[test]
    fn a_denied_dial_cannot_advance_or_reset_retry_state() {
        // ADR-0011: "a denied dial must not silently reset
        // ConnectionManager retry state." Expressed structurally rather
        // than as a rule to remember -- recording an outcome requires a
        // ticket, and a denial produces none, so there is no call a
        // caller could make.
        let mut m = manager(8);
        let p = peer(P1);

        let t = m
            .handle()
            .load()
            .admit(&request(P1, "/a"), 0)
            .expect("admitted");
        m.record_failure(t, 0);
        assert_eq!(m.scheduled_retries(), 1);
        let after_first = m.due_retries(0);
        assert!(after_first.is_empty(), "not due yet");

        // A behaviour-originated dial for the same peer is now refused
        // by backoff. The refusal must leave the schedule exactly as it
        // was -- neither cleared nor advanced.
        let before = m.revision();
        let denied = m.handle().load().admit(
            &DialRequest {
                peer: Some(p.clone()),
                address: "/a".to_owned(),
                origin: DialOrigin::KademliaQuery,
            },
            1_000,
        );
        assert!(denied.is_err(), "backoff refuses it");
        assert_eq!(m.scheduled_retries(), 1, "the schedule is untouched");
        assert_eq!(m.revision(), before, "and nothing was republished");
        assert!(m.due_retries(1_000).is_empty(), "and it is still not due");
    }

    #[test]
    fn the_retry_cadence_is_the_one_connectivity_md_states() {
        // `architecture/transport/libp2p/CONNECTIVITY.md`: 30 s,
        // exponential, bounded by 5 min. Restated as numbers in this
        // module, so this test is what turns a drift into a failure.
        let mut m = manager(64);
        let mut delays = Vec::new();
        let mut now = 0u64;
        for _ in 0..10 {
            let t = m.handle().load().admit(&request(P1, "/a"), now).ok();
            // Once backoff bites, drive the clock forward to the moment
            // the schedule says the peer is due -- which is the point of
            // the test: the two must agree.
            let Some(t) = t else {
                now += 1_000;
                continue;
            };
            let due_before = m.scheduled_retries();
            m.record_failure(t, now);
            assert!(m.scheduled_retries() >= due_before);
            let next = m.due_retries(u64::MAX);
            assert_eq!(next, vec![peer(P1)]);
            // Advance to exactly when it becomes due and note the gap.
            let mut probe = now;
            while m.due_retries(probe).is_empty() {
                probe += 1_000;
            }
            delays.push(probe - now);
            now = probe;
        }
        assert_eq!(delays.first(), Some(&30_000), "the first wait is 30 s");
        assert!(
            delays.windows(2).all(|w| w[1] >= w[0]),
            "the cadence never shortens: {delays:?}"
        );
        assert!(
            delays.iter().all(|d| *d <= 5 * 60 * 1_000),
            "and is bounded by five minutes: {delays:?}"
        );
        assert!(
            delays.contains(&(5 * 60 * 1_000)),
            "and actually reaches the ceiling: {delays:?}"
        );
    }

    #[test]
    fn a_success_clears_the_retry_and_republishes() {
        let mut m = manager(8);
        let t = m
            .handle()
            .load()
            .admit(&request(P1, "/a"), 0)
            .expect("admitted");
        m.record_failure(t, 0);
        assert_eq!(m.scheduled_retries(), 1);

        let handle = m.handle();
        let stale = handle.load();
        let t = handle
            .load()
            .admit(&request(P1, "/a"), 40_000)
            .expect("past the backoff");
        drop(m.record_success(t, 40_000));
        assert_eq!(m.scheduled_retries(), 0, "a success clears the schedule");

        // PROMPTLY, in ADR-0011's word: the handle sees the new policy
        // without being told, and the older snapshot is observably
        // older rather than silently equivalent.
        assert!(
            handle.load().revision() > stale.revision(),
            "the handle must see a newer revision"
        );
    }

    #[test]
    fn revoking_authorization_reaches_a_holder_that_never_asked() {
        // The staleness that is not merely a timing detail. A holder
        // caches the handle, not the snapshot, so a revocation lands on
        // its next decision rather than whenever it thinks to refresh.
        let mut m = manager(8);
        let handle = m.handle();
        drop(
            handle
                .load()
                .admit(&request(P1, "/a"), 0)
                .expect("permitted while trusted"),
        );

        m.begin_shutdown();

        assert_eq!(
            handle.load().admit(&request(P1, "/a"), 0).err(),
            Some(DialDenial::ShuttingDown),
            "the same handle, no refresh, current answer"
        );
    }

    #[test]
    fn a_snapshot_taken_before_shutdown_stops_admitting() {
        // The holder that DID cache the snapshot -- the case the test
        // above does not cover, because it reloads. Draining was a
        // photographed bool, so an `Arc` taken one instant before
        // `begin_shutdown` went on admitting dials for as long as
        // anything kept it, and the manager had no way to reach it.
        let mut m = manager(8);
        let stale = m.handle().load();
        drop(
            stale
                .admit(&request(P1, "/a"), 0)
                .expect("permitted while running"),
        );

        m.begin_shutdown();

        assert_eq!(
            stale.admit(&request(P1, "/a"), 0).err(),
            Some(DialDenial::ShuttingDown),
            "the snapshot it already held, and it refuses"
        );
    }

    #[test]
    fn a_superseded_snapshot_decides_nothing() {
        // Everything except drain and the pending count is read from the
        // photograph, so a retained `Arc` would answer with whatever
        // authorization, backoff and quarantine state was current when
        // it was taken -- indefinitely. Publication now makes the old
        // snapshot refuse instead, which bounds policy staleness to the
        // interval between load and decision rather than to the holder's
        // lifetime.
        let mut m = manager(8);
        let stale = m.handle().load();
        let revision = stale.revision();

        // Any mutation publishes. This one is a success, so nothing
        // about it would have denied the dial below on the merits.
        let ticket = m.handle().admit(&request(P1, "/a"), 0).expect("admitted");
        drop(m.record_success(ticket, 0));
        assert!(m.revision() > revision, "the mutation published");

        assert_eq!(
            stale.admit(&request(P1, "/a"), 0).err(),
            Some(DialDenial::PolicySuperseded),
            "an obsolete photograph is not an authorization"
        );
        assert_eq!(
            stale.pending_dials(),
            0,
            "and a refusal reserves nothing, superseded or not"
        );

        // The remedy, and the reason the refusal is recoverable: ask
        // through the handle and it reloads.
        let fresh = m
            .handle()
            .admit(&request(P1, "/a"), 0)
            .expect("the current snapshot admits");
        drop(fresh);
    }

    #[test]
    fn a_publication_during_admission_is_not_outrun() {
        // THE WINDOW BETWEEN THE CHECK AND THE DECISION. The freshness
        // test happens before the policy read and the two reservations;
        // a quarantine, revocation or drain published in that gap would
        // otherwise be applied by a snapshot that had already passed
        // its only test, and the caller would get a ticket issued under
        // policy that no longer exists.
        let m = std::rc::Rc::new(std::cell::RefCell::new(manager(8)));
        let stale = m.borrow().handle().load();

        let publisher = std::rc::Rc::clone(&m);
        DURING_ADMIT.with(|h| {
            *h.borrow_mut() = Some(Box::new(move || publisher.borrow_mut().publish()));
        });

        assert_eq!(
            stale.admit(&request(P1, "/a"), 0).err(),
            Some(DialDenial::PolicySuperseded),
            "a publication concurrent with the decision refuses it"
        );
        // ROLLED BACK. A refusal that kept its reservations would leak
        // one dial slot and one connection slot per lost race, and the
        // ceilings would decay under exactly the load that makes the
        // race likely.
        assert_eq!(stale.pending_dials(), 0, "the pending slot came back");
        assert_eq!(stale.connections(), 0, "and so did the connection slot");
    }

    #[test]
    fn concurrent_dials_cannot_exceed_the_connection_ceiling() {
        // Counting connections only once they establish let every dial
        // admitted before the first one connected see a count of zero,
        // so a ceiling of one admitted as many concurrent dials as the
        // pending budget allowed. The slot is reserved by the
        // admission and carried by the ticket, so the second dial is
        // refused while the first is still in flight.
        let m = manager_holding(1);
        let first = m
            .handle()
            .admit(&request(P1, "/a"), 0)
            .expect("the only connection slot");
        assert_eq!(m.connections(), 1, "reserved at admission, not at connect");

        assert_eq!(
            m.handle().admit(&request(P1, "/b"), 0).err(),
            Some(DialDenial::ConnectionLimitReached),
            "nothing has connected yet, and that is the point"
        );

        drop(first);
        assert_eq!(m.connections(), 0, "an abandoned dial frees its slot");
        drop(
            m.handle()
                .admit(&request(P1, "/b"), 0)
                .expect("and the ceiling is usable again"),
        );
    }

    fn trusting(data_plane: &[&str], infrastructure: &[&str]) -> TrustSources {
        TrustSources {
            peers: PeerTrustPolicy::new(data_plane.iter().map(|p| peer(p))).expect("small"),
            infrastructure: InfrastructureSet::new(infrastructure.iter().map(|p| peer(p)))
                .expect("small"),
        }
    }

    #[test]
    fn nobody_is_trusted_until_somebody_says_so() {
        // ADR-0012's default, and the one a hardcoded
        // `ConnectionClass::DataPlaneTrusted` at the dial site quietly
        // inverted: an empty configuration admitted everyone for
        // everything, and no test noticed because every test passed the
        // class it wanted.
        let m = untrusting(8);
        assert_eq!(m.classify(&peer(P1)), ConnectionClass::Unauthorized);
        assert_eq!(
            m.handle().admit(&request(P1, "/a"), 0).err(),
            Some(DialDenial::Unauthorized),
            "an unclassified peer is not dialable"
        );
    }

    #[test]
    fn the_two_authorities_stay_separate() {
        // ADR-0036: infrastructure authorization is a DIFFERENT
        // permission, not a weaker data-plane trust. A relay this
        // profile uses to be reachable must not thereby become a peer
        // it will exchange application messages with.
        let mut m = untrusting(8);
        let _ = m.set_trust(trusting(&[P1], &[P2]), &[]);

        assert_eq!(m.classify(&peer(P1)), ConnectionClass::DataPlaneTrusted);
        assert_eq!(
            m.classify(&peer(P2)),
            ConnectionClass::ConnectivityInfrastructureOnly
        );
        assert_eq!(
            m.handle().admit(&request(P2, "/a"), 0).err(),
            Some(DialDenial::NotAuthorizedForDataPlane),
            "the infrastructure peer is refused the data plane it was never granted"
        );
    }

    #[test]
    fn revoking_trust_names_the_connections_that_must_go() {
        // ADR-0012 requires a removal to evict active connectivity, not
        // merely to change what the next dial is told. This crate owns
        // no connections, so it reports what the caller has to close --
        // and reporting nothing, which is what an unpublished trust
        // change amounted to, is how a revoked peer keeps its session.
        let mut m = untrusting(8);
        let live = [peer(P1), peer(P2)];
        let _ = m.set_trust(trusting(&[P1, P2], &[]), &live);

        let revoked = m.set_trust(trusting(&[P1], &[P2]), &live);
        assert_eq!(
            revoked,
            vec![Revoked {
                peer: peer(P2),
                was: ConnectionClass::DataPlaneTrusted,
                now: ConnectionClass::ConnectivityInfrastructureOnly,
            }],
            "P2 lost the data plane and must be evicted from it; P1 changed nothing"
        );
    }

    #[test]
    fn a_widened_authorization_evicts_nothing() {
        // The other direction. Granting a peer MORE must not tear down
        // the connection it already has: an eviction list computed from
        // "the class changed" rather than "the class narrowed" would
        // drop a live session every time an operator added a
        // permission.
        let mut m = untrusting(8);
        let live = [peer(P1)];
        let _ = m.set_trust(trusting(&[], &[P1]), &live);

        assert!(
            m.set_trust(trusting(&[P1], &[P1]), &live).is_empty(),
            "promotion to the data plane is not a revocation"
        );
    }

    #[test]
    fn inbound_is_judged_by_the_same_authorization_as_outbound() {
        // ADR-0011: current authorization applies before an inbound
        // data-plane connection is RETAINED. Arriving is not an
        // authorization, and an infrastructure peer authorized for
        // reachability is not thereby a data-plane peer.
        let mut m = manager(8);
        assert!(m.retain_inbound(ConnectionClass::DataPlaneTrusted));
        assert!(!m.retain_inbound(ConnectionClass::ConnectivityInfrastructureOnly));
        assert!(!m.retain_inbound(ConnectionClass::Unauthorized));

        m.begin_shutdown();
        assert!(
            !m.retain_inbound(ConnectionClass::DataPlaneTrusted),
            "a draining runtime keeps nothing new"
        );
    }

    #[test]
    fn the_retry_table_is_bounded_and_keeps_the_soonest() {
        // A map keyed by peer is state a remote party grows by failing
        // to connect. When it is full the entry that would wait longest
        // is forgotten: refusing the NEWEST failure instead would leave
        // the peer that just failed with no schedule at all, and a peer
        // with no schedule is one nothing is holding back.
        let mut m = ConnectionManager::new(ConnectionPolicy::new(4096, 64), 4096);
        m.max_retry_entries = 4;
        let generated: Vec<String> = (0..8u32)
            .map(|i| {
                format!(
                    "Qm{}",
                    format!("{i:044}").replace('0', "a")[..44].to_owned()
                )
            })
            .collect();
        // Trusted first, because the gate classifies now and an
        // unauthorized peer never reaches the retry table at all.
        let _ = m.set_trust(
            TrustSources {
                peers: PeerTrustPolicy::new(generated.iter().map(|p| peer(p))).expect("small"),
                infrastructure: InfrastructureSet::default(),
            },
            &[],
        );
        for (i, p) in (0..8u32).zip(generated.iter()) {
            let t = m
                .handle()
                .load()
                .admit(
                    &DialRequest {
                        peer: Some(peer(p)),
                        address: format!("/ip4/10.0.0.{i}/tcp/1"),
                        origin: DialOrigin::ConnectionManager,
                    },
                    u64::from(i) * 1_000,
                )
                .expect("admitted");
            m.record_failure(t, u64::from(i) * 1_000);
        }
        assert_eq!(m.scheduled_retries(), 4, "the table is bounded");
    }
}
