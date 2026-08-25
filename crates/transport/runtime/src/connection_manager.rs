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

use std::sync::Weak;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
    /// This profile's own identity, when the manager knows it.
    ///
    /// Private, and set only by [`ConnectionManager::set_trust`] from
    /// the authoritative value the runtime holds -- never by whoever
    /// supplies the two sets. A caller that could name the local peer
    /// could also name a different one, which is the confusion this
    /// exists to prevent rather than a flexibility worth offering.
    local_peer: Option<TransportIdentity>,
}

impl TrustSources {
    /// Build the trust sources from the two authorities a
    /// configuration supplies.
    ///
    /// A constructor rather than a struct literal because the local
    /// identity is deliberately not one of the inputs: it is bound by
    /// [`ConnectionManager::bind_local_peer`] from the value the
    /// runtime derived from its own keypair. A caller that could pass
    /// it could also pass a different one, and "who am I" is not a
    /// question configuration gets to answer.
    #[must_use]
    pub fn new(peers: PeerTrustPolicy, infrastructure: InfrastructureSet) -> Self {
        Self {
            peers,
            infrastructure,
            local_peer: None,
        }
    }

    /// The class this profile grants `peer`, right now.
    ///
    /// Data-plane trust is checked first and wins, because it is the
    /// broader authority: a peer in both sets may do everything the
    /// infrastructure set would have permitted. A peer in neither is
    /// [`ConnectionClass::Unauthorized`], which is the DEFAULT answer --
    /// an empty configuration admits nobody (ADR-0012), and there is no
    /// constructor here that says otherwise.
    ///
    /// THE LOCAL PEER IS NEVER ANY OTHER CLASS. A configuration listing
    /// this profile's own identity is a mistake -- a copied allowlist,
    /// a template filled in wrong -- and treating it as an ordinary
    /// trusted remote would let self-directed admission, retries and
    /// address-book entries all proceed for a peer that cannot be
    /// dialed. Answered here rather than at each call site because
    /// there are three of them and a fourth is one commit away.
    #[must_use]
    pub fn classify(&self, peer: &TransportIdentity) -> ConnectionClass {
        if self.local_peer.as_ref() == Some(peer) {
            return ConnectionClass::Unauthorized;
        }
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
    /// A WEAK reference back to the single cell that holds whichever
    /// snapshot is currently published.
    ///
    /// Not `published_revision: Arc<AtomicU64>`, which this replaces.
    /// That was a second piece of shared state, written in a SEPARATE
    /// step from installing the new snapshot in the cell -- between
    /// the two, an old snapshot's own revision could still equal the
    /// not-yet-updated atomic, so it read as fresh and decided against
    /// policy that had already been superseded. Reading the live
    /// revision back out of the SAME cell every other reader consults
    /// leaves nothing that can disagree with it, because there is only
    /// one write.
    ///
    /// Weak, not `Arc`: the cell holds an `Arc<PolicySnapshot>`, so a
    /// strong reference back to the cell from inside the snapshot it
    /// contains would be a genuine reference cycle -- neither side
    /// could ever be dropped. A weak reference breaks it; the manager
    /// itself holds the one strong reference that keeps the cell alive.
    current: Weak<RwLock<Arc<PolicySnapshot>>>,
    /// A WEAK reference to a token the manager alone owns.
    ///
    /// Separate from `current`, and it has to be: `SnapshotHandle` holds
    /// a STRONG `Arc` to the cell so it can read it, so the cell outlives
    /// the manager whenever a handle does. Asking "does the cell still
    /// exist" therefore answers "does anyone still hold a handle", which
    /// is not the question -- and a caller that kept a handle and dropped
    /// its manager went on being admitted indefinitely against the final
    /// snapshot, which the docs on [`Self::is_current`] flatly promised
    /// could not happen.
    ///
    /// [`ManagerLiveness`] exists to be owned by exactly one place. No
    /// handle, snapshot, or ticket holds a strong reference to it, so its
    /// upgrade failing means the manager itself is gone.
    manager: Weak<ManagerLiveness>,
}

/// A token whose only property is who owns it.
///
/// Zero-sized. [`ConnectionManager`] holds the sole `Arc`; snapshots hold
/// a `Weak`. Nothing else may hold a strong reference -- that is the
/// entire contract, and it is what makes `Weak::upgrade` returning `None`
/// mean "the manager is gone" rather than "nobody is looking any more".
#[derive(Debug)]
pub(crate) struct ManagerLiveness;

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

    /// Whether this is the snapshot currently published.
    ///
    /// Two questions, and they are genuinely different:
    ///
    /// 1. **Does the manager still exist?** Asked of [`ManagerLiveness`],
    ///    which only the manager owns. If it is gone nothing can ever
    ///    publish again, so there is no "current" to be, and refusing is
    ///    the fail-closed answer to a question that no longer has one.
    /// 2. **Is this the snapshot in the cell?** `self.revision` against
    ///    the currently installed snapshot's own revision field, fetched
    ///    fresh through the one cell every snapshot and handle shares.
    ///
    /// The first used to be asked of the CELL, which a `SnapshotHandle`
    /// keeps alive by holding a strong `Arc` to it. A caller that dropped
    /// its manager while retaining a handle therefore kept upgrading
    /// successfully, kept matching the final revision, and kept being
    /// admitted -- for as long as it held the handle.
    ///
    /// Enforced by `a_handle_that_outlives_its_manager_admits_nothing`
    /// and `the_liveness_token_is_owned_by_the_manager_alone`.
    #[must_use]
    fn is_current(&self) -> bool {
        if self.manager.upgrade().is_none() {
            return false;
        }
        self.current.upgrade().is_some_and(|cell| {
            self.revision == cell.read().unwrap_or_else(|e| e.into_inner()).revision
        })
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
        //
        // ONE READ of the ONE place that says what is current: the cell
        // this snapshot came from, upgraded and read fresh. Comparing
        // against a second value published in a separate step is what
        // let an old snapshot pass this check during the instant between
        // that value's two writes; there is only one write now.
        if !self.is_current() {
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
        if self.shutting_down.load(Ordering::Acquire) || !self.is_current() {
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
    /// Why this dial was asked for.
    ///
    /// Read for exactly one question: does settling this ticket own the
    /// peer's SCHEDULER CLAIM? Only the reconnect scheduler claims, so
    /// only a `ConnectionManager`-origin ticket may consume or release
    /// one. A manual dial settling must leave the schedule alone --
    /// the retry entry is peer-scoped while a dial outcome is
    /// address-scoped, and conflating them let one bad address cancel
    /// the reconnect that would have tried a good one.
    origin: DialOrigin,
    peer: Option<TransportIdentity>,
    address: String,
    settled: bool,
    connection_kept: bool,
}

impl DialTicket {
    /// The peer this permission was granted for, if one was named.
    #[must_use]
    pub const fn peer(&self) -> Option<&TransportIdentity> {
        self.peer.as_ref()
    }

    /// Why this dial was asked for.
    ///
    /// Read at establishment: ADR-0036's separation is an origin/class
    /// PAIR, so revalidating an outbound connection needs the reason it
    /// was opened, not only what the peer is authorized for.
    #[must_use]
    pub const fn origin(&self) -> DialOrigin {
        self.origin
    }

    /// Whether settling this ticket owns the peer's scheduler claim.
    ///
    /// Only the reconnect scheduler claims a retry entry, so only its
    /// own dials may consume or release one.
    const fn owns_scheduler_claim(&self) -> bool {
        matches!(self.origin, DialOrigin::ConnectionManager)
    }

    /// The address this permission was granted for.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
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
    /// A scheduler tick has claimed this peer and an attempt is under
    /// way that has not yet been settled. A claimed entry is never
    /// returned by [`ConnectionManager::take_due_retries`] again, which
    /// is what stops the same slow dial from being started twice.
    claimed: bool,
}

/// Default ceiling on addresses remembered for one peer.
///
/// Small, because the list is written by the peer itself: Identify
/// reports whatever addresses it cares to claim, and a peer that
/// claimed a thousand would otherwise cost this profile a thousand
/// entries and a thousand dial candidates. Eight is enough for a host
/// with several interfaces and a relayed address, which is the case
/// the bound exists to serve rather than to punish.
pub const DEFAULT_MAX_ADDRESSES_PER_PEER: usize = 8;

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
    /// This profile's own identity, once the runtime has said what it
    /// is. `None` until then, which is the honest answer for a manager
    /// constructed by a test that never had one.
    local_peer: Option<TransportIdentity>,
    retries: std::collections::BTreeMap<TransportIdentity, Retry>,
    /// Candidate addresses per peer.
    ///
    /// Bounded twice over: entries exist only for peers the trust
    /// sources classify as something other than `Unauthorized`, so the
    /// number of keys is bounded by the allowlist rather than by
    /// whoever connects, and each key holds at most
    /// `max_addresses_per_peer`.
    book: std::collections::BTreeMap<TransportIdentity, std::collections::BTreeSet<String>>,
    max_addresses_per_peer: usize,
    max_retry_entries: usize,
    published: Arc<RwLock<Arc<PolicySnapshot>>>,
    /// The SOLE strong reference to this manager's liveness token.
    ///
    /// Dropping the manager drops this, and every snapshot it ever
    /// published starts refusing. Handing a clone of this `Arc` to
    /// anything else silently restores the defect it exists to close.
    alive: Arc<ManagerLiveness>,
}

impl ConnectionManager {
    /// Build a manager around a connection policy.
    #[must_use]
    pub fn new(policy: ConnectionPolicy, max_pending_dials: usize) -> Self {
        let pending = Arc::new(AtomicUsize::new(0));
        let connections = Arc::new(AtomicUsize::new(0));
        let max_connections = policy.max_connections;
        let shutting_down = Arc::new(AtomicBool::new(false));
        let trust = Arc::new(TrustSources::default());
        let alive = Arc::new(ManagerLiveness);

        // BUILT WITH `Arc::new_cyclic`, because the first snapshot has
        // to hold a weak reference to the very cell it is about to be
        // installed in -- and that cell does not exist until this call
        // returns. The closure receives a `Weak` to what the `Arc`
        // will become, which can be cloned and stored before the outer
        // `Arc` finishes constructing, and is not usable (upgrading
        // returns `None`) until it does. Nothing here upgrades it
        // early; it is only stored.
        let published: Arc<RwLock<Arc<PolicySnapshot>>> = Arc::new_cyclic(|weak| {
            RwLock::new(Arc::new(PolicySnapshot {
                policy: policy.clone(),
                trust: Arc::clone(&trust),
                revision: 0,
                pending: Arc::clone(&pending),
                max_pending_dials,
                connections: Arc::clone(&connections),
                max_connections,
                shutting_down: Arc::clone(&shutting_down),
                current: weak.clone(),
                manager: Arc::downgrade(&alive),
            }))
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
            local_peer: None,
            retries: std::collections::BTreeMap::new(),
            book: std::collections::BTreeMap::new(),
            max_addresses_per_peer: DEFAULT_MAX_ADDRESSES_PER_PEER,
            max_retry_entries: DEFAULT_MAX_RETRY_ENTRIES,
            published,
            alive,
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
            current: Arc::downgrade(&self.published),
            manager: Arc::downgrade(&self.alive),
        });
        // ONE WRITE. The revision a reader compares itself against IS
        // this cell's own content now, not a second value kept in step
        // with it by hand -- there is no longer a window between "the
        // new snapshot is installed" and "the fact that it is current
        // becomes visible", because those are the same write.
        *self.published.write().unwrap_or_else(|e| e.into_inner()) = next;
    }

    /// Tell the manager which identity is this profile's own.
    ///
    /// Called by the runtime, from the value it derived from the
    /// keypair -- the authoritative one. Every classification from here
    /// on answers [`ConnectionClass::Unauthorized`] for that identity,
    /// whatever a configured allowlist says, so a mistaken self-entry
    /// cannot reach admission, retries, or the address book.
    ///
    /// Rebinds the currently published trust immediately rather than
    /// waiting for the next [`Self::set_trust`], so there is no window
    /// in which the local peer is bound in the manager but not in what
    /// the gate is reading.
    pub fn bind_local_peer(&mut self, local: TransportIdentity) {
        self.local_peer = Some(local);
        let mut trust = (*self.trust).clone();
        trust.local_peer = self.local_peer.clone();
        self.trust = Arc::new(trust);
        self.publish();
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
        // REBOUND ON EVERY CHANGE, from the manager's own copy rather
        // than from what the caller supplied. A `TrustSources` handed in
        // by configuration cannot name the local peer -- the field is
        // private -- so a later trust update cannot unbind it either,
        // whether by omission or by naming a different identity.
        let mut trust = trust;
        trust.local_peer = self.local_peer.clone();
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

    /// Remember an address as a candidate for `peer`.
    ///
    /// Returns whether it was remembered. Refused for a peer this
    /// profile does not classify: an address book keyed by anyone who
    /// can send an Identify message is a map an unauthorized party
    /// grows, and the trust allowlist is what bounds the key set.
    ///
    /// Addresses reaching here from Identify are ADVISORY -- the peer
    /// asserted them about itself. Remembering one is not trust, not
    /// proof of reachability, and not permission to dial: every dial
    /// still passes admission, which is where a quarantined address is
    /// refused.
    ///
    /// When the per-peer list is full, an address the policy will not
    /// currently dial makes way for the new one. A dialable address is
    /// never displaced, so a peer cannot flush its own known-good route
    /// by asserting eight new ones -- and the displaced address keeps
    /// its quarantine, which lives in the policy rather than here, so
    /// eviction launders nothing.
    pub fn learn_address(&mut self, peer: &TransportIdentity, address: &str, now_ms: u64) -> bool {
        if matches!(self.classify(peer), ConnectionClass::Unauthorized) {
            return false;
        }
        let max = self.max_addresses_per_peer;
        let policy = &self.policy;
        let known = self.book.entry(peer.clone()).or_default();
        if known.contains(address) {
            return true;
        }
        if known.len() >= max {
            let evictable = known
                .iter()
                .find(|a| !policy.is_address_dialable(peer, a, now_ms))
                .cloned();
            match evictable {
                Some(stale) => {
                    known.remove(&stale);
                }
                None => return false,
            }
        }
        known.insert(address.to_owned());
        true
    }

    /// Addresses to try for `peer`, known-good first.
    ///
    /// The order [`ConnectionPolicy::preferred_addresses`] computes,
    /// which until now nothing asked for: a peer with a working route
    /// and a quarantined one was dialed at whichever address the caller
    /// happened to hold.
    #[must_use]
    pub fn dial_candidates(&self, peer: &TransportIdentity, now_ms: u64) -> Vec<String> {
        let known: Vec<String> = self
            .book
            .get(peer)
            .map(|a| a.iter().cloned().collect())
            .unwrap_or_default();
        self.policy.preferred_addresses(peer, &known, now_ms)
    }

    /// How many addresses are remembered for `peer`.
    #[must_use]
    pub fn known_addresses(&self, peer: &TransportIdentity) -> usize {
        self.book
            .get(peer)
            .map_or(0, std::collections::BTreeSet::len)
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
    ///
    /// For a TRANSIENT failure -- the network refused, timed out, or
    /// reset the attempt. A structural one -- an address this profile
    /// cannot dial at all -- is [`Self::record_permanent_failure`], and
    /// answering "will retrying help" is the caller's job because only
    /// the backend knows which `DialError` it received.
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
            // REMEMBER THE ADDRESS WE JUST TRIED, or the retry we are
            // about to schedule has nothing to dial.
            //
            // `dial_candidates` reads the address book and nothing else,
            // and the public `dial(peer, address)` API admits an address
            // that was never learned through Identify. A transient
            // failure there scheduled a retry against an EMPTY book, so
            // the first tick found no candidates and cleared the entry
            // -- which removes it outright, not merely its claim -- and
            // nothing recreates it, so learning the address afterwards
            // could not restart the peer either. One transient refusal
            // on a directly-dialled address ended reconnection for that
            // peer permanently.
            //
            // `learn_address` is the same bounded, authorization-checked
            // path Identify uses: it refuses an unauthorized peer, caps
            // the list at `max_addresses_per_peer`, and evicts only an
            // address the policy will not currently dial. An address
            // this profile actually attempted is at least as good a
            // candidate as one a peer asserted about itself.
            self.learn_address(&peer, ticket.address(), now_ms);
            // A CLAIM THIS TICKET DOES NOT OWN SURVIVES THE RESCHEDULE.
            // Rewriting the entry with `claimed: false` was correct for
            // the scheduler's own dial -- that is how a claim is given
            // back -- and wrong for every other origin: a manual dial to
            // a second address can fail transiently while the
            // scheduler's dial is still in flight, and clearing the flag
            // there makes the entry due again with a dial already
            // running. The next tick then starts the duplicate that
            // claiming exists to prevent.
            let held_by_another = !ticket.owns_scheduler_claim()
                && self.retries.get(&peer).is_some_and(|entry| entry.claimed);
            self.schedule_retry(peer, now_ms, delay, held_by_another);
        }
        self.settle(ticket);
        self.publish();
    }

    /// Record a failure that retrying cannot fix.
    ///
    /// A `MultiaddrNotSupported`, `NoAddresses`, or `LocalPeerId` dial
    /// error describes this profile's own transport stack, not the
    /// remote end's availability -- the same address fails the same way
    /// every time, indefinitely, which the paused-time scheduler test
    /// caught: a UDP address on a TCP-only Swarm was scheduled and
    /// retried forever by [`Self::record_failure`]'s unconditional
    /// reschedule.
    ///
    /// The ticket is settled and NOTHING is rescheduled. If the peer was
    /// claimed from [`Self::take_due_retries`], it simply does not
    /// re-enter the table; if it has another address, that address is
    /// untouched by this call and remains a candidate on its own merit.
    pub fn record_permanent_failure(&mut self, ticket: DialTicket, now_ms: u64) {
        let _ = now_ms;
        if let Some(peer) = ticket.peer().cloned() {
            // THE ADDRESS IS UNUSABLE, NOT THE PEER. This used to remove
            // the peer's whole retry entry, which is peer-scoped while
            // the failure is address-scoped: a manual dial to one bad
            // address cancelled the scheduled reconnect that would have
            // tried a good one still sitting in the book.
            //
            // Forgetting the address is what stops the loop the old
            // unconditional reschedule created -- a scheduler claim is
            // released rather than consumed, so the next tick tries the
            // peer's OTHER addresses, and if there are none
            // `dial_candidates` comes back empty and the scheduler
            // clears the claim itself.
            if let Some(known) = self.book.get_mut(&peer) {
                known.remove(ticket.address());
                if known.is_empty() {
                    self.book.remove(&peer);
                }
            }
            if ticket.owns_scheduler_claim() {
                self.release_retry_claim(&peer);
            }
        }
        self.settle(ticket);
        self.publish();
    }

    /// The handshake succeeded, and admission is no longer what it was
    /// when this ticket was issued.
    ///
    /// The window between an outbound dial being admitted and its
    /// handshake completing is exactly the window a trust revocation or
    /// a drain can land in. Recording the outcome as an ordinary
    /// success would retain a connection under authority that no longer
    /// exists; recording it as an ordinary FAILURE would be wrong too --
    /// nothing about the network or the remote peer failed, and
    /// scheduling a retry for a peer this profile no longer trusts
    /// would be the same mistake the trust-revocation eviction path
    /// exists to prevent, reached from a different direction.
    ///
    /// Settled with neither backoff nor a retry, and no reschedule for
    /// the same reason [`Self::record_permanent_failure`] schedules
    /// none: a peer becoming trusted again is not this method's job to
    /// notice.
    pub fn record_authorization_withdrawn(&mut self, ticket: DialTicket, now_ms: u64) {
        let _ = now_ms;
        // CLEARED, not released: unlike a quarantine or an unusable
        // address, this is not a fact about one route. The peer is no
        // longer authorized, so there is nothing for a later tick to
        // try, and leaving the entry claimed would strand it -- never
        // selected again, still holding retry-table capacity.
        if ticket.owns_scheduler_claim()
            && let Some(peer) = ticket.peer().cloned()
        {
            self.clear_retry_claim(&peer);
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
        // A CLAIMED ATTEMPT THAT ENDS HERE MUST GIVE THE CLAIM BACK.
        // The quarantine is address-scoped and the peer may have other
        // routes, so the entry is released rather than removed -- but
        // it was released by NOTHING before this, so a scheduled retry
        // ending in a wrong-key answer left the entry claimed forever:
        // permanently excluded from selection, permanently occupying
        // retry-table capacity, and the peer's good addresses never
        // tried again.
        if ticket.owns_scheduler_claim()
            && let Some(peer) = ticket.peer().cloned()
        {
            self.release_retry_claim(&peer);
        }
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

    /// Whether a connection of this class is still authorized to be
    /// held open, RIGHT NOW.
    ///
    /// ADR-0011: the same CURRENT authorization policy that governs an
    /// admission decision applies before a connection is RETAINED,
    /// whichever direction it started in. Inbound is not a way in for a
    /// peer that outbound would refuse -- "it connected to us" is not
    /// an authorization -- and an outbound connection whose handshake
    /// outlasted a revocation or a drain is not grandfathered in just
    /// because admission approved the dial that produced it.
    #[must_use]
    pub fn authorizes(&self, class: ConnectionClass) -> bool {
        self.authorizes_for(class, DialOrigin::Manual)
    }

    /// Whether a connection of this class, opened for this REASON, is
    /// still authorized to be held open.
    ///
    /// ADR-0036's separation is a pair, not a class alone: an
    /// infrastructure-only peer is dialable for reachability and
    /// refused for the data plane, on the same address in the same
    /// moment. `ConnectionPolicy::admit` has always decided it that
    /// way -- `origin.is_data_plane()` is the discriminator, and
    /// `tests/transport-contract` pins every origin/class pair.
    ///
    /// Revalidating an established connection with the data-plane-only
    /// predicate therefore closed connections admission had correctly
    /// permitted: a relay reservation, a relay circuit, an AutoNAT
    /// probe or a DCUtR hole punch to an infrastructure peer completed
    /// its handshake and was immediately dropped. The inbound path has
    /// no origin to consult and no such pair to honour, which is why it
    /// keeps the stricter predicate and why applying that one to
    /// outbound was wrong rather than merely conservative.
    #[must_use]
    pub fn authorizes_for(&self, class: ConnectionClass, origin: DialOrigin) -> bool {
        if self.shutting_down.load(Ordering::Acquire) {
            return false;
        }
        match class {
            ConnectionClass::Unauthorized => false,
            ConnectionClass::DataPlaneTrusted => true,
            ConnectionClass::ConnectivityInfrastructureOnly => !origin.is_data_plane(),
        }
    }

    /// Whether draining has begun.
    ///
    /// Read by the direct-admission path, which must refuse new work for
    /// the same reason inbound connections are refused: this node is
    /// about to drop what it is holding.
    #[must_use]
    pub fn is_draining(&self) -> bool {
        self.shutting_down.load(Ordering::Acquire)
    }

    /// Begin draining. Admission refuses from the next snapshot on.
    pub fn begin_shutdown(&mut self) {
        self.shutting_down.store(true, Ordering::Release);
        self.policy.shutting_down = true;
        self.publish();
    }

    /// Claim up to `limit` due retries, soonest first, REMOVING them
    /// from the schedule.
    ///
    /// The read-only predecessor of this method returned the same
    /// entries on every tick until something else cleared them, which
    /// produced three failures at once: a slow dial still pending when
    /// the next tick fired got dialled again, because nothing recorded
    /// that an attempt was already under way; a peer stuck at the front
    /// with no usable address could consume every scheduler selection
    /// forever, because reading it changed nothing about its position;
    /// and there was no way to tell "claimed, an attempt is in flight"
    /// from "still waiting its turn".
    ///
    /// Claiming is unconditional and REMOVES the entry. A caller that
    /// cannot start a dial this tick -- no candidate address, the peer
    /// is no longer authorized -- must not put it back on a hair
    /// trigger: doing nothing here is correct, because a peer with
    /// nothing to try is not usefully "due" again a moment later. A
    /// caller whose dial genuinely fails re-enters the schedule through
    /// [`Self::record_failure`], which is the same path any other
    /// failed dial uses and carries its own backoff.
    #[must_use]
    pub fn take_due_retries(&mut self, now_ms: u64, limit: usize) -> Vec<TransportIdentity> {
        let mut due: Vec<(TransportIdentity, u64)> = self
            .retries
            .iter()
            .filter(|(_, r)| !r.claimed && now_ms >= r.due_at_ms)
            .map(|(p, r)| (p.clone(), r.due_at_ms))
            .collect();
        due.sort_by_key(|(_, at)| *at);
        due.truncate(limit);
        for (peer, _) in &due {
            if let Some(entry) = self.retries.get_mut(peer) {
                entry.claimed = true;
            }
        }
        due.into_iter().map(|(p, _)| p).collect()
    }

    /// Give up a claim without ever producing a ticket to settle it
    /// with -- there was nothing to dial, or authorization no longer
    /// permits it. Removes the entry outright: a peer with no candidate
    /// address, or one this profile no longer trusts, gains nothing
    /// from being reconsidered a moment later.
    pub fn clear_retry_claim(&mut self, peer: &TransportIdentity) {
        self.retries.remove(peer);
    }

    /// Take the claim back, for a scheduler tick that released one and
    /// then started a later candidate anyway.
    ///
    /// A scheduled retry with several addresses can have an early one
    /// fail SYNCHRONOUSLY -- an address this profile cannot dial at all
    /// -- which settles that ticket and releases the claim, while the
    /// tick goes on to start a later candidate successfully. The peer
    /// would then have a dial in flight AND an unclaimed, already-due
    /// entry, so the next tick would dial it again: the duplicate
    /// concurrent retry that claiming exists to prevent, reached
    /// through the one path that settles mid-loop.
    pub fn reclaim_retry(&mut self, peer: &TransportIdentity) {
        if let Some(entry) = self.retries.get_mut(peer) {
            entry.claimed = true;
        }
    }

    /// Give up a claim without touching WHY it was due. Used when
    /// admission itself refused the scheduled dial for a reason that
    /// may already have cleared by the next tick -- a resource ceiling,
    /// a superseded snapshot -- rather than one retrying can never fix.
    ///
    /// `due_at_ms` and `attempts` are left exactly as they were, which
    /// is the same guarantee [`Self::record_failure`]'s sibling
    /// invariant makes for every other origin: a denial must not reset
    /// retry state. The entry is simply eligible for
    /// [`Self::take_due_retries`] again.
    pub fn release_retry_claim(&mut self, peer: &TransportIdentity) {
        if let Some(entry) = self.retries.get_mut(peer) {
            entry.claimed = false;
        }
    }

    /// Peers awaiting a retry.
    #[must_use]
    pub fn scheduled_retries(&self) -> usize {
        self.retries.len()
    }

    /// Whether `peer`'s retry has come due, WITHOUT claiming it.
    ///
    /// Diagnostic only. [`Self::take_due_retries`] is the only method
    /// production code may use to decide what to dial next -- this one
    /// answers "when", not "go", and calling it costs nothing because it
    /// changes nothing.
    #[must_use]
    pub fn is_retry_due(&self, peer: &TransportIdentity, now_ms: u64) -> bool {
        self.retries
            .get(peer)
            .is_some_and(|r| !r.claimed && now_ms >= r.due_at_ms)
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

    /// `claimed` carries a claim forward that this failure did not own;
    /// see [`Self::record_failure`].
    fn schedule_retry(&mut self, peer: TransportIdentity, now_ms: u64, delay: u64, claimed: bool) {
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
                claimed,
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
        request_at(peer_id, address, DialOrigin::ConnectionManager)
    }

    /// A request whose ORIGIN matters, since only the scheduler's own
    /// dials may consume a retry claim.
    fn request_at(peer_id: &str, address: &str, origin: DialOrigin) -> DialRequest {
        DialRequest {
            peer: Some(peer(peer_id)),
            address: address.to_owned(),
            origin,
        }
    }

    #[test]
    fn a_directly_dialled_address_survives_into_its_own_retry() {
        // THE PUBLIC `dial(peer, address)` PATH, which is the caller
        // this function actually has. That API admits an address nobody
        // learned through Identify, so the book can be empty while a
        // retry is being scheduled — and `dial_candidates` reads the
        // book and nothing else. The first tick then found no
        // candidates, cleared the entry (which REMOVES it, not merely
        // its claim), and nothing recreates it: one transient refusal
        // ended reconnection for that peer permanently, and learning the
        // address afterwards could not restart it.
        let mut m = manager(4);
        let p = peer(P1);
        assert_eq!(
            m.known_addresses(&p),
            0,
            "nothing was learned through Identify — this is the whole case"
        );

        let ticket = m
            .handle()
            .admit(&request(P1, "/ip4/10.0.0.1/tcp/4001"), 0)
            .expect("a trusted peer is admitted");
        m.record_failure(ticket, 0);

        assert_eq!(
            m.known_addresses(&p),
            1,
            "the address we just tried is remembered"
        );

        // ...and the scheduled retry can therefore actually dial. The
        // backoff has to have elapsed, which is what the retry delay is.
        let later = 60_000;
        let due = m.take_due_retries(later, 8);
        assert_eq!(due, vec![p.clone()], "the peer is due");
        assert!(
            !m.dial_candidates(&p, later).is_empty(),
            "and the tick has something to dial, so the entry is not cleared"
        );
    }

    #[test]
    fn a_handle_that_outlives_its_manager_admits_nothing() {
        // THE HANDLE IS THE CALLER THIS FUNCTION ACTUALLY HAS. The Swarm
        // task holds a `SnapshotHandle`, not a manager, and a handle is
        // `Clone` and `'static` -- so "the manager was dropped while a
        // handle survived" is not an exotic shape, it is the ordinary
        // shutdown ordering of a task that outlives the thing that
        // spawned it.
        //
        // `is_current` used to ask whether the CELL was still reachable.
        // The handle holds a strong `Arc` to that cell to read it, so it
        // kept its own answer alive: the upgrade succeeded, the revision
        // still matched the final snapshot, and admission carried on
        // indefinitely against authorization nothing could revoke.
        let m = manager(4);
        let handle = m.handle();

        // Admitted while the manager is alive, so the refusal below is
        // the drop and not some unrelated denial.
        assert!(
            handle
                .admit(&request(P1, "/ip4/10.0.0.1/tcp/4001"), 0)
                .is_ok(),
            "a trusted peer is admitted while the manager exists"
        );

        drop(m);

        assert!(
            matches!(
                handle.admit(&request(P1, "/ip4/10.0.0.1/tcp/4001"), 0),
                Err(DialDenial::PolicySuperseded)
            ),
            "with no manager there is nothing to be current against"
        );
        // ...and the same through the documented `load().admit(..)` path,
        // which is public and bypasses the reload loop entirely.
        assert!(
            matches!(
                handle
                    .load()
                    .admit(&request(P1, "/ip4/10.0.0.1/tcp/4001"), 0),
                Err(DialDenial::PolicySuperseded)
            ),
            "a directly loaded snapshot refuses too"
        );
    }

    #[test]
    fn the_liveness_token_is_owned_by_the_manager_alone() {
        // The contract that makes the test above mean anything: if any
        // handle, snapshot, or ticket ever held a STRONG reference to
        // the token, dropping the manager would no longer drop it and
        // the fail-closed answer would quietly stop arriving. Counting
        // is what notices a clone someone adds later, where the
        // behavioural test above would keep passing until the very
        // reference that broke it happened to be the last one.
        let m = manager(4);
        let handle = m.handle();
        let snapshot = handle.load();
        let ticket = handle
            .admit(&request(P1, "/ip4/10.0.0.1/tcp/4001"), 0)
            .expect("trusted");

        assert_eq!(
            Arc::strong_count(&m.alive),
            1,
            "exactly one strong reference, and it is the manager's"
        );

        drop(ticket);
        drop(snapshot);
        drop(handle);
        assert_eq!(
            Arc::strong_count(&m.alive),
            1,
            "and dropping every other holder changed nothing, because \
             none of them held one"
        );
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
    fn a_claimed_retry_is_not_offered_twice() {
        // Consequence 1 of the finding: a slow dial still pending when
        // the next tick fires must not be started again. The old
        // read-only `due_retries` returned the same entry every call;
        // `take_due_retries` must not.
        let mut m = manager(8);
        let t = m
            .handle()
            .load()
            .admit(&request(P1, "/a"), 0)
            .expect("admitted");
        m.record_failure(t, 0);

        let due = 30_000;
        let first = m.take_due_retries(due, 8);
        assert_eq!(first, vec![peer(P1)]);
        let second = m.take_due_retries(due, 8);
        assert!(second.is_empty(), "already claimed; not offered again");
    }

    /// A manual dial failing does not hand back a claim it never took.
    ///
    /// `schedule_retry` rewrote the entry with `claimed: false`, which
    /// is how the SCHEDULER gives its claim back and wrong for every
    /// other origin. With a scheduler dial still in flight, a manual
    /// dial to a second address failing transiently made the entry due
    /// again, and the next tick started the duplicate that claiming
    /// exists to prevent.
    #[test]
    fn a_manual_failure_does_not_release_the_schedulers_claim() {
        let mut m = manager(8);
        let seed = m
            .handle()
            .load()
            .admit(&request(P1, "/a"), 0)
            .expect("admitted");
        m.record_failure(seed, 0);

        // The scheduler takes the claim and its dial is still in flight.
        assert_eq!(m.take_due_retries(30_000, 8), vec![peer(P1)]);

        // A MANUAL dial to a different address fails transiently while
        // that one is pending. Past the backoff so it is admitted on its
        // own merit rather than refused before reaching the code here.
        let manual = m
            .handle()
            .admit(&request_at(P1, "/b", DialOrigin::Manual), 31_000)
            .expect("admitted");
        m.record_failure(manual, 31_000);

        assert!(
            m.take_due_retries(300_000, 8).is_empty(),
            "the scheduler's dial is still in flight: no duplicate"
        );
    }

    /// The other direction: the SCHEDULER's own failure still returns
    /// the claim, or a peer whose dial failed would never be retried.
    #[test]
    fn the_schedulers_own_failure_gives_the_claim_back() {
        let mut m = manager(8);
        let seed = m
            .handle()
            .load()
            .admit(&request(P1, "/a"), 0)
            .expect("admitted");
        m.record_failure(seed, 0);
        assert_eq!(m.take_due_retries(30_000, 8), vec![peer(P1)]);

        let claimed = m
            .handle()
            .load()
            .admit(&request_at(P1, "/a", DialOrigin::ConnectionManager), 30_000)
            .expect("admitted");
        m.record_failure(claimed, 30_000);

        assert_eq!(
            m.take_due_retries(300_000, 8),
            vec![peer(P1)],
            "its own failure releases the claim, so the peer is retried"
        );
    }

    #[test]
    fn a_transient_failure_reschedules_and_keeps_attempts() {
        // The backoff cadence depends on `attempts` surviving the claim.
        // A design that forgot the entry on claim would reset every
        // rescheduled peer to the base delay forever.
        let mut m = manager(8);
        let t = m
            .handle()
            .load()
            .admit(&request(P1, "/a"), 0)
            .expect("admitted");
        m.record_failure(t, 0);
        let claimed = m.take_due_retries(30_000, 8);
        assert_eq!(claimed, vec![peer(P1)]);

        let t2 = m
            .handle()
            .load()
            .admit(&request(P1, "/a"), 30_000)
            .expect("a claimed peer can still be dialed directly");
        m.record_failure(t2, 30_000);
        // Second failure: attempts=2, so the delay is 60s (30s * 2^1),
        // not the base 30s a reset counter would produce.
        assert!(
            !m.is_retry_due(&peer(P1), 30_000 + 30_000),
            "the second failure must not be due after only the base delay"
        );
        assert!(m.is_retry_due(&peer(P1), 30_000 + 60_000));
    }

    #[test]
    fn a_permanent_failure_forgets_the_address_not_the_peers_schedule() {
        // The retry entry is PEER-scoped; a permanent dial failure is
        // ADDRESS-scoped. Removing the peer's whole entry meant a manual
        // dial to one unusable address cancelled the scheduled reconnect
        // that would have tried a good one still in the book.
        let mut m = manager(8);
        let good = "/ip4/10.0.0.1/tcp/1";
        let unusable = "/ip4/10.0.0.2/tcp/1";
        assert!(m.learn_address(&peer(P1), good, 0));
        assert!(m.learn_address(&peer(P1), unusable, 0));

        // A transient failure on the good address schedules a retry.
        let t = m
            .handle()
            .admit(&request_at(P1, good, DialOrigin::Manual), 0)
            .expect("admitted");
        m.record_failure(t, 0);
        assert_eq!(m.scheduled_retries(), 1);

        // A MANUAL dial to the unusable address then fails permanently.
        // It owns no scheduler claim, so it must not touch the schedule.
        let t2 = m
            .handle()
            .admit(&request_at(P1, unusable, DialOrigin::Manual), 30_000)
            .expect("admitted");
        m.record_permanent_failure(t2, 30_000);

        assert_eq!(
            m.scheduled_retries(),
            1,
            "a manual dial's permanent failure must not cancel the peer's reconnect"
        );
        let candidates = m.dial_candidates(&peer(P1), 60_000);
        assert!(
            candidates.iter().any(|a| a == good),
            "the good address survives: {candidates:?}"
        );
        assert!(
            !candidates.iter().any(|a| a == unusable),
            "and the unusable one is forgotten: {candidates:?}"
        );
    }

    #[test]
    fn a_claimed_permanent_failure_releases_rather_than_strands_the_claim() {
        // A scheduled attempt that ends permanently must give the claim
        // back so the peer's OTHER addresses are tried. Removing the
        // entry would abandon them; leaving it claimed would strand it.
        let mut m = manager(8);
        let good = "/ip4/10.0.0.1/tcp/1";
        let unusable = "/ip4/10.0.0.2/tcp/1";
        assert!(m.learn_address(&peer(P1), good, 0));
        assert!(m.learn_address(&peer(P1), unusable, 0));

        let t = m
            .handle()
            .admit(&request_at(P1, good, DialOrigin::Manual), 0)
            .expect("admitted");
        m.record_failure(t, 0);
        assert_eq!(m.take_due_retries(30_000, 8), vec![peer(P1)]);

        let claimed = m
            .handle()
            .admit(
                &request_at(P1, unusable, DialOrigin::ConnectionManager),
                30_000,
            )
            .expect("admitted");
        m.record_permanent_failure(claimed, 30_000);

        assert_eq!(
            m.take_due_retries(30_000, 8),
            vec![peer(P1)],
            "the claim came back, so the good address gets its turn"
        );
    }

    #[test]
    fn a_claimed_attempt_ending_in_a_mismatch_gives_the_claim_back() {
        // Nothing released the claim on this path, so a scheduled retry
        // answered by the wrong key left the entry claimed forever:
        // never selected again, still occupying retry-table capacity,
        // and the peer's good addresses never tried.
        let mut m = manager(8);
        let t = m
            .handle()
            .admit(&request_at(P1, "/a", DialOrigin::Manual), 0)
            .expect("admitted");
        m.record_failure(t, 0);
        assert_eq!(m.take_due_retries(30_000, 8), vec![peer(P1)]);

        let claimed = m
            .handle()
            .admit(&request_at(P1, "/b", DialOrigin::ConnectionManager), 30_000)
            .expect("admitted");
        let _ = m.record_identity_mismatch(claimed, 30_000);

        assert_eq!(
            m.take_due_retries(30_000, 8),
            vec![peer(P1)],
            "an address-scoped quarantine releases the claim rather than stranding it"
        );
    }

    #[test]
    fn a_claimed_attempt_losing_authorization_clears_the_claim() {
        // Unlike a quarantine, this is not a fact about one route:
        // there is nothing for a later tick to try, so the entry goes
        // rather than being released -- but it must not be left claimed
        // either, which is what stranded it.
        let mut m = manager(8);
        let t = m
            .handle()
            .admit(&request_at(P1, "/a", DialOrigin::Manual), 0)
            .expect("admitted");
        m.record_failure(t, 0);
        assert_eq!(m.take_due_retries(30_000, 8), vec![peer(P1)]);

        let claimed = m
            .handle()
            .admit(&request_at(P1, "/a", DialOrigin::ConnectionManager), 30_000)
            .expect("admitted");
        m.record_authorization_withdrawn(claimed, 30_000);

        assert_eq!(m.scheduled_retries(), 0, "the entry is gone, not stranded");
    }

    #[test]
    fn a_later_candidate_starting_takes_the_claim_back() {
        // A scheduled retry with several addresses can have an early
        // one fail SYNCHRONOUSLY -- an address this profile cannot dial
        // at all -- which settles that ticket and releases the claim
        // while the same tick goes on to start a later candidate. The
        // peer would then have a dial in flight AND an unclaimed,
        // already-due entry, so the next tick dials it again: the
        // duplicate concurrent retry claiming exists to prevent,
        // reached through the one path that settles mid-loop.
        let mut m = manager(8);
        let unusable = "/ip4/10.0.0.2/tcp/1";
        let good = "/ip4/10.0.0.1/tcp/1";
        assert!(m.learn_address(&peer(P1), unusable, 0));
        assert!(m.learn_address(&peer(P1), good, 0));

        let t = m
            .handle()
            .admit(&request_at(P1, good, DialOrigin::Manual), 0)
            .expect("admitted");
        m.record_failure(t, 0);
        assert_eq!(m.take_due_retries(30_000, 8), vec![peer(P1)]);

        // The tick's first candidate fails synchronously and releases.
        let first = m
            .handle()
            .admit(
                &request_at(P1, unusable, DialOrigin::ConnectionManager),
                30_000,
            )
            .expect("admitted");
        m.record_permanent_failure(first, 30_000);
        assert_eq!(
            m.take_due_retries(30_000, 8),
            vec![peer(P1)],
            "released, as the mid-loop settle requires"
        );
        // Put it back as the loop found it, then start a later one.
        m.release_retry_claim(&peer(P1));
        let _second = m
            .handle()
            .admit(&request_at(P1, good, DialOrigin::ConnectionManager), 30_000)
            .expect("admitted");
        m.reclaim_retry(&peer(P1));

        assert!(
            m.take_due_retries(30_000, 8).is_empty(),
            "a dial is in flight, so the next tick must not start another"
        );
    }

    #[test]
    fn a_manual_dial_never_consumes_the_schedulers_claim() {
        // THE OWNERSHIP CHECK ITSELF. The three tests above all settle
        // tickets whose origin already matches, so each would pass with
        // `owns_scheduler_claim` hardcoded true -- which is finding B
        // exactly: a manual dial reaching into peer-scoped retry state
        // it does not own.
        //
        // `record_authorization_withdrawn` is the one that CLEARS, so it
        // is where borrowing another origin's claim actually destroys
        // something: a manual dial losing authorization mid-handshake
        // would cancel a reconnect scheduled by the scheduler for a
        // completely different address.
        let mut m = manager(8);
        let t = m
            .handle()
            .admit(&request_at(P1, "/a", DialOrigin::Manual), 0)
            .expect("admitted");
        m.record_failure(t, 0);
        assert_eq!(m.scheduled_retries(), 1, "the scheduler has an entry");

        // PAST THE BACKOFF the failure above imposed, so this dial is
        // admitted on its merits rather than refused before it can
        // reach the code under test.
        let manual = m
            .handle()
            .admit(&request_at(P1, "/b", DialOrigin::Manual), 31_000)
            .expect("admitted");
        m.record_authorization_withdrawn(manual, 31_000);

        assert_eq!(
            m.scheduled_retries(),
            1,
            "a manual dial must not cancel a schedule it does not own"
        );
    }

    #[test]
    fn a_claim_with_nothing_to_dial_is_cleared() {
        // A peer with no candidate address gains nothing from being
        // reconsidered a moment later -- the finding's starvation case.
        let mut m = manager(8);
        let t = m
            .handle()
            .load()
            .admit(&request(P1, "/a"), 0)
            .expect("admitted");
        m.record_failure(t, 0);
        assert_eq!(m.take_due_retries(30_000, 8), vec![peer(P1)]);

        m.clear_retry_claim(&peer(P1));
        assert_eq!(m.scheduled_retries(), 0);
        assert!(m.take_due_retries(60_000, 8).is_empty());
    }

    #[test]
    fn a_claim_denied_by_a_recoverable_reason_is_released_unchanged() {
        // "A denied dial must not reset retry state" -- released, not
        // rescheduled, so the peer is offered again on the very next
        // tick rather than waiting out a fresh backoff it did not earn.
        let mut m = manager(8);
        let t = m
            .handle()
            .load()
            .admit(&request(P1, "/a"), 0)
            .expect("admitted");
        m.record_failure(t, 0);
        assert_eq!(m.take_due_retries(30_000, 8), vec![peer(P1)]);

        m.release_retry_claim(&peer(P1));
        assert_eq!(
            m.take_due_retries(30_000, 8),
            vec![peer(P1)],
            "released at the same due time, reclaimable immediately"
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
        assert!(!m.is_retry_due(&p, 0), "not due yet");

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
        assert!(!m.is_retry_due(&p, 1_000), "and it is still not due");
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
            assert!(m.is_retry_due(&peer(P1), u64::MAX));
            // Advance to exactly when it becomes due and note the gap.
            let mut probe = now;
            while !m.is_retry_due(&peer(P1), probe) {
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
    fn currency_is_read_from_the_same_cell_the_snapshot_came_from() {
        // The atomicity this rests on, asserted as a property rather
        // than by trying to observe a window that no longer exists.
        //
        // The predecessor kept the current revision in a SECOND shared
        // value, written after the new snapshot was installed. Between
        // those two writes an old snapshot's own revision still equalled
        // the not-yet-updated value, so it read as current and decided
        // against policy already superseded. There is now one write and
        // one place to read: whatever the published cell holds IS the
        // answer, so "installed" and "current" cannot disagree because
        // they are the same fact.
        //
        // Observable consequence: for ANY snapshot, currency is exactly
        // "is this the Arc the cell holds", checkable from outside by
        // comparing revisions -- and no sequence of publishes can
        // produce a moment where an older snapshot answers otherwise,
        // because there is no intermediate state to catch it in.
        let mut m = manager(8);
        let handle = m.handle();

        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(handle.load());
            let ticket = m.handle().admit(&request(P1, "/a"), 0).expect("admitted");
            drop(m.record_success(ticket, 0));
        }
        let newest = handle.load();

        // Every snapshot taken before the latest publish refuses, and
        // the one the cell currently holds admits -- with no ordering
        // of the publishes in between able to change either answer.
        for (age, stale) in held.iter().enumerate() {
            assert!(
                stale.revision() < newest.revision(),
                "snapshot {age} should predate the newest"
            );
            assert_eq!(
                stale.admit(&request(P2, "/b"), 0).err(),
                Some(DialDenial::PolicySuperseded),
                "snapshot {age} is not the published one and must not decide"
            );
        }
        drop(
            newest
                .admit(&request(P2, "/b"), 0)
                .expect("the snapshot the cell holds is the one that decides"),
        );
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
        TrustSources::new(
            PeerTrustPolicy::new(data_plane.iter().map(|p| peer(p))).expect("small"),
            InfrastructureSet::new(infrastructure.iter().map(|p| peer(p))).expect("small"),
        )
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
    fn the_local_identity_is_never_classified_as_a_remote_peer() {
        // A configuration that lists this profile's own PeerId is a
        // mistake -- a copied allowlist, a template filled in wrong --
        // and classifying it as an ordinary trusted remote would let
        // self-directed admission, retries and address-book entries all
        // proceed for a peer that cannot be dialed.
        let mut m = untrusting(8);
        m.bind_local_peer(peer(P1));
        // P1 is listed in BOTH sets, as emphatically as a configuration
        // can say it.
        let _ = m.set_trust(trusting(&[P1, P2], &[P1]), &[]);

        assert_eq!(
            m.classify(&peer(P1)),
            ConnectionClass::Unauthorized,
            "the local identity outranks anything the configuration says"
        );
        assert_eq!(
            m.classify(&peer(P2)),
            ConnectionClass::DataPlaneTrusted,
            "and other peers are unaffected"
        );
        assert_eq!(
            m.handle().admit(&request(P1, "/a"), 0).err(),
            Some(DialDenial::Unauthorized),
            "so a self-dial is refused rather than admitted"
        );
    }

    #[test]
    fn a_later_trust_change_cannot_unbind_the_local_identity() {
        // The binding has to survive every update, not merely the
        // first: a caller supplies the two sets and cannot name the
        // local peer, so a subsequent set_trust must not drop it by
        // omission.
        let mut m = untrusting(8);
        m.bind_local_peer(peer(P1));
        let _ = m.set_trust(trusting(&[P1], &[]), &[]);
        assert_eq!(m.classify(&peer(P1)), ConnectionClass::Unauthorized);

        let _ = m.set_trust(trusting(&[P1, P2], &[]), &[]);
        assert_eq!(
            m.classify(&peer(P1)),
            ConnectionClass::Unauthorized,
            "still the local identity after a second trust change"
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

    const A1: &str = "/ip4/192.0.2.1/tcp/4001";
    const A2: &str = "/ip4/192.0.2.2/tcp/4001";

    #[test]
    fn an_unauthorized_peer_gets_no_address_book_entry() {
        // The book is written by whoever sends an Identify message, so
        // its key set has to be bounded by something this profile
        // decided. That something is the trust allowlist: an
        // unclassified peer is not dialable, so remembering where to
        // dial it is storage an unauthorized party would be choosing.
        let mut m = untrusting(8);
        assert!(!m.learn_address(&peer(P1), A1, 0));
        assert_eq!(m.known_addresses(&peer(P1)), 0);

        let _ = m.set_trust(trusting(&[P1], &[]), &[]);
        assert!(m.learn_address(&peer(P1), A1, 0));
        assert_eq!(m.known_addresses(&peer(P1)), 1);
    }

    #[test]
    fn the_address_book_is_bounded_per_peer() {
        let mut m = manager(8);
        for i in 0..DEFAULT_MAX_ADDRESSES_PER_PEER {
            assert!(m.learn_address(&peer(P1), &format!("/ip4/198.51.100.{i}/tcp/1"), 0));
        }
        assert_eq!(m.known_addresses(&peer(P1)), DEFAULT_MAX_ADDRESSES_PER_PEER);
        assert!(
            !m.learn_address(&peer(P1), A2, 0),
            "a full book of dialable addresses refuses rather than displacing one"
        );
        assert_eq!(m.known_addresses(&peer(P1)), DEFAULT_MAX_ADDRESSES_PER_PEER);
    }

    #[test]
    fn a_quarantined_address_makes_way_and_a_working_one_does_not() {
        // The peer writes this list. If a new assertion could displace
        // any entry, a peer that had one known-good route and then
        // claimed eight new addresses would have flushed the route that
        // works -- which is the poisoning attack one layer up, done
        // through the book instead of through backoff.
        let mut m = manager(8);
        for i in 0..DEFAULT_MAX_ADDRESSES_PER_PEER {
            assert!(m.learn_address(&peer(P1), &format!("/ip4/198.51.100.{i}/tcp/1"), 0));
        }
        // One of them turns out to be serving somebody else.
        let ticket = m
            .handle()
            .admit(&request(P1, "/ip4/198.51.100.3/tcp/1"), 0)
            .expect("admitted");
        assert!(m.record_identity_mismatch(ticket, 0));

        assert!(
            m.learn_address(&peer(P1), A2, 0),
            "the quarantined entry is the one that makes way"
        );
        let candidates = m.dial_candidates(&peer(P1), 0);
        assert!(
            !candidates.iter().any(|a| a == "/ip4/198.51.100.3/tcp/1"),
            "and the quarantined address is not offered while it is quarantined"
        );
        assert!(candidates.iter().any(|a| a == A2));
    }

    #[test]
    fn a_known_good_address_is_offered_first() {
        // `preferred_addresses` has existed since Stage 2 and was
        // called by nobody, so a peer with a working route and a failing
        // one was dialed at whichever address the caller happened to
        // hold.
        let mut m = manager(8);
        assert!(m.learn_address(&peer(P1), A1, 0));
        assert!(m.learn_address(&peer(P1), A2, 0));

        // A2 works; A1 does not.
        let good = m.handle().admit(&request(P1, A2), 0).expect("admitted");
        drop(m.record_success(good, 0));
        let bad = m.handle().admit(&request(P1, A1), 1_000).expect("admitted");
        m.record_failure(bad, 1_000);

        assert_eq!(
            m.dial_candidates(&peer(P1), 2_000)
                .first()
                .map(String::as_str),
            Some(A2),
            "the route that authenticated comes first"
        );
    }

    #[test]
    fn withdrawn_authorization_settles_without_scheduling_anything() {
        // The window this closes: admission photographed trust at ONE
        // instant, and the handshake can outlast it. Recording the
        // outcome as an ordinary success would retain a connection
        // under authority that no longer exists; recording it as an
        // ordinary failure would schedule a retry for a peer this
        // profile no longer trusts, which is the same mistake reached
        // from the other direction.
        let mut m = manager(8);
        let t = m
            .handle()
            .load()
            .admit(&request(P1, "/a"), 0)
            .expect("admitted");
        assert_eq!(m.connections(), 1, "the connection slot was reserved");

        m.record_authorization_withdrawn(t, 0);
        assert_eq!(m.connections(), 0, "settled, the slot came back");
        assert_eq!(
            m.scheduled_retries(),
            0,
            "authorization withdrawn is not a failure the peer earned a retry for"
        );
    }

    #[test]
    fn an_infrastructure_peer_keeps_a_reachability_connection_and_loses_a_data_plane_one() {
        // ADR-0036's separation is an origin/class PAIR, and
        // `ConnectionPolicy::admit` has always decided it that way --
        // `tests/transport-contract/tests/stage2_exit_gate.rs` pins
        // every combination. Revalidating an established connection
        // with the data-plane-only predicate ignored the pair, so a
        // relay reservation, relay circuit, AutoNAT probe or DCUtR hole
        // punch to an infrastructure peer completed its handshake and
        // was closed immediately -- admission permitted it and
        // establishment threw it away.
        let mut m = untrusting(8);
        let _ = m.set_trust(trusting(&[], &[P1]), &[]);
        let class = m.classify(&peer(P1));
        assert_eq!(class, ConnectionClass::ConnectivityInfrastructureOnly);

        for origin in DialOrigin::ALL {
            let kept = m.authorizes_for(class, origin);
            assert_eq!(
                kept,
                !origin.is_data_plane(),
                "{origin:?} on an infrastructure peer: kept must match what admission permits"
            );
        }

        // A drain still refuses everything, whatever the origin.
        m.begin_shutdown();
        for origin in DialOrigin::ALL {
            assert!(
                !m.authorizes_for(class, origin),
                "{origin:?} must not survive a drain"
            );
        }
    }

    #[test]
    fn a_connection_is_judged_by_the_same_authorization_whichever_way_it_started() {
        // ADR-0011: current authorization applies before a connection
        // is RETAINED, inbound or outbound. Arriving is not an
        // authorization, and an infrastructure peer authorized for
        // reachability is not thereby a data-plane peer.
        let mut m = manager(8);
        assert!(m.authorizes(ConnectionClass::DataPlaneTrusted));
        assert!(!m.authorizes(ConnectionClass::ConnectivityInfrastructureOnly));
        assert!(!m.authorizes(ConnectionClass::Unauthorized));

        m.begin_shutdown();
        assert!(
            !m.authorizes(ConnectionClass::DataPlaneTrusted),
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
            TrustSources::new(
                PeerTrustPolicy::new(generated.iter().map(|p| peer(p))).expect("small"),
                InfrastructureSet::default(),
            ),
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
