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
    /// Live count of admitted dials whose OUTCOME may still need an
    /// address entry, SHARED with the manager (review R4 on fa3eab8).
    ///
    /// A resource, like `pending`: admission checked that an identity
    /// mismatch could be recorded but reserved nothing, so two dials
    /// admitted against one free entry both counted on it, and the
    /// second mismatch found the table full of live quarantines and was
    /// not recorded -- the address free to be dialled again as soon as
    /// an unrelated quarantine expired. Each admitted dial that names a
    /// peer now reserves one unit against `max_addresses` minus the live
    /// quarantines, and holds it until settled: a mismatch turns at most
    /// its own unit into a quarantine, so every one of them finds room
    /// without evicting another.
    outcomes: Arc<AtomicUsize>,
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
            self.revision
                == cell
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .revision
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

    /// Whether this address is dialable for this peer right now.
    ///
    /// CAPACITY-FREE AND TICKET-FREE, which is the point. The
    /// established hook must judge the address a behaviour dial
    /// actually used, and its only prior instrument was a probe through
    /// [`Self::admit`] — which decides policy AND takes a reservation,
    /// so at a full ceiling the probe was refused for capacity the very
    /// connection being judged was occupying, and the caller had to
    /// discard capacity denials by enumerating them (SPIKE-003 F11).
    /// This reads the address quarantine and nothing else; it cannot
    /// see capacity, so there is nothing to discard.
    ///
    /// A read of the PHOTOGRAPHED policy, like [`Self::classify`]: one
    /// publication stale is permitted for a policy answer.
    #[must_use]
    pub fn address_dialable(&self, peer: &TransportIdentity, address: &str, now_ms: u64) -> bool {
        self.policy.is_address_dialable(peer, address, now_ms)
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
            outcomes: Arc::clone(&self.outcomes),
            outcome_reserved: false,
            peer: request.peer.clone(),
            address: request.address.clone(),
            origin: request.origin,
            settled: false,
            connection_kept: false,
            admitted_at_ms: now_ms,
        };
        if reserve(&self.connections, self.max_connections).is_err() {
            // `ticket` releases the pending slot as it drops here, and
            // holds no connection slot to release.
            let mut ticket = ticket;
            ticket.connection_kept = true;
            return Err(DialDenial::ConnectionLimitReached);
        }

        // THE ROOM ITS OUTCOME MAY NEED, reserved now (review R4 on
        // fa3eab8). Against the entries a quarantine can still take: the
        // table's size less the LIVE quarantines outside the book, which
        // no outcome may evict; one on a book entry takes no table slot
        // (`a_book_quarantine_takes_no_admission_room`). A dial that
        // names no peer records nothing.
        let mut ticket = ticket;
        if ticket.peer.is_some() {
            let room = self
                .policy
                .max_addresses
                .saturating_sub(self.policy.live_quarantines(now_ms));
            if reserve(&self.outcomes, room).is_err() {
                // Releases the pending and connection slots as it drops.
                drop(ticket);
                return Err(DialDenial::PolicyStateFull);
            }
            ticket.outcome_reserved = true;
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

// A seam at the instant a snapshot is installed: whatever outcome unit
// a settlement or a hand-over holds must still be held here, since a
// holder of the snapshot being replaced admits against it until the
// write lands (`an_outcome_unit_is_held_until_its_snapshot_is_installed`).
// Compiled out of every non-test build.
#[cfg(test)]
thread_local! {
    static BEFORE_INSTALL: std::cell::RefCell<Option<Box<dyn Fn()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn before_install() {
    let hook = BEFORE_INSTALL.with(|h| h.borrow_mut().take());
    if let Some(f) = hook {
        f();
    }
}

#[cfg(not(test))]
const fn before_install() {}

/// Why an authenticated connection is not retained, beyond its
/// authorization ([`ConnectionManager::admits_retention`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionRefusal {
    /// `max_connections_per_peer` connections to this peer are held.
    PerPeerLimitReached,
    /// `max_connected_peers` distinct peers are held, and this is another.
    ConnectedPeerLimitReached,
}

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
    /// The outcome reservation (`PolicySnapshot::outcomes`), held while
    /// `outcome_reserved`.
    outcomes: Arc<AtomicUsize>,
    outcome_reserved: bool,
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
    /// The time admission judged this dial at: the live quarantines its
    /// outcome's reserved room was counted against.
    admitted_at_ms: u64,
}

impl DialTicket {
    /// The time an outcome of this dial is judged at: never earlier than
    /// its admission, since the room admission reserved was counted
    /// against the quarantines live THEN, and one that lapsed in between
    /// would read as live again to an earlier clock and leave the
    /// outcome no room. Used by the four settlements whose time decides
    /// room or a hand-over: a mismatch
    /// (`an_outcome_settled_before_its_admission_time_is_still_recorded`),
    /// a failure (`a_failure_settled_before_its_admission_time_is_still_recorded`),
    /// a success (`a_success_settled_before_its_admission_time_is_still_recorded`)
    /// and a permanent failure
    /// (`a_permanent_failure_settled_before_its_admission_time_still_removes_the_route`);
    /// a withdrawn or locally refused dial records nothing time-bound.
    const fn settled_at(&self, now_ms: u64) -> u64 {
        if now_ms > self.admitted_at_ms {
            now_ms
        } else {
            self.admitted_at_ms
        }
    }

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

    /// Re-bind this permission to the address the dial actually used.
    ///
    /// The behaviour-dial escape hatch, and nothing else: a
    /// behaviour-originated dial is admitted with an EMPTY placeholder
    /// address, because at admission libp2p has not chosen one — the
    /// pending hook receives no addresses (SPIKE-003 F9) — and settling
    /// the placeholder would record every such route against one empty
    /// address-policy entry (F12). This moves the ticket onto the real
    /// address at the first moment it exists.
    ///
    /// Returns `false` — and changes nothing — unless the ticket holds
    /// the placeholder and the replacement is non-empty: an ordinary
    /// ticket's address was DECIDED at admission, and a caller that
    /// could move it afterwards would settle a route the gate never
    /// admitted. `a_placeholder_rebinds_exactly_once_and_a_bound_address_never`
    /// holds that shut.
    pub fn rebind_address(&mut self, address: &str) -> bool {
        if !self.address.is_empty() || address.is_empty() {
            return false;
        }
        address.clone_into(&mut self.address);
        true
    }
}

impl Drop for DialTicket {
    fn drop(&mut self) {
        if self.outcome_reserved {
            // Dropped unsettled: nothing was recorded, so the room it was
            // holding goes back now. A SETTLED ticket's unit is released
            // by the manager after it publishes (`ConnectionManager::settle`).
            self.outcomes.fetch_sub(1, Ordering::AcqRel);
        }
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
        Arc::clone(
            &self
                .current
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
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
#[derive(Debug, Clone, PartialEq, Eq)]
struct Retry {
    due_at_ms: u64,
    attempts: u32,
    /// A scheduler tick has claimed this peer and an attempt is under
    /// way that has not yet been settled. A claimed entry is never
    /// returned by [`ConnectionManager::take_due_retries`] again, which
    /// is what stops the same slow dial from being started twice.
    claimed: bool,
    /// The relay whose hop the last circuit dial never reached
    /// ([`ConnectionManager::record_relay_hop_unreached`]): the retry is
    /// also due the moment a direct connection to it establishes
    /// ([`ConnectionManager::relay_reached`]), rather than only at its
    /// delay.
    waits_on: Option<TransportIdentity>,
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

/// The most addresses per peer a profile may configure
/// (`transport.limits.max_addresses_per_peer`, `integer[1..32]`).
pub const MAX_ADDRESSES_PER_PEER: usize = 32;

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

/// Book peers the trust no longer classifies that keep their addresses,
/// at most (#117's blind review F3): enough that an operator removing
/// and re-adding trust entries does not cost those peers their routes,
/// bounded so a rotating allowlist cannot grow the book with trust churn.
pub const MAX_RETIRED_BOOK_PEERS: usize = 256;

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
    /// Outcome reservations (`PolicySnapshot::outcomes`), shared.
    outcomes: Arc<AtomicUsize>,
    /// Units of settled tickets not yet returned to `outcomes`.
    ///
    /// Returned in [`Self::publish`], AFTER the snapshot carrying the
    /// settlement's quarantine is installed. Returned at settlement, a
    /// holder of the previous snapshot -- still current until the
    /// publication -- would see the unit free beside a quarantine
    /// count that does not yet include it, and admit one dial too many.
    outcomes_to_return: usize,
    connections: Arc<AtomicUsize>,
    max_connections: usize,
    shutting_down: Arc<AtomicBool>,
    /// This profile's own identity, once the runtime has said what it
    /// is. `None` until then, which is the honest answer for a manager
    /// constructed by a test that never had one.
    local_peer: Option<TransportIdentity>,
    retries: std::collections::BTreeMap<TransportIdentity, Retry>,
    /// Book peers the current trust no longer classifies, longest-revoked
    /// first, at most [`MAX_RETIRED_BOOK_PEERS`]: their entries are kept
    /// so a trust flap does not cost a peer its routes (`set_trust`).
    retired: std::collections::VecDeque<TransportIdentity>,
    /// The retry and quarantine decisions not yet handed up, oldest
    /// first, at most [`MAX_GATE_NOTES`] (`drain_notes`).
    notes: std::collections::VecDeque<GateNote>,
    /// Notes lost to that bound since the manager was built.
    notes_dropped: u64,
    /// The latest `now_ms` any call has handed the manager, for the two
    /// paths that take a book entry out with no clock of their own
    /// (`retire_unclassified_book_peers`,
    /// `record_permanent_address_failure_unadmitted`). It can only lag
    /// the real time, and a lagging clock sees a quarantine as live for
    /// longer -- keeping an entry, never laundering one.
    clock_ms: u64,
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
        let outcomes = Arc::new(AtomicUsize::new(0));
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
                outcomes: Arc::clone(&outcomes),
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
            outcomes,
            outcomes_to_return: 0,
            connections,
            max_connections,
            shutting_down,
            local_peer: None,
            retries: std::collections::BTreeMap::new(),
            retired: std::collections::VecDeque::new(),
            notes: std::collections::VecDeque::new(),
            notes_dropped: 0,
            clock_ms: 0,
            max_addresses_per_peer: DEFAULT_MAX_ADDRESSES_PER_PEER,
            max_retry_entries: DEFAULT_MAX_RETRY_ENTRIES,
            published,
            alive,
        }
    }

    /// Remember at most `limit` addresses per peer: the profile's
    /// `transport.limits.max_addresses_per_peer` (architect-cto's ruling
    /// on #145), clamped to `1..=`[`MAX_ADDRESSES_PER_PEER`] so no caller
    /// can unbound the peer-written list. A lower limit takes effect at
    /// the peer's next learned address.
    pub fn set_max_addresses_per_peer(&mut self, limit: usize) {
        self.max_addresses_per_peer = limit.clamp(1, MAX_ADDRESSES_PER_PEER);
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

    /// The live address and peer policy, READ-ONLY: what a status
    /// surface reads the bounded tables' sizes from (plan §15's dial-gate
    /// introspection). A shared borrow, so a status reader cannot write
    /// a quarantine or a backoff through it.
    #[must_use]
    pub const fn policy(&self) -> &ConnectionPolicy {
        &self.policy
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
            outcomes: Arc::clone(&self.outcomes),
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
        before_install();
        *self
            .published
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = next;
        // NOW the settled tickets' outcome units go back: the snapshot
        // just installed already counts whatever quarantine they became.
        if self.outcomes_to_return > 0 {
            self.outcomes
                .fetch_sub(self.outcomes_to_return, Ordering::AcqRel);
            self.outcomes_to_return = 0;
        }
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
        trust.local_peer.clone_from(&self.local_peer);
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
        trust.local_peer.clone_from(&self.local_peer);
        self.trust = Arc::new(trust);
        // THE BOOK FOLLOWS THE TRUST, WITH A MEMORY (review R3 on
        // fa3eab8, and #117's blind review F3 against the first fix). It
        // admits addresses only for a peer the trust classifies, and
        // nothing removed a peer's entry when it stopped being
        // classified, so a rotating allowlist of one peer left the book
        // holding every peer ever trusted. Dropping every unclassified
        // peer at once fixed the bound and broke a trust FLAP: a peer
        // removed and re-added before its retry lost its configured and
        // learned routes, and nothing re-learns them for a peer that is
        // not connected. So the most recently revoked peers keep their
        // entries, in revocation order, up to `MAX_RETIRED_BOOK_PEERS`;
        // re-authorizing one restores it untouched, and past the bound
        // the longest-revoked goes. The book's keys are at most the
        // peers the current trust classifies plus that bound, plus the
        // retired peers an entry of which held, at the last pass, a live
        // quarantine that could not be handed over (`hand_over`: no table
        // slot, or no free outcome unit). Such a peer stays until a later
        // pass releases it, even once the quarantine has lapsed
        // (`retire_unclassified_book_peers`).
        // A quarantine leaves the book only into a table that can take
        // it (`release_from_book`), so nothing a dial is suppressed by is
        // forgotten either way.
        self.retire_unclassified_book_peers();
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

    /// Take `address` out of `peer`'s book: the one way out, refused for
    /// a live quarantine that cannot be handed over -- no free outcome
    /// unit here, or no table slot in `ConnectionPolicy::release_from_book`.
    ///
    /// A LIVE QUARANTINE THAT LEAVES also takes one unit of the outcome
    /// reservation's room, since a book record counts against neither
    /// the table nor that room and a record outside the book counts
    /// against both. So it leaves only if an outcome unit is free,
    /// holds it until the snapshot counting it is published -- the
    /// settlement pattern, so no holder of the older snapshot admits
    /// against room the move has taken
    /// (`an_outcome_unit_is_held_until_its_snapshot_is_installed`) --
    /// and is refused otherwise
    /// (`a_quarantine_leaves_the_book_only_into_unreserved_room`).
    fn hand_over(&mut self, peer: &TransportIdentity, address: &str, now_ms: u64) -> bool {
        let live = self
            .policy
            .book
            .get(peer)
            .is_some_and(|k| k.contains(address))
            && self
                .policy
                .address(peer, address)
                .is_some_and(|s| s.is_punitive_at(now_ms));
        if !live {
            return self.policy.release_from_book(peer, address, now_ms);
        }
        let room = self
            .policy
            .max_addresses
            .saturating_sub(self.policy.live_quarantines(now_ms));
        if reserve(&self.outcomes, room).is_err() {
            return false;
        }
        if !self.policy.release_from_book(peer, address, now_ms) {
            self.outcomes.fetch_sub(1, Ordering::AcqRel);
            return false;
        }
        self.outcomes_to_return += 1;
        self.publish();
        true
    }

    fn observe(&mut self, now_ms: u64) {
        self.clock_ms = self.clock_ms.max(now_ms);
    }

    /// Bring the book's retired peers in line with the current trust
    /// (`set_trust`): newly unclassified peers join the back of
    /// `retired`, re-classified ones leave it, and past
    /// [`MAX_RETIRED_BOOK_PEERS`] the longest-retired lose their entry.
    fn retire_unclassified_book_peers(&mut self) {
        let current = Arc::clone(&self.trust);
        let unclassified = |peer: &TransportIdentity| {
            matches!(current.classify(peer), ConnectionClass::Unauthorized)
        };
        self.retired.retain(|peer| unclassified(peer));
        let newly: Vec<TransportIdentity> = self
            .policy
            .book
            .keys()
            .filter(|peer| unclassified(peer) && !self.retired.contains(peer))
            .cloned()
            .collect();
        self.retired.extend(newly);
        // A peer with an entry that stays -- a live quarantine that
        // cannot be handed over (ADR-0011, amendment 2026-09-28) -- keeps its
        // place at the front, so the next pass tries it again first, and
        // does not count against the bound, so no other retired peer
        // loses its routes early
        // (`a_retirement_pass_leaves_a_quarantine_the_table_cannot_take`).
        let mut stuck = std::collections::VecDeque::new();
        while self.retired.len() > MAX_RETIRED_BOOK_PEERS {
            if let Some(oldest) = self.retired.pop_front() {
                let addresses: Vec<String> = self
                    .policy
                    .book
                    .get(&oldest)
                    .map(|known| known.iter().cloned().collect())
                    .unwrap_or_default();
                for address in addresses {
                    let _ = self.hand_over(&oldest, &address, self.clock_ms);
                }
                if self.policy.book.contains_key(&oldest) {
                    stuck.push_back(oldest);
                }
            }
        }
        while let Some(peer) = stuck.pop_back() {
            self.retired.push_front(peer);
        }
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
    /// When the per-peer list is full, a quarantined address makes way
    /// for the new one; else a never-successful address that has failed,
    /// the most-failed first
    /// (`a_full_book_gives_up_its_most_failed_never_working_entry`,
    /// `a_failing_never_working_entry_goes_before_a_proven_one`); else one
    /// that succeeded and has failed since, the oldest success first
    /// (`among_routes_that_stopped_answering_the_oldest_proof_goes_first`).
    /// An address with no failure -- recently good or not yet tried -- is
    /// never displaced, nor is the most recently proven route, so a peer
    /// cannot flush the route that works by asserting new ones
    /// (`a_quarantined_address_makes_way_and_a_working_one_does_not`,
    /// `the_address_book_is_bounded_per_peer`,
    /// `a_full_book_never_gives_up_the_peers_last_proven_route`,
    /// `a_full_book_of_recently_good_routes_refuses_the_newcomer`). The
    /// entry's state is the book's while it is held -- never pruned apart
    /// from it (`a_book_entrys_state_is_never_pruned_apart_from_it`) -- and
    /// a displaced address's state returns to the policy table, which
    /// keeps a live quarantine until it lapses and a failure-only record
    /// only as it keeps any address outside the book. When a live
    /// quarantine cannot be handed over (`hand_over`) the entry stays and
    /// the newcomer is refused
    /// (`a_full_table_keeps_a_quarantined_entry_in_the_book`,
    /// `a_quarantine_leaves_the_book_only_into_unreserved_room`), so
    /// eviction launders nothing; a re-learned address may come back
    /// untried if its failure-only record was pruned meanwhile.
    pub fn learn_address(&mut self, peer: &TransportIdentity, address: &str, now_ms: u64) -> bool {
        self.observe(now_ms);
        if matches!(self.classify(peer), ConnectionClass::Unauthorized) {
            return false;
        }
        let max = self.max_addresses_per_peer;
        let known = self.policy.book.get(peer);
        if known.is_some_and(|k| k.contains(address)) {
            return true;
        }
        // THE BOOK REMEMBERS THE PROOF (ADR-0011, amendment 2026-09-28):
        // each entry's success, failure and quarantine state is its own,
        // never pruned apart from it. A full book gives up, in order: a
        // quarantined entry; a never-successful entry that HAS FAILED, the
        // most-failed first; an entry that succeeded and has failed since,
        // the oldest success first. It never gives up an entry with no
        // failure -- recently good or not yet tried, so a stream of
        // asserted addresses cannot churn out a peer's untried routes --
        // nor the most recently proven route, which is the one that works
        // (#137).
        let evictable = match known {
            Some(k) if k.len() >= max => {
                let policy = &self.policy;
                let state = |a: &String| policy.address(peer, a);
                let last_success = |a: &String| state(a).and_then(|s| s.last_success_ms);
                let newest_proven = k
                    .iter()
                    .filter(|a| last_success(a).is_some())
                    .max_by_key(|a| (last_success(a), std::cmp::Reverse((*a).clone())))
                    .cloned();
                let victim = k
                    .iter()
                    .filter(|a| {
                        !policy.is_address_dialable(peer, a, now_ms)
                            || (state(a).is_some_and(|s| s.consecutive_failures > 0)
                                && newest_proven.as_ref() != Some(*a))
                    })
                    // `Reverse(None)` sorts above every `Reverse(Some)`,
                    // so a never-successful entry comes before a proven
                    // one, and among proven ones the oldest success first.
                    .max_by_key(|a| {
                        (
                            !policy.is_address_dialable(peer, a, now_ms),
                            std::cmp::Reverse(last_success(a)),
                            state(a).map_or(0, |s| s.consecutive_failures),
                        )
                    })
                    .cloned();
                match victim {
                    Some(victim) => Some(victim),
                    None => return false,
                }
            }
            _ => None,
        };
        if let Some(stale) = evictable {
            // A quarantine that cannot be handed over keeps its entry,
            // and the newcomer is refused.
            if !self.hand_over(peer, &stale, now_ms) {
                return false;
            }
        }
        // UNTRIED: no record of its own, in the book or the policy table
        // -- asked before it goes in.
        let untried = self.policy.address(peer, address).is_none();
        self.policy
            .book
            .entry(peer.clone())
            .or_default()
            .insert(address.to_owned());
        // A NEW ROUTE IS NOT WAITING ON A FAILURE IT DID NOT EARN (ADR-0011
        // §Address-scoped failure, A 2026-10-01; architect-cto's ruling,
        // relay seq 9992). The dial-failure retry is keyed per peer, so a
        // fresh address learned while the peer waits on a stale one's
        // failure inherited that wait -- RETRY_BASE_MS and up. An untried
        // address makes the retry due now, once: the dial gives it a
        // record, so learning it again moves nothing. Only the due time
        // moves; the attempt count carries, and the next failure sets the
        // due time anew. Admission's own lift is the policy's, for a
        // non-empty address only.
        if untried
            && let Some(retry) = self.retries.get_mut(peer)
            && !retry.claimed
        {
            retry.due_at_ms = retry.due_at_ms.min(now_ms);
        }
        true
    }

    /// Addresses to try for `peer`, recently good first.
    ///
    /// The order [`ConnectionPolicy::preferred_addresses`] computes,
    /// which until now nothing asked for: a peer with a working route
    /// and a quarantined one was dialed at whichever address the caller
    /// happened to hold.
    #[must_use]
    pub fn dial_candidates(&self, peer: &TransportIdentity, now_ms: u64) -> Vec<String> {
        let known: Vec<String> = self
            .policy
            .book
            .get(peer)
            .map(|a| a.iter().cloned().collect())
            .unwrap_or_default();
        self.policy.preferred_addresses(peer, &known, now_ms)
    }

    /// How many addresses are remembered for `peer`.
    #[must_use]
    pub fn known_addresses(&self, peer: &TransportIdentity) -> usize {
        self.policy
            .book
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
        let now_ms = ticket.settled_at(now_ms);
        self.observe(now_ms);
        if !self.issued_here(&ticket) {
            return self.keep_connection(ticket);
        }
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
    ///
    /// Returns the retry it scheduled, for the caller to report, or `None`
    /// when it scheduled none: a ticket not issued here, a placeholder,
    /// or a hole-punch dial.
    pub fn record_failure(&mut self, ticket: DialTicket, now_ms: u64) -> Option<RetryScheduled> {
        let now_ms = ticket.settled_at(now_ms);
        self.observe(now_ms);
        if !self.issued_here(&ticket) {
            return None;
        }
        // A PLACEHOLDER NAMES NO ROUTE. A behaviour dial is admitted
        // with an empty address (F9) and rebound to the real one at the
        // established hook or from the failure's own address list; a
        // ticket still empty here failed before any address was known.
        // There is nothing to score, nothing worth learning — an empty
        // string in the address book becomes a dial candidate — and
        // nothing a retry could dial, so it settles and does no more.
        if ticket.address().is_empty() {
            self.settle(ticket);
            self.publish();
            return None;
        }
        // A HOLE-PUNCH DIAL IS NOT A ROUTE. Its address is a candidate
        // the far end named for THIS attempt -- a NAT mapping, a port
        // the far end's Identify observed -- and the peer it names is
        // connected by construction, over the relayed connection the
        // attempt runs on. Scoring it would put a connected peer in
        // backoff and refuse the dials the attempt has left (and, after
        // a one-sided punch, the whole peer for thirty seconds);
        // learning it would put a guess in the book; scheduling a
        // retry would have the scheduler redial a peer it is connected
        // to, directly after a punch the other end's dial completed.
        // What a failed punch costs is the ATTEMPT lifecycle's to
        // decide -- the cooldown in the DCUtR adapter -- not this
        // table's. PR #102 round 1.
        // `a_hole_punch_dials_failure_settles_and_scores_schedules_and_learns_nothing`
        // pins it.
        if ticket.origin() == DialOrigin::DcutrHolePunch {
            self.settle(ticket);
            self.publish();
            return None;
        }
        let mut scheduled = None;
        if let Some(peer) = ticket.peer().cloned() {
            // ONE delay, used for both. The address-scoped backoff and
            // the reconnect schedule disagreeing would mean the manager
            // retries a peer at a moment its own policy still refuses,
            // producing a denial the retry counter then treats as
            // another failure -- a peer talking itself into permanent
            // backoff without the remote end doing anything.
            let delay = self.retry_delay_ms(&peer);
            let peer_backoff =
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
            // the list at `max_addresses_per_peer`, and evicts only a
            // quarantined or a failing address, never the most recently
            // proven route (its own doc says which first). An address
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
            let attempt = self.schedule_retry(peer.clone(), now_ms, delay, held_by_another);
            let retry = RetryScheduled {
                attempt,
                delay_ms: delay,
                peer_backoff,
            };
            self.note(GateNote::RetryScheduled {
                peer,
                origin: ticket.origin(),
                retry,
            });
            scheduled = Some(retry);
        }
        self.settle(ticket);
        self.publish();
        scheduled
    }

    /// Settle a circuit dial that never reached its RELAY, scoring
    /// nothing against the destination.
    ///
    /// A `/p2p-circuit` dial goes through the relay first, and when this
    /// node holds no connection to the relay the relay client opens one
    /// and parks the circuit on it. If that hop is refused or fails --
    /// at start-up the client's own reservation dial to the same relay
    /// is still in flight, and the hop's dial is refused on the Swarm's
    /// peer condition -- the parked circuit is dropped and the transport
    /// reports the CIRCUIT as failed. Nothing about the destination was
    /// learnt: it was never asked. Scored by [`Self::record_failure`],
    /// the circuit address was the peer's only route, so the peer went
    /// into punitive backoff and every `DialPeer` for the next thirty
    /// seconds was refused, while the relay connected milliseconds later
    /// (rust-ui-dev-01's measurement on j6, 2026-10-09).
    ///
    /// So the address and the peer are not scored, the route is kept
    /// (learnt, as [`Self::record_failure`] learns an attempted address),
    /// and the reconnect is scheduled at the peer's ordinary delay AND
    /// marked as waiting on `relay`: the first direct connection to the
    /// relay makes it due at once ([`Self::relay_reached`]). A relay
    /// that stays down therefore costs the ordinary cadence, never a
    /// tight loop. `None` when the ticket was not issued here.
    pub fn record_relay_hop_unreached(
        &mut self,
        ticket: DialTicket,
        relay: &TransportIdentity,
        now_ms: u64,
    ) -> Option<RetryScheduled> {
        let now_ms = ticket.settled_at(now_ms);
        self.observe(now_ms);
        if !self.issued_here(&ticket) {
            return None;
        }
        let mut scheduled = None;
        if let Some(peer) = ticket.peer().cloned() {
            if !ticket.address().is_empty() {
                self.learn_address(&peer, ticket.address(), now_ms);
            }
            let delay = self.retry_delay_ms(&peer);
            let held_by_another = !ticket.owns_scheduler_claim()
                && self.retries.get(&peer).is_some_and(|entry| entry.claimed);
            let attempt = self.schedule_retry(peer.clone(), now_ms, delay, held_by_another);
            if let Some(entry) = self.retries.get_mut(&peer) {
                entry.waits_on = Some(relay.clone());
            }
            let retry = RetryScheduled {
                attempt,
                delay_ms: delay,
                peer_backoff: false,
            };
            self.note(GateNote::RetryScheduled {
                peer,
                origin: ticket.origin(),
                retry,
            });
            scheduled = Some(retry);
        }
        self.settle(ticket);
        self.publish();
        scheduled
    }

    /// A direct connection to `relay` is established: every retry
    /// waiting on it ([`Self::record_relay_hop_unreached`]) is due now.
    /// A claimed entry is left alone -- its attempt is under way.
    pub fn relay_reached(&mut self, relay: &TransportIdentity, now_ms: u64) {
        self.observe(now_ms);
        for entry in self.retries.values_mut() {
            if !entry.claimed && entry.waits_on.as_ref() == Some(relay) {
                entry.due_at_ms = entry.due_at_ms.min(now_ms);
                entry.waits_on = None;
            }
        }
    }

    /// Score an address-scoped failure with no ticket and no admission.
    ///
    /// SPIKE-003 F15: settling a multi-address `DialError::Transport`
    /// needs one score per exhausted address, and the old route to a
    /// score — mint a ticket through `admit` — required passing the very
    /// policy the failure had just changed AND a spare slot per address.
    /// Sequential settlement hit the peer backoff the first score
    /// advanced; batched settlement hit the ceiling; either way the
    /// remaining routes stayed unscored and immediately retryable. This
    /// is the address-scoped failure API that needs no admission: it
    /// scores the route, learns it (an attempted address is a candidate,
    /// exactly as [`Self::record_failure`] argues), and reserves
    /// nothing.
    ///
    /// It does NOT touch the retry schedule: the dial's primary ticket
    /// settlement owns that, and a second reschedule for the same dial
    /// would double-count one failure.
    pub fn record_address_failure_unadmitted(
        &mut self,
        peer: &TransportIdentity,
        address: &str,
        now_ms: u64,
    ) {
        if address.is_empty() {
            return;
        }
        let delay = self.retry_delay_ms(peer);
        let _ = self
            .policy
            .record_address_failure(peer, address, now_ms, delay);
        let _ = self.learn_address(peer, address, now_ms);
        self.publish();
    }

    /// Forget a route that retrying cannot fix, with no ticket at all.
    ///
    /// The admission-free counterpart of [`Self::record_permanent_failure`],
    /// for the EXTRA addresses of a multi-address `DialError::Transport`
    /// whose own attempt was structural: the same address fails the same
    /// way every time this process asks, so scoring it as transient — the
    /// only admission-free option before this existed — kept it
    /// retryable forever. Removes the route from the book and schedules
    /// nothing, exactly as the ticketed version does -- save a route
    /// holding a live quarantine that cannot be handed over, which stays
    /// (ADR-0011, amendment 2026-09-28: the book never drops a quarantine
    /// it cannot hand over). Quarantined, it is not dialled; a dial after
    /// the lapse that fails the same way removes it then.
    pub fn record_permanent_address_failure_unadmitted(
        &mut self,
        peer: &TransportIdentity,
        address: &str,
    ) {
        if address.is_empty() {
            return;
        }
        let _ = self.hand_over(peer, address, self.clock_ms);
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
        let now_ms = ticket.settled_at(now_ms);
        self.observe(now_ms);
        if !self.issued_here(&ticket) {
            return;
        }
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
            // clears the claim itself. A route holding a live quarantine
            // that cannot be handed over stays, undialled until the lapse, as
            // `record_permanent_address_failure_unadmitted` says.
            let _ = self.hand_over(&peer, ticket.address(), now_ms);
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
        self.observe(now_ms);
        if !self.issued_here(&ticket) {
            return;
        }
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

    /// Settle a dial THIS NODE refused, scoring nothing.
    ///
    /// The counterpart of [`Self::record_authorization_withdrawn`] for a
    /// refusal that is not about trust: the outbound gate's established
    /// hook denies a connection whose address the quarantine
    /// suppresses, and libp2p reports that back as an ordinary
    /// `DialError::Denied`.
    ///
    /// Passing it to [`Self::record_failure`] was wrong in two ways at
    /// once, and both are self-inflicted. The address-scoped score
    /// extended the very quarantine that caused the refusal, so a
    /// suppression this node keeps re-testing could never lapse — a
    /// time-bounded verdict turned permanent by the act of enforcing
    /// it. And the peer-scoped backoff that rides with it advanced a
    /// TRUSTED peer toward punitive backoff over one address this node
    /// declined to use, which is exactly what ADR-0011 separates
    /// address failures from peer failures to prevent: a known-good
    /// route must not be suppressed by a bad one.
    ///
    /// Nothing about the network or the remote peer failed here. The
    /// slot is returned and no retry is scheduled, for the same reason
    /// the authorization path schedules none.
    pub fn record_locally_refused(&mut self, ticket: DialTicket, now_ms: u64) {
        self.observe(now_ms);
        if !self.issued_here(&ticket) {
            return;
        }
        if ticket.owns_scheduler_claim()
            && let Some(peer) = ticket.peer().cloned()
        {
            self.clear_retry_claim(&peer);
        }
        self.settle(ticket);
        self.publish();
    }

    /// Connection SLOTS in use right now: established connections,
    /// dialed and accepted, plus dials admitted and not yet settled --
    /// the slot is reserved at admission, so the ceiling counts what is
    /// on its way (`concurrent_dials_cannot_exceed_the_connection_ceiling`
    /// asserts the reservation). Not the established count alone.
    #[must_use]
    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::Acquire)
    }

    /// Record that the peer at this address authenticated a different
    /// identity.
    ///
    /// Returns whether the quarantine was recorded, which for a ticket
    /// that names a peer it always is: admission reserved the address
    /// entry this outcome may need (`PolicySnapshot::outcomes`, review
    /// R4 on fa3eab8), so a table full of live quarantines can no longer
    /// swallow it. `false` means the ticket named no peer, or was not
    /// issued by this manager (`issued_here`).
    /// `every_admitted_identity_mismatch_is_recorded_when_admissions_compete`
    /// pins it.
    pub fn record_identity_mismatch(&mut self, ticket: DialTicket, now_ms: u64) -> bool {
        let now_ms = ticket.settled_at(now_ms);
        self.observe(now_ms);
        if !self.issued_here(&ticket) {
            return false;
        }
        let mismatched = ticket.peer().cloned().is_some_and(|peer| {
            self.policy
                .record_identity_mismatch(&peer, ticket.address(), now_ms)
        });
        if mismatched && let Some(peer) = ticket.peer().cloned() {
            self.note(GateNote::AddressQuarantined {
                peer,
                for_ms: crate::connection_policy::IDENTITY_MISMATCH_QUARANTINE_MS,
            });
        }
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

    /// Whether THIS manager issued `ticket` (review R6 on fa3eab8).
    ///
    /// Every settlement method writes this manager's policy, retries and
    /// book only for a ticket it issued: a ticket from another manager
    /// carries another manager's reservations and names a dial this one
    /// never admitted. Such a ticket is not recorded here; it ends on its
    /// issuer exactly as a dropped one does -- its pending, connection
    /// and outcome units returned there -- and `record_success` hands
    /// back a slot on the issuer's connection count, since a connection
    /// exists either way. Identity is the shared pending counter, which
    /// every ticket this manager issues holds and no other does.
    /// `a_foreign_ticket_settles_on_its_issuer_and_writes_nothing_here`
    /// pins it.
    fn issued_here(&self, ticket: &DialTicket) -> bool {
        Arc::ptr_eq(&ticket.pending, &self.pending)
    }

    fn settle(&mut self, mut ticket: DialTicket) {
        ticket.settled = true;
        // THE TICKET'S OWN COUNTERS, never this manager's (review R6 on
        // fa3eab8): settlement decremented `self.pending`, so a ticket
        // handed to a manager that did not issue it wrapped that
        // manager's count and left its issuer's reservation held for
        // good. A foreign ticket reaches here only from `record_success`
        // (every other settlement returns first on `issued_here`), and it
        // ends on its issuer like a dropped one.
        ticket.pending.fetch_sub(1, Ordering::AcqRel);
        // Returned by the `publish` that follows every settlement, not
        // here (`outcomes_to_return`) -- when it is this manager's unit;
        // a foreign one is released by the ticket's own `Drop`.
        if ticket.outcome_reserved && Arc::ptr_eq(&ticket.outcomes, &self.outcomes) {
            ticket.outcome_reserved = false;
            self.outcomes_to_return += 1;
        }
        // `connection_kept` stays false, so the ticket's connection
        // reservation is released as it drops. A dial that failed holds
        // no connection.
    }

    /// Settle the dial and TRANSFER its connection reservation.
    fn keep_connection(&mut self, mut ticket: DialTicket) -> ConnectionSlot {
        ticket.connection_kept = true;
        // The slot the ticket reserved, on the counter it reserved it on
        // (review R6 on fa3eab8), not this manager's.
        let slot = ConnectionSlot {
            connections: Arc::clone(&ticket.connections),
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

    /// Whether one more connection to an authenticated peer may be
    /// RETAINED, given what is already held: `held_for_peer` connections
    /// to it, and `connected_peers` distinct peers in all.
    ///
    /// The two peer ceilings (`max_connections_per_peer`,
    /// `max_connected_peers`) are not slots reserved at admission, as the
    /// total is: a dial may name no peer, and an inbound's peer is known
    /// only once Noise has run. So they are decided at retention, by the
    /// one task that holds the open set, which makes the count exact --
    /// there is no second writer to race. A peer already held takes no
    /// new place among the connected peers.
    ///
    /// # Errors
    /// The [`RetentionRefusal`] that applied.
    pub fn admits_retention(
        &self,
        held_for_peer: usize,
        connected_peers: usize,
    ) -> Result<(), RetentionRefusal> {
        if held_for_peer >= self.policy.max_connections_per_peer {
            return Err(RetentionRefusal::PerPeerLimitReached);
        }
        if held_for_peer == 0 && connected_peers >= self.policy.max_connected_peers {
            return Err(RetentionRefusal::ConnectedPeerLimitReached);
        }
        Ok(())
    }

    /// An inbound connection from `peer` was RETAINED: the peer is up.
    ///
    /// Resets the peer's dial backoff -- the peer-scoped suppression and
    /// the scheduled retry with its attempt count -- because a peer that
    /// just reached this profile is better evidence than a timer its
    /// failures set while it was away (architect-cto's ruling of
    /// 2026-10-06, relay seq 13444: a restarted peer must not stay
    /// unreachable for the backoff's 30-60 s after it is back).
    /// CONNECTIVITY.md's cadence stays for a peer that has not shown
    /// itself. Address quarantines are untouched: an inbound proves the
    /// peer, not any address this profile dials it at.
    ///
    /// Called only after retention admitted the connection, so it never
    /// acts for a peer the trust policy refuses. Returns whether there was
    /// anything to reset.
    pub fn record_inbound_retained(&mut self, peer: &TransportIdentity, now_ms: u64) -> bool {
        self.observe(now_ms);
        let backoff = self.policy.clear_peer_backoff(peer);
        let retry = self.retries.remove(peer).is_some();
        if backoff || retry {
            self.publish();
        }
        backoff || retry
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
    /// way -- `origin.names_application_destination()` is the
    /// discriminator. What pins
    /// it is split across three places, and none of them alone covers
    /// the grid: `tests/transport-contract`'s exit gate asserts every
    /// origin against `ConnectivityInfrastructureOnly`, while
    /// `connection_policy`'s own tests cover the other two classes --
    /// `an_unauthorized_peer_is_refused_whatever_the_origin` and
    /// `a_trusted_destination_is_admitted_under_every_origin`.
    ///
    /// Revalidating an established connection with the destination-only
    /// predicate therefore closed connections admission had correctly
    /// permitted: a relay reservation or an AutoNAT probe to an
    /// infrastructure peer completed its handshake and was immediately
    /// dropped.
    ///
    /// It closed relay circuits and DCUtR hole punches to such a peer
    /// too — but admission should never have permitted those, which is
    /// D2 and D1, refused at the predicate since Stage 11 step 2 (see
    /// `DialOrigin::names_application_destination`). This function's
    /// job is to AGREE with admission, so fixing them there fixed them
    /// here; the bug it was written for is the disagreement, not the
    /// classification.
    ///
    /// The inbound path has no origin of its own to consult and no such
    /// pair to honour, which is why it keeps the stricter predicate
    /// unless this profile is connectivity infrastructure for the
    /// peer: an AutoNAT server it dialled, whose dial-back arrives
    /// inbound (step 3); every authorized inbound while it serves
    /// probes (step 4, under `AutonatProbe`) or relays (step 6, under
    /// `RelayReservation`) -- the route-3 arm in the libp2p runtime's
    /// `dialing.rs`, whose closure names the origin. Applying the
    /// stricter predicate to outbound was wrong rather than merely
    /// conservative.
    #[must_use]
    pub fn authorizes_for(&self, class: ConnectionClass, origin: DialOrigin) -> bool {
        if self.shutting_down.load(Ordering::Acquire) {
            return false;
        }
        match class {
            ConnectionClass::Unauthorized => false,
            ConnectionClass::DataPlaneTrusted => true,
            ConnectionClass::ConnectivityInfrastructureOnly => {
                !origin.names_application_destination()
            }
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

    /// Whether `peer` is this profile's own `PeerId`.
    ///
    /// `classify` already answers `Unauthorized` for it, which is right
    /// for a DIAL — this node is not a peer it may connect to. A direct
    /// SEND owes a different answer: `DIRECT.md` makes the local `PeerId`
    /// `InvalidArgument`, a caller mistake, not a trust verdict. Asked
    /// separately so the two do not have to share one code.
    #[must_use]
    pub fn is_local_peer(&self, peer: &TransportIdentity) -> bool {
        self.local_peer.as_ref() == Some(peer)
    }

    /// This node's own `PeerId`, once bound.
    ///
    /// A locally published broadcast has to name a publisher for the
    /// sessions that receive it, and the honest answer is this node --
    /// the same identity the mesh would have authenticated had the
    /// message come from outside.
    #[must_use]
    pub fn local_peer(&self) -> Option<&TransportIdentity> {
        self.local_peer.as_ref()
    }

    /// Begin draining. Admission refuses from the next snapshot on.
    pub fn begin_shutdown(&mut self) {
        self.shutting_down.store(true, Ordering::Release);
        self.policy.shutting_down = true;
        self.publish();
    }

    /// Claim up to `limit` due retries, soonest first, MARKING them
    /// claimed: they stay in the schedule, excluded from selection until
    /// the attempt settles.
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
    /// Claiming is unconditional and RETAINS the entry with `claimed`
    /// set -- an earlier version removed it, and this sentence said so
    /// after the code stopped. The claim ends with the attempt: a failed
    /// dial reschedules through [`Self::record_failure`], which carries
    /// its own backoff; a caller that cannot start a dial this tick -- no
    /// candidate address, the peer no longer authorized -- gives it up
    /// with [`Self::clear_retry_claim`], because a peer with nothing to
    /// try is not usefully "due" again a moment later; and a claim
    /// released mid-tick is taken back with [`Self::reclaim_retry`].
    #[must_use]
    pub fn take_due_retries(&mut self, now_ms: u64, limit: usize) -> Vec<TransportIdentity> {
        self.observe(now_ms);
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
        let attempts = self.retries.get(peer).map_or(0, |r| r.attempts);
        retry_backoff_ms(attempts)
    }

    /// `claimed` carries a claim forward that this failure did not own;
    /// see [`Self::record_failure`].
    /// Returns the attempt number the retry will be.
    fn schedule_retry(
        &mut self,
        peer: TransportIdentity,
        now_ms: u64,
        delay: u64,
        claimed: bool,
    ) -> u32 {
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

        let attempt = attempts.saturating_add(1);
        self.retries.insert(
            peer,
            Retry {
                due_at_ms: now_ms.saturating_add(delay),
                attempts: attempt,
                claimed,
                waits_on: None,
            },
        );
        attempt
    }

    /// The retry and quarantine decisions made since the last call, oldest
    /// first, for the runtime to report (`observability.md` §Logs, A
    /// 2026-10-06). One queue every settlement path writes, so no path
    /// can decide without being reported -- a failure settled inside
    /// `attempt_dial` as much as one the event loop settles.
    pub fn drain_notes(&mut self) -> Vec<GateNote> {
        self.notes.drain(..).collect()
    }

    /// How many notes the [`MAX_GATE_NOTES`] bound discarded, oldest
    /// first, because nothing drained them in time.
    #[must_use]
    pub fn notes_dropped(&self) -> u64 {
        self.notes_dropped
    }

    fn note(&mut self, note: GateNote) {
        if self.notes.len() >= MAX_GATE_NOTES {
            let _ = self.notes.pop_front();
            self.notes_dropped = self.notes_dropped.saturating_add(1);
        }
        self.notes.push_back(note);
    }

    /// What the gate holds against `peer` at `now_ms`, for diagnostics:
    /// its peer-scoped backoff, the earliest release once every known
    /// address is quarantined (`CONNECTIVITY.md` §19), and its scheduled
    /// retry. Times only -- no address leaves the manager.
    #[must_use]
    pub fn peer_gate_state(&self, peer: &TransportIdentity, now_ms: u64) -> PeerGateState {
        PeerGateState {
            backoff_until_ms: self
                .policy
                .peer(peer)
                .and_then(|b| b.until_ms)
                .filter(|until| now_ms < *until),
            quarantined_until_ms: self.policy.quarantined_until(peer, now_ms),
            retry_due_at_ms: self.retries.get(peer).map(|r| r.due_at_ms),
        }
    }
}

/// How many undrained notes the manager keeps. The runtime drains every
/// turn of its loop, so the bound is met only by a turn that settles more
/// dials than this -- each holding a pending-dial slot, so the pending
/// ceiling bounds a turn's settlements too.
pub const MAX_GATE_NOTES: usize = 64;

/// A decision of the gate's that its runtime reports. Peers and times
/// only: no address leaves the manager this way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateNote {
    /// A failed dial scheduled the peer's next retry.
    RetryScheduled {
        /// The peer.
        peer: TransportIdentity,
        /// Who asked for the dial that failed.
        origin: DialOrigin,
        /// What was scheduled.
        retry: RetryScheduled,
    },
    /// An address of the peer answered with another identity and is
    /// quarantined.
    AddressQuarantined {
        /// The peer the address was dialled as.
        peer: TransportIdentity,
        /// For how long.
        for_ms: u64,
    },
}

/// A retry [`ConnectionManager::record_failure`] scheduled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryScheduled {
    /// Which retry this will be: 1 for the first after a run of successes.
    pub attempt: u32,
    /// How long until it is due.
    pub delay_ms: u64,
    /// Whether the failure also put the peer itself in backoff, which it
    /// does only when no other known-good address remains.
    pub peer_backoff: bool,
}

/// The gate's hold on one peer, as [`ConnectionManager::peer_gate_state`]
/// reports it. Every field is `None` for a peer the gate holds nothing
/// against.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PeerGateState {
    /// Dials to the peer are refused until then.
    pub backoff_until_ms: Option<u64>,
    /// Every address in the peer's book is quarantined until then, the
    /// earliest release; `None` while any is dialable (`CONNECTIVITY.md`
    /// §19).
    pub quarantined_until_ms: Option<u64>,
    /// The scheduled retry comes due then.
    pub retry_due_at_ms: Option<u64>,
}

/// `CONNECTIVITY.md`'s first retry delay for a peer not yet verified:
/// 30 seconds.
///
/// PUBLIC, because the same numbers govern two schedules: the gate's
/// re-dial of a peer that would not connect, and the AutoNAT adapter's
/// `retest` of an address a server reported unreachable (`AUTONAT.md`
/// §4, Amendment 2026-09-09 (ii): "the dial gate's constants, applied
/// to a re-test rather than read from a configuration key"). Two
/// copies of 30 s and 5 min would be two numbers that drift.
pub const RETRY_BASE_MS: u64 = 30_000;
/// The ceiling that backoff never exceeds: five minutes.
pub const RETRY_CEILING_MS: u64 = 5 * 60 * 1_000;

/// The delay before the `attempts`-th retry: exponential from
/// [`RETRY_BASE_MS`], bounded by [`RETRY_CEILING_MS`].
///
/// Shifted by a CLAMPED exponent. `1u64 << 64` is undefined-ish in the
/// sense that it panics in debug and wraps in release, and an attempt
/// counter is driven by how often a remote end refuses to connect -- so
/// the clamp is a bound on remote-influenced arithmetic, not a tidiness.
#[must_use]
pub const fn retry_backoff_ms(attempts: u32) -> u64 {
    let shifted = RETRY_BASE_MS.saturating_mul(1u64 << (if attempts < 8 { attempts } else { 8 }));
    if shifted < RETRY_CEILING_MS {
        shifted
    } else {
        RETRY_CEILING_MS
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn the_backoff_is_connectivity_md_s_30s_doubling_to_5m_and_stops_doubling_there() {
        // `CONNECTIVITY.md` "retry after a failure: 30 s, exponentially/
        // backoff bounded by 5 min", now shared with the AutoNAT
        // adapter's `retest` schedule through this one function.
        assert_eq!(retry_backoff_ms(0), 30_000);
        assert_eq!(retry_backoff_ms(1), 60_000);
        assert_eq!(retry_backoff_ms(3), 240_000);
        assert_eq!(retry_backoff_ms(4), 300_000, "clamped at the ceiling");
        assert_eq!(retry_backoff_ms(8), 300_000);
        assert_eq!(
            retry_backoff_ms(u32::MAX),
            300_000,
            "the exponent is clamped, not the product alone"
        );
        assert_eq!(RETRY_BASE_MS, 30_000);
        assert_eq!(RETRY_CEILING_MS, 300_000);
    }

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

    /// The per-peer ceiling: connections 1..=N to one peer are retained,
    /// the next is not -- and a peer already held needs no new place, so
    /// the connected-peer ceiling does not refuse it.
    #[test]
    fn retention_holds_each_peer_to_its_ceiling() {
        let mut policy = ConnectionPolicy::new(64, 64);
        policy.max_connections_per_peer = 3;
        policy.max_connected_peers = 2;
        let m = ConnectionManager::new(policy, 64);
        // One other peer held: this one's first connection makes two.
        assert_eq!(m.admits_retention(0, 1), Ok(()), "connection 1");
        for held in 1..3 {
            // At the connected-peer ceiling, and a held peer is no new place.
            assert_eq!(
                m.admits_retention(held, 2),
                Ok(()),
                "connection {}",
                held + 1
            );
        }
        assert_eq!(
            m.admits_retention(3, 2),
            Err(RetentionRefusal::PerPeerLimitReached),
            "the fourth"
        );
    }

    /// The connected-peer ceiling: a NEW peer past it is refused, one
    /// below it is retained.
    #[test]
    fn retention_holds_the_connected_peers_to_their_ceiling() {
        let mut policy = ConnectionPolicy::new(64, 64);
        policy.max_connected_peers = 2;
        let m = ConnectionManager::new(policy, 64);
        assert_eq!(m.admits_retention(0, 1), Ok(()), "the second peer");
        assert_eq!(
            m.admits_retention(0, 2),
            Err(RetentionRefusal::ConnectedPeerLimitReached),
            "the third"
        );
    }

    /// The defaults are the schema's.
    #[test]
    fn the_peer_ceilings_default_to_the_schemas() {
        let policy = ConnectionPolicy::new(64, 64);
        assert_eq!(
            (policy.max_connected_peers, policy.max_connections_per_peer),
            (256, 3)
        );
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

    /// Review R6 on fa3eab8: settlement decremented the RECEIVING
    /// manager's counters, so a ticket from A settled on B wrapped B's
    /// pending count, left A's reservation held, and `record_success`
    /// moved the connection onto B's ceiling. A foreign ticket now ends
    /// on its issuer and writes nothing into the receiver.
    #[test]
    fn a_foreign_ticket_settles_on_its_issuer_and_writes_nothing_here() {
        let a = manager(4);
        let mut b = manager(4);
        let address = "/ip4/10.0.0.1/tcp/1";

        let t = a
            .handle()
            .admit(&request(P1, address), 0)
            .expect("admitted");
        assert_eq!(a.handle().load().pending_dials(), 1);
        b.record_failure(t, 0);
        assert_eq!(
            a.handle().load().pending_dials(),
            0,
            "A's reservation came back"
        );
        assert_eq!(
            b.handle().load().pending_dials(),
            0,
            "B's count did not wrap"
        );
        assert_eq!(
            b.scheduled_retries(),
            0,
            "B scheduled nothing for a dial it never admitted"
        );
        assert_eq!(
            b.known_addresses(&peer(P1)),
            0,
            "and learned nothing from it"
        );

        let t = a
            .handle()
            .admit(&request(P1, address), 0)
            .expect("admitted");
        assert!(
            !b.record_identity_mismatch(t, 0),
            "B records no foreign quarantine"
        );
        assert_eq!(b.policy.live_quarantines(0), 0);
        assert_eq!(
            a.outcomes.load(Ordering::Acquire),
            0,
            "A's outcome unit came back"
        );

        let t = a
            .handle()
            .admit(&request(P1, address), 0)
            .expect("admitted");
        let slot = b.record_success(t, 0);
        assert_eq!(a.connections(), 1, "the connection stays on A's ceiling");
        assert_eq!(b.connections(), 0, "and never reached B's");
        assert_eq!(a.handle().load().pending_dials(), 0);
        drop(slot);
        assert_eq!(a.connections(), 0);
    }

    /// Review R3 on fa3eab8, and #117's blind review F3: the book admitted
    /// only classified peers and never dropped one that stopped being
    /// classified, so an allowlist of ONE peer rotated past the trust
    /// bound left it holding every peer ever trusted; and the first fix,
    /// dropping them all at once, cost a peer its routes across a trust
    /// flap. The book now keeps the most recently revoked, bounded. THE
    /// CONTROL is an infrastructure peer present throughout.
    #[test]
    fn a_rotating_allowlist_keeps_the_book_bounded_and_a_flap_keeps_its_routes() {
        fn synthetic(n: usize) -> TransportIdentity {
            let mut bytes = [0_u8; 38];
            bytes[..6].copy_from_slice(&[0x00, 0x24, 0x08, 0x01, 0x12, 0x20]);
            bytes[6..14].copy_from_slice(&(n as u64).to_be_bytes());
            TransportIdentity::parse(bs58::encode(bytes).into_string())
                .expect("a decodable synthetic identity")
        }
        let relay = peer(P2);
        let trusting_one = |p: &TransportIdentity| {
            TrustSources::new(
                PeerTrustPolicy::new([p.clone()]).expect("one peer"),
                InfrastructureSet::new([relay.clone()]).expect("one relay"),
            )
        };
        let mut m = ConnectionManager::new(ConnectionPolicy::new(64, 64), 64);
        let rotations = PeerTrustPolicy::MAX_ALLOWED_PEERS + 4;
        for n in 0..rotations {
            let current = synthetic(n);
            let _ = m.set_trust(trusting_one(&current), &[]);
            assert!(m.learn_address(&current, "/ip4/10.0.0.1/tcp/1", 0));
            assert!(m.learn_address(&relay, "/ip4/10.0.0.2/tcp/1", 0));
        }
        assert_eq!(
            m.policy.book.len(),
            2 + MAX_RETIRED_BOOK_PEERS,
            "the peer trusted now, the relay and the retired bound, not {rotations} peers"
        );
        assert_eq!(
            m.known_addresses(&synthetic(0)),
            0,
            "the longest-revoked is gone"
        );
        assert_eq!(
            m.known_addresses(&synthetic(rotations - 2)),
            1,
            "a recently revoked peer keeps its route"
        );
        assert_eq!(m.known_addresses(&relay), 1, "the relay's address is kept");

        // A FLAP: revoked and re-added, its route intact.
        let flapping = synthetic(rotations - 1);
        let _ = m.set_trust(trusting_one(&synthetic(rotations)), &[]);
        let _ = m.set_trust(trusting_one(&flapping), &[]);
        assert_eq!(
            m.dial_candidates(&flapping, 0),
            vec!["/ip4/10.0.0.1/tcp/1".to_owned()],
            "re-authorized, it dials where it did before"
        );
        assert!(m.retired.len() <= MAX_RETIRED_BOOK_PEERS);
    }

    #[test]
    fn an_outcome_settled_before_its_admission_time_is_still_recorded() {
        // Admission counts live quarantines at ITS time; an outcome
        // settled at an earlier time -- a caller whose clock reads behind
        // the admitting one's -- would see a quarantine that lapsed in
        // between as live, and find none of the room it was promised
        // (#137 re-review 8, risk 1). A settlement is judged no earlier
        // than its admission.
        use crate::connection_policy::IDENTITY_MISMATCH_QUARANTINE_MS as Q;
        let mut policy = ConnectionPolicy::new(64, 64);
        policy.max_addresses = 2;
        let mut m = ConnectionManager::new(policy, 64);
        let _ = m.set_trust(trusting(&[P1, P2], &[]), &[]);
        let t = m
            .handle()
            .admit(&request(P1, "/ip4/10.0.0.1/tcp/1"), 0)
            .expect("admitted");
        assert!(m.record_identity_mismatch(t, 0), "A is quarantined until Q");

        // At Q, A has lapsed: both slots are free, and two dials take them.
        let tx = m
            .handle()
            .admit(&request(P1, "/ip4/10.0.0.2/tcp/1"), Q)
            .expect("X admitted");
        let ty = m
            .handle()
            .admit(&request(P1, "/ip4/10.0.0.3/tcp/1"), Q)
            .expect("Y admitted");
        // Both settle a millisecond EARLIER, when A was still live.
        assert!(
            m.record_identity_mismatch(tx, Q - 1),
            "X's mismatch is recorded"
        );
        assert!(m.record_identity_mismatch(ty, Q - 1), "and so is Y's");
    }

    #[test]
    fn a_failure_settled_before_its_admission_time_is_still_recorded() {
        // The same clamp on the other settlements (#138 review F5): a
        // transient failure outside the book needs a slot too, and at an
        // earlier clock a lapsed quarantine would hold it.
        use crate::connection_policy::IDENTITY_MISMATCH_QUARANTINE_MS as Q;
        let mut policy = ConnectionPolicy::new(64, 64);
        policy.max_addresses = 1;
        let mut m = ConnectionManager::new(policy, 64);
        let _ = m.set_trust(trusting(&[P1, P2], &[]), &[]);
        let t = m
            .handle()
            .admit(&request(P1, "/ip4/10.0.0.1/tcp/1"), 0)
            .expect("admitted");
        assert!(
            m.record_identity_mismatch(t, 0),
            "A fills the one slot until Q"
        );
        let tf = m
            .handle()
            .admit(&request(P2, "/ip4/10.0.0.2/tcp/1"), Q)
            .expect("admitted once A lapsed");
        m.record_failure(tf, Q - 1);
        assert!(
            m.policy
                .address(&peer(P2), "/ip4/10.0.0.2/tcp/1")
                .is_some_and(|s| s.consecutive_failures == 1),
            "the failure is recorded"
        );
    }

    #[test]
    fn a_success_settled_before_its_admission_time_is_still_recorded() {
        // A success outside the book needs a slot as a failure does
        // (#138 review F5 remainder).
        use crate::connection_policy::IDENTITY_MISMATCH_QUARANTINE_MS as Q;
        let mut policy = ConnectionPolicy::new(64, 64);
        policy.max_addresses = 1;
        let mut m = ConnectionManager::new(policy, 64);
        let _ = m.set_trust(trusting(&[P1, P2], &[]), &[]);
        let t = m
            .handle()
            .admit(&request(P1, "/ip4/10.0.0.1/tcp/1"), 0)
            .expect("admitted");
        assert!(
            m.record_identity_mismatch(t, 0),
            "A fills the one slot until Q"
        );
        let ts = m
            .handle()
            .admit(&request(P2, "/ip4/10.0.0.2/tcp/1"), Q)
            .expect("admitted once A lapsed");
        drop(m.record_success(ts, Q - 1));
        assert!(
            m.policy
                .address(&peer(P2), "/ip4/10.0.0.2/tcp/1")
                .is_some_and(|s| s.last_success_ms.is_some()),
            "the success is recorded"
        );
    }

    #[test]
    fn a_permanent_failure_settled_before_its_admission_time_still_removes_the_route() {
        // A permanent failure hands the route out of the book; judged at
        // an earlier clock, the route's own lapsed quarantine reads as
        // live and the hand-over is refused into a table with no room
        // (#138 review F5 remainder).
        use crate::connection_policy::IDENTITY_MISMATCH_QUARANTINE_MS as Q;
        let route = "/ip4/10.0.0.1/tcp/1";
        let mut policy = ConnectionPolicy::new(64, 64);
        policy.max_addresses = 2;
        let mut m = ConnectionManager::new(policy, 64);
        let _ = m.set_trust(trusting(&[P1, P2], &[]), &[]);
        assert!(m.learn_address(&peer(P1), route, 0));
        let t = m.handle().admit(&request(P1, route), 0).expect("admitted");
        assert!(
            m.record_identity_mismatch(t, 0),
            "the route is quarantined until Q"
        );
        // One slot outside the book holds a quarantine live past Q; the
        // other is the outcome room admission reserves for the dial, so
        // a live quarantine handed over now would find no free unit.
        let t = m
            .handle()
            .admit(&request(P2, "/ip4/10.0.0.9/tcp/1"), 1_000)
            .expect("admitted");
        assert!(m.record_identity_mismatch(t, 1_000));
        let tp = m
            .handle()
            .admit(&request(P1, route), Q)
            .expect("admitted once the route's quarantine lapsed");
        m.record_permanent_failure(tp, Q - 1);
        assert_eq!(m.known_addresses(&peer(P1)), 0, "the route left the book");
    }

    /// Review R4 on fa3eab8: admission checked that an outcome COULD be
    /// recorded and reserved nothing, so two dials admitted against the
    /// last free entry both counted on it, and the second identity
    /// mismatch found the table full of live quarantines and was not
    /// recorded -- that address dialable again the moment an unrelated
    /// quarantine lapsed. The review's own timeline, at a table of two.
    #[test]
    #[expect(
        clippy::many_single_char_names,
        reason = "short names for the handful of actors this test juggles, each introduced where it is built"
    )]
    fn every_admitted_identity_mismatch_is_recorded_when_admissions_compete() {
        use crate::connection_policy::IDENTITY_MISMATCH_QUARANTINE_MS as Q;
        let mut policy = ConnectionPolicy::new(64, 64);
        policy.max_addresses = 2;
        let mut m = ConnectionManager::new(policy, 64);
        let _ = m.set_trust(trusting(&[P1, P2], &[]), &[]);
        let (a, x, y) = (
            "/ip4/10.0.0.1/tcp/1",
            "/ip4/10.0.0.2/tcp/1",
            "/ip4/10.0.0.3/tcp/1",
        );

        let t = m.handle().admit(&request(P1, a), 0).expect("admitted");
        assert!(m.record_identity_mismatch(t, 0), "A is quarantined until Q");

        let late = Q - 1_000;
        let tx = m
            .handle()
            .admit(&request(P1, x), late)
            .expect("X is admitted: one entry is free");
        assert_eq!(
            m.handle().admit(&request(P1, y), late).err(),
            Some(DialDenial::PolicyStateFull),
            "Y is refused: the last entry is already X's to record into"
        );
        assert!(
            m.record_identity_mismatch(tx, late),
            "X's mismatch finds room"
        );

        // A lapses; Y is admitted now, and its mismatch is recorded too.
        let ty = m
            .handle()
            .admit(&request(P1, y), Q)
            .expect("Y is admitted once A's quarantine lapsed");
        assert!(
            m.record_identity_mismatch(ty, Q),
            "Y's mismatch is recorded"
        );

        for (address, until) in [(x, late + Q), (y, Q + Q)] {
            assert_eq!(
                m.handle().admit(&request(P1, address), until - 1).err(),
                Some(DialDenial::AddressQuarantined),
                "{address} stays suppressed for its whole interval"
            );
        }
        assert_eq!(
            m.outcomes.load(Ordering::Acquire),
            0,
            "every settled ticket returned its unit"
        );

        // An unsettled ticket returns its unit as it drops.
        let dropped = m
            .handle()
            .admit(&request(P2, "/ip4/10.0.0.9/tcp/1"), 2 * Q + 1)
            .expect("admitted");
        assert_eq!(m.outcomes.load(Ordering::Acquire), 1);
        drop(dropped);
        assert_eq!(m.outcomes.load(Ordering::Acquire), 0);
    }

    #[test]
    fn a_hole_punch_dials_failure_settles_and_scores_schedules_and_learns_nothing() {
        let mut m = manager(4);
        let p = peer(P1);
        let ticket = m
            .handle()
            .admit(
                &request_at(P1, "/ip4/10.0.0.1/tcp/4001", DialOrigin::DcutrHolePunch),
                0,
            )
            .expect("a punch toward a data-plane peer is admitted");
        m.record_failure(ticket, 0);
        assert_eq!(m.scheduled_retries(), 0, "no reconnect is scheduled");
        assert_eq!(m.known_addresses(&p), 0, "the candidate is not learned");
        assert_eq!(m.handle().load().pending_dials(), 0, "the slot is settled");
        // Asked of the backoff itself: a dial to another address would be
        // admitted either way now, an untried address not being held by a
        // backoff it did not earn (ADR-0011 A 2026-10-01).
        assert!(
            m.policy().peer(&p).is_none_or(|b| b.is_clear_at(1)),
            "the peer is not in backoff"
        );
        // THE CONTROL: the same failure under any other origin schedules
        // the retry, learns the address and puts the peer in backoff.
        let mut m = manager(4);
        let ticket = m
            .handle()
            .admit(&request(P1, "/ip4/10.0.0.1/tcp/4001"), 0)
            .expect("admitted");
        m.record_failure(ticket, 0);
        assert_eq!(m.scheduled_retries(), 1);
        assert_eq!(m.known_addresses(&p), 1);
        assert!(
            m.policy().peer(&p).is_some_and(|b| !b.is_clear_at(1)),
            "the peer is in backoff"
        );
    }

    /// A circuit dial that never reached its relay scores nothing against
    /// the destination: no peer backoff, so the next `DialPeer` is
    /// admitted; the route is kept; the reconnect is scheduled and made
    /// due by the relay's connection, not by its delay. A connection to
    /// another peer makes nothing due. THE CONTROL: the same failure
    /// through `record_failure` backs the peer off.
    #[test]
    fn a_circuit_that_never_reached_its_relay_backs_off_nothing_and_waits_on_the_relay() {
        let circuit = format!("/ip4/10.0.0.9/tcp/4001/p2p/{P2}/p2p-circuit");
        let p = peer(P1);
        let relay = peer(P2);
        let mut m = manager(4);
        let ticket = m
            .handle()
            .admit(&request_at(P1, &circuit, DialOrigin::RelayCircuit), 0)
            .expect("a circuit toward a data-plane peer is admitted");
        let scheduled = m
            .record_relay_hop_unreached(ticket, &relay, 0)
            .expect("a retry is scheduled");
        assert!(!scheduled.peer_backoff, "reported without peer backoff");
        assert!(
            m.policy().peer(&p).is_none_or(|b| b.is_clear_at(1)),
            "the peer is not in backoff"
        );
        assert_eq!(m.known_addresses(&p), 1, "the circuit route is kept");
        assert_eq!(m.handle().load().pending_dials(), 0, "the slot is settled");
        assert!(
            m.handle()
                .admit(&request_at(P1, &circuit, DialOrigin::RelayCircuit), 1)
                .is_ok(),
            "the next circuit dial is admitted at once"
        );
        assert!(m.take_due_retries(1, 8).is_empty(), "not due yet");
        m.relay_reached(&peer(P1), 2);
        assert!(
            m.take_due_retries(2, 8).is_empty(),
            "another peer's connection is not the relay"
        );
        m.relay_reached(&relay, 3);
        assert_eq!(
            m.take_due_retries(3, 8),
            [p.clone()],
            "due the moment the relay connects"
        );

        let mut m = manager(4);
        let ticket = m
            .handle()
            .admit(&request_at(P1, &circuit, DialOrigin::RelayCircuit), 0)
            .expect("admitted");
        m.record_failure(ticket, 0);
        assert!(
            m.policy().peer(&p).is_some_and(|b| !b.is_clear_at(1)),
            "the control: scored as the peer's failure, it backs the peer off"
        );
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

    /// ADR-0011 A 2026-10-01 (relay seq 9992), the race made
    /// deterministic: a stale address's dial fails and schedules the
    /// peer's retry; a FRESH address learned meanwhile makes that retry due
    /// at once rather than at `RETRY_BASE_MS`; once that address has failed
    /// too, learning it again moves nothing.
    #[test]
    fn a_newly_learned_address_makes_the_retry_due_once() {
        let mut m = manager(8);
        let stale = m
            .handle()
            .load()
            .admit(&request(P1, "/stale"), 0)
            .expect("admitted");
        m.record_failure(stale, 0);
        assert!(
            m.take_due_retries(1_000, 8).is_empty(),
            "the control: the retry waits its backoff"
        );

        assert!(m.learn_address(&peer(P1), "/fresh", 1_000));
        assert_eq!(
            m.take_due_retries(1_000, 8),
            vec![peer(P1)],
            "the fresh route is tried now"
        );
        let fresh = m
            .handle()
            .admit(
                &request_at(P1, "/fresh", DialOrigin::ConnectionManager),
                1_000,
            )
            .expect("admitted");
        m.record_failure(fresh, 1_000);

        assert!(m.learn_address(&peer(P1), "/fresh", 2_000), "already known");
        assert!(
            m.take_due_retries(2_000, 8).is_empty(),
            "a re-learned address with a record of its own moves nothing"
        );
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
    fn a_retained_inbound_resets_the_peers_backoff_and_keeps_its_quarantines() {
        let mut m = manager(8);
        let t = m
            .handle()
            .load()
            .admit(&request(P1, "/a"), 0)
            .expect("admitted");
        m.record_failure(t, 0);
        let q = m
            .handle()
            .load()
            .admit(&request(P1, "/q"), 0)
            .expect("admitted");
        assert!(m.record_identity_mismatch(q, 0));
        // The control: the failure holds the peer off, and its retry is
        // scheduled 30 s out.
        assert_eq!(
            m.handle().load().admit(&request(P1, "/a"), 1_000).err(),
            Some(DialDenial::PeerBackoff)
        );
        assert_eq!(m.scheduled_retries(), 1);

        assert!(m.record_inbound_retained(&peer(P1), 1_000));
        assert_eq!(m.scheduled_retries(), 0, "the retry and its count go");
        assert!(
            m.handle().load().admit(&request(P1, "/a"), 1_000).is_ok(),
            "the peer is dialable at once"
        );
        assert_eq!(
            m.handle().load().admit(&request(P1, "/q"), 1_000).err(),
            Some(DialDenial::AddressQuarantined),
            "an inbound proves the peer, not the mismatched address"
        );
        // The next failure starts the cadence over, at 30 s.
        let t = m
            .handle()
            .load()
            .admit(&request(P1, "/a"), 2_000)
            .expect("admitted");
        m.record_failure(t, 2_000);
        assert!(!m.is_retry_due(&peer(P1), 31_999));
        assert!(m.is_retry_due(&peer(P1), 32_000));
        // Nothing to reset is said so.
        assert!(!m.record_inbound_retained(&peer(P2), 2_000));
    }

    #[test]
    fn a_failure_reports_the_retry_it_scheduled_and_the_gate_state_follows() {
        let mut m = manager(8);
        assert_eq!(m.peer_gate_state(&peer(P1), 0), PeerGateState::default());
        let t = m
            .handle()
            .load()
            .admit(&request(P1, "/a"), 0)
            .expect("admitted");
        assert_eq!(
            m.record_failure(t, 0),
            Some(RetryScheduled {
                attempt: 1,
                delay_ms: 30_000,
                peer_backoff: true
            })
        );
        assert_eq!(
            m.peer_gate_state(&peer(P1), 1_000),
            PeerGateState {
                backoff_until_ms: Some(30_000),
                quarantined_until_ms: None,
                retry_due_at_ms: Some(30_000),
            }
        );
        let t = m
            .handle()
            .load()
            .admit(&request(P1, "/a"), 30_000)
            .expect("past the backoff");
        assert_eq!(
            m.record_failure(t, 30_000),
            Some(RetryScheduled {
                attempt: 2,
                delay_ms: 60_000,
                peer_backoff: true
            })
        );
        // A quarantine holds the peer only once EVERY known address is
        // quarantined (section 19): one of two is not, with the other a
        // route still -- the control -- and both are, by the earlier
        // release, which is when the peer is dialable again; it lapses
        // there.
        let q_ms: u64 = 30 * 60 * 1_000;
        assert!(m.learn_address(&peer(P2), "/q1", 0));
        assert!(m.learn_address(&peer(P2), "/q2", 0));
        let q1 = m
            .handle()
            .load()
            .admit(&request(P2, "/q1"), 0)
            .expect("admitted");
        assert!(m.record_identity_mismatch(q1, 0));
        let one = m.peer_gate_state(&peer(P2), 1_000);
        assert_eq!(one.quarantined_until_ms, None, "/q2 is still a route");
        let q2 = m
            .handle()
            .load()
            .admit(&request(P2, "/q2"), 5_000)
            .expect("admitted");
        assert!(m.record_identity_mismatch(q2, 5_000));
        let held = m.peer_gate_state(&peer(P2), 6_000);
        assert_eq!(held.quarantined_until_ms, Some(q_ms), "the earlier release");
        assert_eq!(held.backoff_until_ms, None, "a mismatch is not a backoff");
        assert_eq!(
            m.peer_gate_state(&peer(P2), q_ms).quarantined_until_ms,
            None
        );
        // A hole-punch dial schedules nothing and says so.
        let punch = m
            .handle()
            .load()
            .admit(&request_at(P2, "/p", DialOrigin::DcutrHolePunch), 0)
            .expect("admitted");
        assert_eq!(m.record_failure(punch, 0), None);
    }

    #[test]
    fn every_retry_and_quarantine_is_noted_once_and_the_bound_drops_the_oldest() {
        let mut m = manager(8);
        let t = m
            .handle()
            .load()
            .admit(&request(P1, "/a"), 0)
            .expect("admitted");
        let _ = m.record_failure(t, 0);
        let q = m
            .handle()
            .load()
            .admit(&request(P2, "/q"), 0)
            .expect("admitted");
        assert!(m.record_identity_mismatch(q, 0));
        // A hole-punch failure schedules nothing, so notes nothing: the
        // control that a note follows a decision, not a settlement.
        let punch = m
            .handle()
            .load()
            .admit(&request_at(P2, "/p", DialOrigin::DcutrHolePunch), 0)
            .expect("admitted");
        let _ = m.record_failure(punch, 0);
        assert_eq!(
            m.drain_notes(),
            vec![
                GateNote::RetryScheduled {
                    peer: peer(P1),
                    origin: DialOrigin::ConnectionManager,
                    retry: RetryScheduled {
                        attempt: 1,
                        delay_ms: 30_000,
                        peer_backoff: true
                    },
                },
                GateNote::AddressQuarantined {
                    peer: peer(P2),
                    for_ms: 30 * 60 * 1_000
                },
            ]
        );
        assert!(m.drain_notes().is_empty(), "drained, not copied");

        // The bound: one more than it holds loses the oldest, counted.
        let mut now = 0;
        for _ in 0..=MAX_GATE_NOTES {
            now += 400_000;
            let t = m
                .handle()
                .load()
                .admit(&request(P1, "/a"), now)
                .expect("past the backoff");
            let _ = m.record_failure(t, now);
        }
        let notes = m.drain_notes();
        assert_eq!(notes.len(), MAX_GATE_NOTES);
        assert_eq!(m.notes_dropped(), 1);
        assert!(matches!(
            notes.first(),
            Some(GateNote::RetryScheduled { retry, .. }) if retry.attempt == 3
        ));
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
        assert_eq!(
            m.borrow().connections(),
            0,
            "and so did the connection slot"
        );
    }

    #[test]
    fn an_outcome_unit_is_held_until_its_snapshot_is_installed() {
        // A settlement and a hand-over each free a unit of the outcome
        // reservation's room only AFTER the snapshot counting their
        // quarantine is installed: until that write, a holder of the old
        // snapshot admits against it (#137 re-review 8, finding 1, and
        // the pre-existing settle case). The seam reads the counter at
        // the instant before the write.
        fn held_at_install(
            m: &mut ConnectionManager,
            act: impl FnOnce(&mut ConnectionManager),
        ) -> Option<usize> {
            let seen = std::rc::Rc::new(std::cell::Cell::new(None));
            let (outcomes, into) = (Arc::clone(&m.outcomes), std::rc::Rc::clone(&seen));
            BEFORE_INSTALL.with(|h| {
                *h.borrow_mut() = Some(Box::new(move || {
                    into.set(Some(outcomes.load(Ordering::Acquire)));
                }));
            });
            act(m);
            BEFORE_INSTALL.with(|h| h.borrow_mut().take());
            seen.get()
        }

        // SETTLEMENT: the failure's ticket unit is still counted.
        let mut m = manager(8);
        let ticket = m
            .handle()
            .admit(&request(P1, "/ip4/198.51.100.1/tcp/1"), 0)
            .expect("admitted");
        assert_eq!(
            held_at_install(&mut m, |m| {
                let _ = m.record_failure(ticket, 0);
            }),
            Some(1),
            "the settled ticket's unit is held until the install"
        );
        assert_eq!(m.outcomes.load(Ordering::Acquire), 0, "and returned after");

        // HAND-OVER: a live quarantine leaving the book holds a unit.
        let mut m = full_book_with_a_quarantine(1);
        m.policy.max_addresses = 2;
        assert_eq!(
            held_at_install(&mut m, |m| {
                assert!(m.learn_address(&peer(P1), "/ip4/203.0.113.7/tcp/1", 0));
            }),
            Some(1),
            "the handed-over quarantine's unit is held until the install"
        );
        assert_eq!(m.outcomes.load(Ordering::Acquire), 0, "and returned after");
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
    fn a_peer_in_both_sets_is_data_plane_trusted() {
        // THE SCHEMA'S RULE, PINNED WHERE IT IS ENFORCED: "a PeerId in
        // both sets is treated as DataPlaneTrusted for protocol
        // admission" is the branch order of `TrustSources::classify` --
        // the data-plane policy is asked before the infrastructure set
        // -- and nothing asserted the CLASS of a NON-LOCAL peer listed
        // in both until this test (the local-identity test above lists
        // P1 in both, but P1 is the local peer there and exits on the
        // first branch). Swapping the two branches passes every test
        // in this crate (348 of them) and fails five in
        // `libp2p/src/runtime/dialing.rs`, all of which reach the
        // question sideways through `set_trust`'s revocation diff
        // rather than by reading the class; this crate's own
        // `a_widened_authorization_evicts_nothing` lists P1 in both sets
        // and goes silently vacuous under the swap, because the
        // promotion it names stops being one. The swap would hand
        // `ClassGated` a denying handler for a peer the operator listed
        // in `trust.allowed_peers`. (An earlier version of this comment
        // said the swap passed every test in the WORKSPACE; it had been
        // run against this crate alone. Both review findings on PR #80.)
        let mut m = untrusting(8);
        let _ = m.set_trust(trusting(&[P1], &[P1]), &[]);
        assert_eq!(
            m.classify(&peer(P1)),
            ConnectionClass::DataPlaneTrusted,
            "the data-plane policy answers for a peer in both sets"
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
            "a full book of untried addresses refuses rather than displacing one: \
             a stream of assertions cannot churn out a peer's untried routes"
        );
        assert_eq!(m.known_addresses(&peer(P1)), DEFAULT_MAX_ADDRESSES_PER_PEER);
    }

    /// The configured bound is the one that binds, clamped to 1..=32 so
    /// no configuration unbounds a list the peer itself writes.
    #[test]
    fn the_configured_address_bound_binds_and_is_clamped() {
        let fill = |m: &mut ConnectionManager, n: usize| {
            (0..n)
                .filter(|i| m.learn_address(&peer(P1), &format!("/ip4/198.51.100.{i}/tcp/1"), 0))
                .count()
        };
        let mut m = manager(8);
        m.set_max_addresses_per_peer(16);
        assert_eq!(fill(&mut m, 20), 16, "the profile's 16, not the default 8");
        let mut m = manager(8);
        m.set_max_addresses_per_peer(10_000);
        assert_eq!(
            fill(&mut m, 40),
            MAX_ADDRESSES_PER_PEER,
            "clamped to the ceiling"
        );
        let mut m = manager(8);
        m.set_max_addresses_per_peer(0);
        assert_eq!(fill(&mut m, 5), 1, "clamped to at least one");
    }

    #[test]
    fn a_full_book_of_recently_good_routes_refuses_the_newcomer() {
        // ADR-0011, amendment 2026-09-28: a recently-good entry -- worked,
        // not failed since -- is never given up. Eight of them is not the
        // moved-peer case, and the newcomer waits.
        let mut m = manager(8);
        for i in 0..DEFAULT_MAX_ADDRESSES_PER_PEER {
            let address = format!("/ip4/198.51.100.{i}/tcp/1");
            assert!(m.learn_address(&peer(P1), &address, 0));
            prove_then_fail(&mut m, &address, 0, 0);
        }
        assert!(!m.learn_address(&peer(P1), A2, 1_000));
        assert_eq!(m.known_addresses(&peer(P1)), DEFAULT_MAX_ADDRESSES_PER_PEER);
    }

    #[test]
    fn an_identity_mismatch_erases_the_proof() {
        // A route that authenticated a different PeerId is not proof for
        // this one; the quarantine lapsing restores dialability, not the
        // proof (#137 round 5, finding 1).
        let mut m = manager(8);
        assert!(m.learn_address(&peer(P1), A1, 0));
        prove_then_fail(&mut m, A1, 0, 0);
        assert!(
            m.policy
                .address(&peer(P1), A1)
                .is_some_and(crate::connection_policy::AddressState::is_recently_good)
        );
        let ticket = m.handle().admit(&request(P1, A1), 1_000).expect("admitted");
        assert!(m.record_identity_mismatch(ticket, 1_000));
        let state = m.policy.address(&peer(P1), A1).expect("kept");
        assert_eq!(state.last_success_ms, None, "the proof is gone");
    }

    #[test]
    fn a_book_entrys_state_is_never_pruned_apart_from_it() {
        // The book remembers the proof: a working route idle past the
        // policy's TTL keeps its success, so one later blip cannot turn it
        // into "never worked" (#137 round 5, C1). The control: the same
        // state for an address the book does not hold is pruned.
        let mut m = manager(8);
        assert!(m.learn_address(&peer(P1), A1, 0));
        prove_then_fail(&mut m, A1, 0, 0);
        let stray = m.handle().admit(&request(P1, A2), 0).expect("admitted");
        drop(m.record_success(stray, 0));
        let later = crate::connection_policy::DEFAULT_IDLE_TTL_MS + 10_000;
        let _ = m.policy.prune(later);
        assert!(
            m.policy
                .address(&peer(P1), A1)
                .is_some_and(|s| s.last_success_ms.is_some()),
            "the book entry's proof survived the prune"
        );
        assert!(
            m.policy.address(&peer(P1), A2).is_none(),
            "the control: state outside the book is pruned"
        );
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

    /// Book entry i of P1, the third one quarantined, and one quarantined
    /// address OUTSIDE the book filling a table of `table` records.
    fn full_book_with_a_quarantine(table: usize) -> ConnectionManager {
        let mut m = manager(8);
        for i in 0..DEFAULT_MAX_ADDRESSES_PER_PEER {
            assert!(m.learn_address(&peer(P1), &format!("/ip4/198.51.100.{i}/tcp/1"), 0));
        }
        for address in ["/ip4/198.51.100.3/tcp/1", "/ip4/203.0.113.9/tcp/1"] {
            let ticket = m
                .handle()
                .admit(&request(P1, address), 0)
                .expect("admitted");
            assert!(m.record_identity_mismatch(ticket, 0));
        }
        // Shrunk only now: admission reserves room for an outcome, so a
        // table this small would refuse the setup's own dials.
        m.policy.max_addresses = table;
        m
    }

    #[test]
    fn a_full_table_keeps_a_quarantined_entry_in_the_book() {
        // ADR-0011, amendment 2026-09-28: the book never drops a live
        // quarantine it cannot hand over. The table's one record outside
        // the book is punitive, so nothing can make room for the book's
        // quarantined entry: it stays, and the newcomer is refused.
        let mut m = full_book_with_a_quarantine(1);
        assert!(
            !m.learn_address(&peer(P1), "/ip4/203.0.113.7/tcp/1", 0),
            "no room for the quarantine: the newcomer is refused"
        );
        assert!(
            m.policy
                .book
                .get(&peer(P1))
                .is_some_and(|k| k.contains("/ip4/198.51.100.3/tcp/1")),
            "the quarantined entry is still the book's"
        );

        // The control: with room for one more record, the quarantined
        // entry makes way and its quarantine goes with it into the table.
        let mut m = full_book_with_a_quarantine(2);
        assert!(m.learn_address(&peer(P1), "/ip4/203.0.113.7/tcp/1", 0));
        assert!(
            !m.policy
                .book
                .get(&peer(P1))
                .is_some_and(|k| k.contains("/ip4/198.51.100.3/tcp/1")),
            "it left the book"
        );
        assert!(
            !m.policy
                .is_address_dialable(&peer(P1), "/ip4/198.51.100.3/tcp/1", 0),
            "and the table still holds its quarantine"
        );
    }

    #[test]
    fn a_book_entrys_record_is_never_evicted_to_make_room() {
        // The table's least-recently-touched record is a book entry's
        // proof; a table full outside the book makes room from outside
        // it, and the proof survives (#137 re-review 6, F2).
        let book = "/ip4/192.0.2.2/tcp/4001";
        let mut m = manager(8);
        assert!(m.learn_address(&peer(P1), book, 0));
        let _ = prove_then_fail(&mut m, book, 0, 0);
        m.policy.max_addresses = 2;
        for (i, at) in [(1, 10_000), (2, 20_000), (3, 30_000)] {
            assert!(m.policy.record_address_failure(
                &peer(P2),
                &format!("/ip4/198.51.100.{i}/tcp/1"),
                at,
                0
            ));
        }
        assert!(
            m.policy
                .address(&peer(P1), book)
                .and_then(|s| s.last_success_ms)
                .is_some(),
            "the book entry's proof survived the room-making"
        );
        assert!(
            m.policy
                .address(&peer(P2), "/ip4/198.51.100.1/tcp/1")
                .is_none(),
            "the control: room was made, from outside the book"
        );
    }

    #[test]
    fn a_record_leaving_the_book_keeps_the_table_bound() {
        // A failure-only book entry released into a table full of live
        // quarantines is dropped, not added past the bound (#137
        // re-review 6, F3).
        let entry = "/ip4/192.0.2.2/tcp/4001";
        let mut m = manager(8);
        assert!(m.learn_address(&peer(P1), entry, 0));
        assert!(m.policy.record_address_failure(&peer(P1), entry, 0, 0));
        m.policy.max_addresses = 1;
        assert!(
            m.policy
                .record_identity_mismatch(&peer(P2), "/ip4/198.51.100.1/tcp/1", 0)
        );
        assert!(m.policy.release_from_book(&peer(P1), entry, 0));
        assert_eq!(m.known_addresses(&peer(P1)), 0, "it left the book");
        assert_eq!(
            m.policy.address_entries(),
            1,
            "and the table outside the book stays at its bound"
        );
    }

    #[test]
    fn a_book_entrys_first_record_takes_no_room_from_outside_it() {
        // A book key's first record is bounded by the book, and releasing
        // a book entry with no record hands nothing over: neither evicts
        // an unrelated record from a full table (#137 re-review 6, F5).
        let outside = "/ip4/198.51.100.1/tcp/1";
        let mut m = manager(8);
        assert!(m.policy.record_address_failure(&peer(P2), outside, 0, 0));
        m.policy.max_addresses = 1;
        assert!(m.learn_address(&peer(P1), "/ip4/192.0.2.2/tcp/4001", 0));
        assert!(
            m.policy
                .record_address_failure(&peer(P1), "/ip4/192.0.2.2/tcp/4001", 0, 0)
        );
        assert!(m.learn_address(&peer(P1), "/ip4/192.0.2.3/tcp/4001", 0));
        assert!(
            m.policy
                .release_from_book(&peer(P1), "/ip4/192.0.2.3/tcp/4001", 0)
        );
        // A mismatch and a success are the other two first records a book
        // key can get (#137 re-review 7, N1).
        assert!(m.learn_address(&peer(P1), "/ip4/192.0.2.4/tcp/4001", 0));
        assert!(
            m.policy
                .record_identity_mismatch(&peer(P1), "/ip4/192.0.2.4/tcp/4001", 0)
        );
        assert!(m.learn_address(&peer(P1), "/ip4/192.0.2.5/tcp/4001", 0));
        m.policy
            .record_success(&peer(P1), "/ip4/192.0.2.5/tcp/4001", 0);
        assert!(
            m.policy
                .address(&peer(P1), "/ip4/192.0.2.5/tcp/4001")
                .is_some_and(|s| s.last_success_ms.is_some()),
            "the success was recorded"
        );
        assert!(
            m.policy.address(&peer(P2), outside).is_some(),
            "the record outside the book was not evicted for either"
        );
    }

    #[test]
    fn a_book_quarantine_takes_no_admission_room() {
        // A book entry's live quarantine takes no table slot, so it takes
        // none of the room admission reserves for outcomes, nor fills the
        // table for the static check (#137 re-review 7, A): with a table
        // of one, a quarantined book entry leaves every dial admissible.
        let quarantined = "/ip4/192.0.2.2/tcp/4001";
        let other = "/ip4/192.0.2.3/tcp/4001";
        let mut m = manager(8);
        m.policy.max_addresses = 1;
        assert!(m.learn_address(&peer(P1), quarantined, 0));
        assert!(m.learn_address(&peer(P1), other, 0));
        let ticket = m
            .handle()
            .admit(&request(P1, quarantined), 0)
            .expect("admitted");
        assert!(m.record_identity_mismatch(ticket, 0));
        assert_eq!(m.policy.address_entries(), 1, "the quarantine is recorded");
        drop(
            m.handle()
                .admit(&request(P1, other), 0)
                .expect("the peer's other book route is admitted"),
        );
        drop(
            m.handle()
                .admit(&request(P2, "/ip4/198.51.100.1/tcp/1"), 0)
                .expect("an address outside the book is admitted"),
        );
    }

    #[test]
    fn a_quarantine_leaves_the_book_only_into_unreserved_room() {
        // A live quarantine that leaves the book takes a slot the
        // outcome reservation may have promised an admitted dial; it
        // leaves only once that room is free (#137 re-review 7, A).
        let mut m = full_book_with_a_quarantine(1);
        // One live quarantine sits outside the book; a table of two
        // leaves exactly one slot, which the held dial is promised.
        m.policy.max_addresses = 2;
        let held = m
            .handle()
            .admit(&request(P2, "/ip4/198.51.100.1/tcp/1"), 0)
            .expect("admitted, its outcome reserving the one free slot");
        assert!(
            !m.learn_address(&peer(P1), "/ip4/203.0.113.7/tcp/1", 0),
            "the free slot is promised to the held dial: the quarantine stays"
        );
        drop(held);
        assert!(
            m.learn_address(&peer(P1), "/ip4/203.0.113.7/tcp/1", 0),
            "the control: the promise released, the quarantine hands over"
        );
        assert!(
            !m.policy
                .is_address_dialable(&peer(P1), "/ip4/198.51.100.3/tcp/1", 0),
            "and the table holds it"
        );
    }

    #[test]
    fn a_retirement_pass_leaves_a_quarantine_the_table_cannot_take() {
        fn synthetic(n: usize) -> TransportIdentity {
            let mut bytes = [0_u8; 38];
            bytes[..6].copy_from_slice(&[0x00, 0x24, 0x08, 0x01, 0x12, 0x20]);
            bytes[6..14].copy_from_slice(&(n as u64).to_be_bytes());
            TransportIdentity::parse(bs58::encode(bytes).into_string())
                .expect("a decodable synthetic identity")
        }
        let trusting = |p: &TransportIdentity| {
            TrustSources::new(
                PeerTrustPolicy::new([p.clone()]).expect("one peer"),
                InfrastructureSet::default(),
            )
        };
        let quarantined = "/ip4/198.51.100.3/tcp/1";
        let run = |table: usize| {
            let mut m = ConnectionManager::new(ConnectionPolicy::new(64, 64), 64);
            let first = synthetic(0);
            let _ = m.set_trust(trusting(&first), &[]);
            assert!(m.learn_address(&first, quarantined, 0));
            for address in [quarantined, "/ip4/203.0.113.9/tcp/1"] {
                let ticket = m
                    .handle()
                    .admit(
                        &DialRequest {
                            peer: Some(first.clone()),
                            address: address.to_owned(),
                            origin: DialOrigin::ConnectionManager,
                        },
                        0,
                    )
                    .expect("admitted");
                assert!(m.record_identity_mismatch(ticket, 0));
            }
            m.policy.max_addresses = table;
            // Enough later revocations to push `first` past the bound.
            for n in 1..=MAX_RETIRED_BOOK_PEERS + 1 {
                let _ = m.set_trust(trusting(&synthetic(n)), &[]);
                assert!(m.learn_address(&synthetic(n), "/ip4/10.0.0.1/tcp/1", 0));
            }
            let _ = m.set_trust(trusting(&synthetic(0xffff)), &[]);
            (
                m.known_addresses(&first),
                m.policy.is_address_dialable(&first, quarantined, 0),
                m.known_addresses(&synthetic(2)),
                m.retired.front() == Some(&first),
                m.retired.len(),
            )
        };
        assert_eq!(
            run(1),
            (1, false, 1, true, MAX_RETIRED_BOOK_PEERS + 1),
            "no room: the quarantined entry stays in the book, its peer at the \
             front for the next pass and outside the bound, so the next-oldest \
             retired peer keeps its routes"
        );
        assert_eq!(
            run(2),
            (0, false, 1, false, MAX_RETIRED_BOOK_PEERS),
            "the control: with room it leaves, and the table keeps its quarantine"
        );
    }

    #[test]
    fn a_full_book_of_failing_addresses_makes_way_for_a_fresh_one() {
        // A peer that moved leaves addresses nobody answers on; they fail
        // and are not quarantined (only an identity mismatch quarantines).
        // The control is `the_address_book_is_bounded_per_peer`: the same
        // full book with no failures refuses.
        let mut m = manager(8);
        // Each dial past the previous failure's backoff (at most five
        // minutes), so every one is admitted and fails.
        let mut now = 0;
        for i in 0..DEFAULT_MAX_ADDRESSES_PER_PEER {
            let address = format!("/ip4/198.51.100.{i}/tcp/1");
            assert!(m.learn_address(&peer(P1), &address, now));
            let ticket = m
                .handle()
                .admit(&request(P1, &address), now)
                .expect("admitted");
            m.record_failure(ticket, now);
            now += 400_000;
        }
        assert!(
            m.learn_address(&peer(P1), A2, now),
            "a failing entry makes way for the address the peer moved to"
        );
        assert_eq!(m.known_addresses(&peer(P1)), DEFAULT_MAX_ADDRESSES_PER_PEER);
        assert!(m.dial_candidates(&peer(P1), now).iter().any(|a| a == A2));
    }

    #[test]
    fn a_full_book_never_gives_up_the_peers_last_proven_route() {
        // A1 worked, then failed once; seven asserted addresses were never
        // dialled. The ninth assertion must not flush A1 (#137 re-review
        // F1), and untried entries are protected too: the book refuses.
        let mut m = manager(8);
        assert!(m.learn_address(&peer(P1), A1, 0));
        let worked = m.handle().admit(&request(P1, A1), 0).expect("admitted");
        drop(m.record_success(worked, 0));
        let failed = m.handle().admit(&request(P1, A1), 1_000).expect("admitted");
        m.record_failure(failed, 1_000);
        for i in 0..DEFAULT_MAX_ADDRESSES_PER_PEER - 1 {
            assert!(m.learn_address(&peer(P1), &format!("/ip4/198.51.100.{i}/tcp/1"), 2_000));
        }
        assert!(
            !m.learn_address(&peer(P1), A2, 3_000),
            "the most recently proven route is not displaced, and untried \
             entries are not either"
        );
        assert!(
            m.dial_candidates(&peer(P1), 400_000)
                .iter()
                .any(|a| a == A1)
        );
    }

    #[test]
    fn a_full_book_gives_up_its_most_failed_never_working_entry() {
        // Distinct failure counts, so the choice is pinned: the entry that
        // failed most goes, and the others stay.
        let mut m = manager(8);
        let mut now = 0;
        for i in 0..DEFAULT_MAX_ADDRESSES_PER_PEER {
            let address = format!("/ip4/198.51.100.{i}/tcp/1");
            assert!(m.learn_address(&peer(P1), &address, now));
            // Entry 5 fails three times, every other entry once.
            for _ in 0..if i == 5 { 3 } else { 1 } {
                let ticket = m
                    .handle()
                    .admit(&request(P1, &address), now)
                    .expect("admitted");
                m.record_failure(ticket, now);
                now += 400_000;
            }
        }
        assert!(m.learn_address(&peer(P1), A2, now));
        let candidates = m.dial_candidates(&peer(P1), now);
        assert!(
            !candidates.iter().any(|a| a == "/ip4/198.51.100.5/tcp/1"),
            "the most-failed entry made way: {candidates:?}"
        );
        assert!(candidates.iter().any(|a| a == "/ip4/198.51.100.4/tcp/1"));
    }

    /// Proves `address` for P1 at `at`, then fails it `failures` times,
    /// each past the previous failure's backoff; returns the time after.
    fn prove_then_fail(m: &mut ConnectionManager, address: &str, at: u64, failures: u32) -> u64 {
        let worked = m
            .handle()
            .admit(&request(P1, address), at)
            .expect("admitted");
        drop(m.record_success(worked, at));
        let mut now = at + 1_000;
        for _ in 0..failures {
            let failed = m
                .handle()
                .admit(&request(P1, address), now)
                .expect("admitted");
            m.record_failure(failed, now);
            now += 400_000;
        }
        now
    }

    #[test]
    fn among_routes_that_stopped_answering_the_oldest_proof_goes_first() {
        // A book of routes that each worked and then failed once, proven
        // at rising times, the OLDEST sorting FIRST by address: a victim
        // key blind to the success time takes the last of its equal
        // maxima, the largest address, so only the success time can make
        // the oldest proof the one that makes way (#137 re-review 6, F1;
        // C2: a dead full book still takes the moved peer's address).
        let mut m = manager(8);
        let mut now = 0;
        let addresses: Vec<String> = (0..DEFAULT_MAX_ADDRESSES_PER_PEER)
            .map(|i| format!("/ip4/192.0.2.{}/tcp/4001", 2 + i))
            .collect();
        for address in &addresses {
            assert!(m.learn_address(&peer(P1), address, now));
            now = prove_then_fail(&mut m, address, now, 1);
        }
        assert!(m.learn_address(&peer(P1), "/ip4/203.0.113.7/tcp/4001", now));
        let candidates = m.dial_candidates(&peer(P1), now + 400_000);
        assert!(
            !candidates.iter().any(|a| a == &addresses[0]),
            "the oldest proof made way: {candidates:?}"
        );
        assert!(
            candidates
                .iter()
                .any(|a| a == &addresses[addresses.len() - 1])
        );
    }

    #[test]
    fn a_failing_never_working_entry_goes_before_a_proven_one() {
        // A proven route failing twice, a newer proven route, and a
        // never-working entry failing once: the never-working one goes,
        // though it has failed less.
        let proven_old = "/ip4/192.0.2.1/tcp/4001";
        let proven_new = "/ip4/192.0.2.2/tcp/4001";
        let never = "/ip4/192.0.2.3/tcp/4001";
        let mut m = manager(8);
        assert!(m.learn_address(&peer(P1), proven_old, 0));
        let now = prove_then_fail(&mut m, proven_old, 0, 2);
        assert!(m.learn_address(&peer(P1), proven_new, now));
        let now = prove_then_fail(&mut m, proven_new, now, 0);
        assert!(m.learn_address(&peer(P1), never, now));
        let failed = m
            .handle()
            .admit(&request(P1, never), now)
            .expect("admitted");
        m.record_failure(failed, now);
        let now = now + 400_000;
        for i in 0..DEFAULT_MAX_ADDRESSES_PER_PEER - 3 {
            assert!(m.learn_address(&peer(P1), &format!("/ip4/198.51.100.{i}/tcp/1"), now));
        }
        assert!(m.learn_address(&peer(P1), "/ip4/203.0.113.7/tcp/4001", now));
        let candidates = m.dial_candidates(&peer(P1), now + 400_000);
        assert!(!candidates.iter().any(|a| a == never), "{candidates:?}");
        assert!(candidates.iter().any(|a| a == proven_old), "{candidates:?}");
    }

    #[test]
    fn a_route_that_stopped_answering_is_offered_after_a_fresh_one() {
        // ADR-0011 prefers RECENTLY authenticated-successful addresses:
        // A1 worked, then failed; A2 is new. The control is
        // `a_known_good_address_is_offered_first`, where the working
        // route has not failed since and stays first.
        let mut m = manager(8);
        assert!(m.learn_address(&peer(P1), A1, 0));
        let worked = m.handle().admit(&request(P1, A1), 0).expect("admitted");
        drop(m.record_success(worked, 0));
        let failed = m.handle().admit(&request(P1, A1), 1_000).expect("admitted");
        m.record_failure(failed, 1_000);
        assert!(m.learn_address(&peer(P1), A2, 2_000));
        assert_eq!(
            m.dial_candidates(&peer(P1), 400_000)
                .first()
                .map(String::as_str),
            Some(A2),
            "the address the peer moved to comes first"
        );
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

    /// The same invariant at the OTHER consumer, which also had no test.
    ///
    /// `authorizes_for` reads `names_application_destination` only for
    /// `ConnectivityInfrastructureOnly`; `DataPlaneTrusted` is `true`
    /// unconditionally. So revalidating an established connection to a
    /// trusted peer never consults the origin -- which is why moving
    /// `RelayCircuit` and `DcutrHolePunch` into the predicate cannot
    /// start tearing down a hole punch or circuit toward a trusted peer
    /// reached through infrastructure.
    ///
    /// The claim is stated in
    /// `DialOrigin::names_application_destination`'s note "at
    /// either site". This is the second site, and it is asserted here
    /// rather than inferred from the first: adding an origin check to
    /// the `DataPlaneTrusted` arm fails this test and not that one.
    #[test]
    fn a_trusted_peer_keeps_its_connection_under_every_origin() {
        let mut m = untrusting(8);
        let _ = m.set_trust(trusting(&[P1], &[]), &[]);
        let class = m.classify(&peer(P1));
        assert_eq!(class, ConnectionClass::DataPlaneTrusted);

        for origin in DialOrigin::ALL {
            assert!(
                m.authorizes_for(class, origin),
                "{origin:?} lost a TRUSTED peer's connection, so the origin's plane is \
                 being consulted where only the class should decide"
            );
        }
    }

    /// The OTHER half of the same "only" claim.
    ///
    /// `a_trusted_peer_keeps_its_connection_under_every_origin` pins the
    /// `DataPlaneTrusted` arm; without this one the `Unauthorized` arm
    /// is unpinned and a mutation survives. `authorizes` -- the
    /// single-argument wrapper the rest of the tests use -- hardcodes
    /// `DialOrigin::Manual`, and
    /// `Manual.names_application_destination()` is true, so
    /// rewriting `Unauthorized => false` as
    /// `=> !origin.names_application_destination()`
    /// leaves every existing assertion green while making the comment's
    /// "only" false. `ConnectionPolicy::admit`'s matching claim is
    /// pinned by `an_unauthorized_peer_is_refused_whatever_the_origin`;
    /// this is that test's counterpart at the second consumer.
    #[test]
    fn an_unauthorized_peer_keeps_nothing_under_any_origin() {
        let m = untrusting(8);
        let class = m.classify(&peer(P1));
        assert_eq!(class, ConnectionClass::Unauthorized);

        for origin in DialOrigin::ALL {
            assert!(
                !m.authorizes_for(class, origin),
                "{origin:?} RETAINED an unauthorized peer's connection, so the origin's \
                 plane is being consulted where the class alone must refuse"
            );
        }
    }

    #[test]
    fn an_infrastructure_peer_keeps_a_reachability_connection_and_loses_a_data_plane_one() {
        // ADR-0036's separation is an origin/class PAIR, and
        // `ConnectionPolicy::admit` has always decided it that way --
        // `tests/transport-contract/tests/stage2_exit_gate.rs` pins
        // every ORIGIN against the infrastructure-only class, and
        // `connection_policy`'s own tests pin the other two classes.
        // (That contract test derived its expectation from
        // `is_data_plane` until 2026-09-03, so "pins" overstated it;
        // it asserts a hardcoded table now.) Revalidating an
        // established connection with the data-plane-only predicate
        // ignored the pair, so a relay reservation or an AutoNAT probe
        // to an infrastructure peer completed its handshake and was
        // closed immediately -- admission permitted it and
        // establishment threw it away. A relay circuit or a DCUtR hole
        // punch to that peer belonged on the same list until Stage 11
        // step 2; both are refused at admission now, so neither
        // reaches establishment to be thrown away.
        //
        // WHAT THIS ASSERTS IS AGREEMENT, not correctness. `kept ==
        // !origin.names_application_destination()` is true by
        // construction of the function under test, which is
        // deliberate: the defect was the
        // two predicates disagreeing. Whether the classification itself
        // is right is the predicate's own pinning test, and SPIKE-004
        // found two origins on the wrong side of it (D1, D2).
        let mut m = untrusting(8);
        let _ = m.set_trust(trusting(&[], &[P1]), &[]);
        let class = m.classify(&peer(P1));
        assert_eq!(class, ConnectionClass::ConnectivityInfrastructureOnly);

        for origin in DialOrigin::ALL {
            let kept = m.authorizes_for(class, origin);
            assert_eq!(
                kept,
                !origin.names_application_destination(),
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

    #[test]
    fn a_locally_refused_dial_scores_neither_the_address_nor_the_peer() {
        // The outbound gate refuses a connection whose address the
        // quarantine suppresses, and libp2p reports that as
        // `DialError::Denied`. Settled as an ordinary failure it was
        // wrong twice over, both self-inflicted: the address score
        // extended the quarantine that caused the refusal, so a
        // suppression this node keeps re-testing could never lapse; and
        // the peer backoff riding with it advanced a TRUSTED peer over
        // one address this node declined to use, which is the
        // address-versus-peer separation ADR-0011 exists for.
        let mut m = manager(4);
        let quarantined = "/ip4/10.0.0.1/tcp/1";
        let good = "/ip4/10.0.0.2/tcp/1";

        let t = m
            .handle()
            .admit(&request(P1, quarantined), 0)
            .expect("admitted");
        assert!(m.record_identity_mismatch(t, 0), "the quarantine is set");

        // The gate now refuses a dial on that same route.
        let refused = m
            .handle()
            .admit(&request(P1, good), 1)
            .expect("a different route is admitted");
        m.record_locally_refused(refused, 1);

        assert_eq!(
            m.scheduled_retries(),
            0,
            "this node's own refusal schedules no retry"
        );
        // Bound, not dropped: a `DialTicket` settles itself on drop, so
        // an unbound one would return the slot before it is counted.
        let readmitted = m
            .handle()
            .admit(&request(P1, good), 2)
            .expect("the peer was not advanced into backoff by our own refusal");
        assert_eq!(
            m.handle().load().pending_dials(),
            1,
            "and the refused dial's slot came back, so this one fits"
        );
        drop(readmitted);
    }

    #[test]
    fn a_placeholder_rebinds_exactly_once_and_a_bound_address_never() {
        let mut m = manager(4);
        let mut t = m
            .handle()
            .admit(&request_at(P1, "", DialOrigin::KademliaQuery), 0)
            .expect("a trusted peer is admitted on the placeholder");
        assert!(
            !t.rebind_address(""),
            "the placeholder cannot rebind to itself"
        );
        assert!(t.rebind_address("/ip4/10.0.0.1/tcp/4001"));
        assert_eq!(t.address(), "/ip4/10.0.0.1/tcp/4001");
        assert!(
            !t.rebind_address("/ip4/10.0.0.2/tcp/4001"),
            "a bound address never rebinds"
        );
        assert_eq!(t.address(), "/ip4/10.0.0.1/tcp/4001", "and nothing moved");
        m.record_failure(t, 0);

        let mut ordinary = m
            .handle()
            .admit(&request(P2, "/ip4/10.0.0.9/tcp/1"), 0)
            .expect("admitted");
        assert!(
            !ordinary.rebind_address("/ip4/10.0.0.2/tcp/1"),
            "an ordinary ticket's address was decided at admission and may not move"
        );
        assert_eq!(ordinary.address(), "/ip4/10.0.0.9/tcp/1");
        m.record_failure(ordinary, 0);
    }

    #[test]
    fn address_dialable_answers_at_a_full_ceiling() {
        // F11: a probe through `admit` at a full ceiling was refused for
        // capacity the judged connection itself occupied, and the caller
        // had to enumerate which denials to discard. This API cannot see
        // capacity, so the answer is about the ADDRESS or it is wrong.
        let mut m = manager(1);
        let t = m
            .handle()
            .admit(&request(P1, "/ip4/10.0.0.1/tcp/1"), 0)
            .expect("admitted");
        assert!(
            m.record_identity_mismatch(t, 0),
            "the quarantine is recorded"
        );

        let held = m
            .handle()
            .admit(&request(P2, "/ip4/10.0.0.7/tcp/1"), 1)
            .expect("admitted");
        assert!(
            matches!(
                m.handle().admit(&request(P1, "/ip4/10.0.0.2/tcp/1"), 1),
                Err(DialDenial::TooManyPendingDials)
            ),
            "the control: the ceiling really is full while the reads below run"
        );
        let snapshot = m.handle().load();
        assert!(
            !snapshot.address_dialable(&peer(P1), "/ip4/10.0.0.1/tcp/1", 1),
            "the quarantined route is refused"
        );
        assert!(
            snapshot.address_dialable(&peer(P1), "/ip4/10.0.0.2/tcp/1", 1),
            "the same peer's OTHER address answers dialable at a full ceiling: \
             capacity cannot leak into an address answer"
        );
        drop(held);
    }

    #[test]
    fn an_unadmitted_failure_scores_the_route_without_a_reservation() {
        let mut m = manager(1);
        // Full for the duration: nothing below may take a reservation.
        let held = m
            .handle()
            .admit(&request(P2, "/ip4/10.0.0.7/tcp/1"), 0)
            .expect("admitted");
        let p = peer(P1);
        m.record_address_failure_unadmitted(&p, "", 0);
        assert_eq!(m.known_addresses(&p), 0, "an empty address is a no-op");
        for i in 0..8 {
            m.record_address_failure_unadmitted(&p, "/ip4/10.0.0.1/tcp/1", i);
        }
        assert_eq!(
            m.known_addresses(&p),
            1,
            "an attempted address is a candidate"
        );
        assert_eq!(
            m.scheduled_retries(),
            0,
            "the primary settlement owns the schedule; this API never touches it"
        );
        drop(held);
        assert!(
            matches!(
                m.handle().admit(&request(P1, "/ip4/10.0.0.1/tcp/1"), 10),
                Err(DialDenial::PeerBackoff)
            ),
            "the scoring reached the policy with no ticket ever minted"
        );
    }

    #[test]
    fn a_placeholder_failure_settles_without_scoring_anything() {
        let mut m = manager(4);
        let p = peer(P1);
        let t = m
            .handle()
            .admit(&request_at(P1, "", DialOrigin::KademliaQuery), 0)
            .expect("admitted");
        assert_eq!(m.handle().load().pending_dials(), 1);
        m.record_failure(t, 0);
        assert_eq!(m.known_addresses(&p), 0, "an empty address is not learned");
        assert_eq!(
            m.scheduled_retries(),
            0,
            "nothing to dial, nothing to retry"
        );
        assert_eq!(
            m.handle().load().pending_dials(),
            0,
            "but the slot is settled"
        );
        let readmitted = m
            .handle()
            .admit(&request(P1, "/ip4/10.0.0.1/tcp/1"), 1)
            .expect("no backoff was advanced by a route nobody can name");
        drop(readmitted);
    }

    #[test]
    fn an_unadmitted_permanent_failure_forgets_the_route() {
        let mut m = manager(4);
        let p = peer(P1);
        m.record_address_failure_unadmitted(&p, "/ip4/10.0.0.1/tcp/1", 0);
        m.record_address_failure_unadmitted(&p, "/ip4/10.0.0.2/tcp/2", 0);
        assert_eq!(m.known_addresses(&p), 2);
        m.record_permanent_address_failure_unadmitted(&p, "/ip4/10.0.0.1/tcp/1");
        assert_eq!(
            m.known_addresses(&p),
            1,
            "the structural route is forgotten; the other survives on its own merit"
        );
        assert_eq!(m.scheduled_retries(), 0, "nothing is ever scheduled here");
        m.record_permanent_address_failure_unadmitted(&p, "");
        assert_eq!(m.known_addresses(&p), 1, "an empty address is a no-op");
    }
    #[test]
    fn no_new_route_here_reaches_learn_address_unseen() {
        // THE PREMISE `interweave-transport-libp2p`'s CANONICALIZATION GUARD
        // RESTS ON, pinned in the crate that can actually break it.
        //
        // That guard counts production calls to `learn_address` and to every
        // other METHOD named in the table below -- including, since the commit
        // that wrote this sentence, `record_address_failure` at an expectation
        // of zero. That one is the POLICY's method rather than this type's and
        // was in no sibling table at all: `mod.rs` builds a `ConnectionPolicy`
        // in production, so a direct call over there was counted by neither
        // guard, which a review found behind an earlier version of this
        // sentence claiming otherwise. `.book` is the one entry below that the
        // sibling does not count, and it is a field rather than a method.
        //
        // No count in this sentence. The first version gave one and it went
        // stale; the second said "every other method" while the exception was
        // live; the third said "NOT ALL OF THEM" in the same commit that
        // removed the exception. Because `learn_route`
        // canonicalizes the address and a path that skips it splits the
        // `(peer, address)` key between the address book and the quarantine
        // map. Its route table is hand-maintained, in another crate, against
        // a comment -- so a NEW method added HERE is invisible to it, and it
        // would go on passing while a caller wrote a raw address into the
        // book. Four rounds of review found that table wrong in one
        // direction or another. Review finding on PR #86.
        //
        // THE DELEGATION PATTERNS ARE COUNTED TOO, which a reviewer measured
        // as the remaining hole: a new method that calls `record_failure` or
        // `record_address_failure_unadmitted` rather than `learn_address`
        // directly reaches it transitively, and counting only the direct
        // name left that invisible in both guards at once. Each of those two
        // appears exactly once here, as its own declaration, so a second
        // occurrence is a new caller.
        //
        // A ROUTE THAT ONLY REMOVES IS STILL A ROUTE, which is the fourth
        // round's finding. `record_permanent_address_failure_unadmitted`
        // reaches `learn_address` through nothing, so describing this set as
        // "what reaches `learn_address`" excluded it -- and it was dropped
        // from both tables on exactly that reasoning. But its body is a
        // BOOK removal (then `get_mut` and `remove`, now `hand_over`) keyed
        // by the caller's string. These guards exist so the book
        // and the quarantine map key one route ONE way, and a raw address
        // handed to that method does not mis-insert -- it fails to remove,
        // and the undialable route then holds one of `max_addresses_per_peer`
        // slots for the life of the process. So the set both guards enforce
        // is every method that keys the book or the quarantine, which is
        // WIDER than the set that reaches `learn_address`. Review finding on
        // PR #86.
        //
        // A TICKET IS A CALLER-SUPPLIED ADDRESS TOO, which a later round
        // found the table missing. `record_permanent_failure` removes from
        // the book by `ticket.address()`, `record_identity_mismatch` writes
        // the quarantine by it, and `record_success` scores it -- all three
        // exactly as ticket-carried as `record_failure`, which WAS in the
        // table. Saying "from a caller-supplied address" let them read as
        // out of scope because a ticket is not a `&str` argument; the
        // address inside it came from a caller all the same. What makes
        // those four safe is that a ticket's address is canonical before the
        // ticket exists. THERE ARE TWO ORIGINS FOR THAT, not one, and an
        // earlier version of this named only the first: `attempt_dial`
        // canonicalizes with `canonical_dial_address`, and
        // `OutboundAdmission`'s established hook rebinds the F9 placeholder
        // with `canonical_for_peer` -- which is the path EVERY
        // behaviour-originated dial takes, so it is the origin of most of the
        // tickets these three methods receive. Both are counted: the first in
        // the sibling guard's table, the second beside the hook itself.
        // Review findings on PR #86.
        //
        // THE BOOK IS KEYED FROM AN ADDRESS IN EXACTLY THREE PLACES HERE:
        // `learn_address`, `record_permanent_address_failure_unadmitted`
        // and `record_permanent_failure`, the last two through the one way
        // out, `hand_over`; the retirement pass removes too, through the
        // same door, but keys by the peer's class and takes no address. The quarantine is reached through
        // `policy.record_address_failure`, `policy.record_identity_mismatch`
        // and `policy.record_success`. All six are in the table below, as a
        // declaration or as an internal caller.
        //
        // `record_address_failure` WAS NOT, and the claim above was false
        // until an audit counted the patterns rather than the prose. Its two
        // internal callers were reached only transitively, through the
        // `record_failure` and `record_address_failure_unadmitted`
        // declarations -- so a new method whose whole body is
        // `self.policy.record_address_failure(peer, address, now_ms, delay)`
        // would write the quarantine from a raw caller string, change no
        // count in either guard, and pass. That is the silent pass both
        // guards exist to refuse. It is counted directly now, and
        // `record_address_failure(` is a substring of neither sibling
        // pattern, so the counts stay independent.
        //
        // AND THE BOOK ACCESSES THEMSELVES ARE COUNTED, because "keyed in
        // exactly three places" was a count with no mechanism: a fourth
        // `self.policy.book.get_mut(peer)` plus `known.remove(address)` changed
        // nothing either. The pattern is `.book` and not `self.policy.book`, which
        // a later round measured as a hole of its own: rustfmt breaks a long
        // chain between the receiver and the field, `dial_candidates` is
        // already wrapped that way -- the receiver on one line and the field
        // on the next -- and `self.policy.book` therefore counted six of the seven
        // accesses then, so a seventh written that way would have been
        // free. There are nine now: the `entry` in `learn_address`, the
        // reads in `dial_candidates` and `known_addresses`, a
        // `get_mut`/`remove` pair in each of the two removers, and the
        // `keys`/`remove` pair in `retire_unclassified_book_peers` (review
        // R3 on fa3eab8, #117 F3). Four of them key nothing by address;
        // they are
        // counted anyway, because the pattern is the FIELD rather than the
        // operation, and a guard that counted only writes would have to
        // parse the surrounding expression. Over-counting fails loudly.
        // Review findings on PR #86.
        //
        // Reads this file's own source, so it cannot see a call built by a
        // macro or reached through a trait object. It cuts at EVERY
        // file-level `#[cfg(test)] mod`, not the first -- dropping the tail
        // is the permissive direction when the expectation is a small
        // number, and the sibling guard had to be fixed for exactly that.
        // The two `#[cfg(test)]` non-module items above the test module stay
        // counted as production; they call none of these, and over-counting
        // fails loudly.
        let source = include_str!("connection_manager.rs");
        let mut production = String::new();
        let mut rest = source;
        while let Some((before, after)) = rest.split_once("\n#[cfg(test)]\nmod ") {
            production.push_str(before);
            // AN OUT-OF-LINE TEST MODULE IS REFUSED. `#[cfg(test)] mod tests;`
            // has no `{`, so the rest of the file would be swallowed as test
            // code.
            //
            // AND THAT IS A SILENT PASS, not the loud failure an earlier
            // version of this comment claimed on the reasoning that dropping
            // text can only lower a count. It cannot lower THESE counts: the
            // declaration sits where the test module sits, near the end, so
            // every existing call is in the text before it and every
            // expectation still matches. What the drop hides is whatever a
            // later commit adds BELOW the declaration -- measured, by
            // planting the out-of-line form plus a new method whose body is
            // `self.policy.book.get_mut(peer)`, `known.remove(address)` and
            // `self.policy.record_address_failure(..)`: every count unchanged,
            // guard green, a raw caller string reaching both the book and the
            // quarantine. This assertion is what closes that shape.
            //
            // NOTHING PINS THE ASSERTION ITSELF. Deleting it leaves the
            // production slice byte-identical and every count matching, so no
            // test in the tree goes red -- the measurement above was a planted
            // tree, not a suite. `dialing.rs` has meta-tests for its module
            // parser; there is no equivalent for this refusal in any of the
            // four guards, and saying so beats implying one.
            //
            // ONE CAVEAT, since the point is precision: deleting the `assert!`
            // alone leaves `head` bound and unused, which CI's
            // `clippy -- -D warnings` rejects. Deleting both lines is what
            // nothing catches. Review findings on PR #86.
            let head: &str = after.split_once('{').map_or(after, |(h, _)| h);
            assert!(
                !head.contains(';'),
                "`#[cfg(test)] mod <name>;` declares its tests in another file, and this \
                 guard cannot tell where they end -- so it refuses. Use an inline \
                 `mod tests {{ ... }}`, or extend this guard to follow the file."
            );
            // `"\n}"` rather than `"\n}\n"`: the surviving newline is the
            // separator the next search needs.
            if let Some((_, tail)) = after.split_once("\n}") {
                rest = tail;
            } else {
                assert!(
                    after.trim_end().ends_with('}'),
                    "a `#[cfg(test)] mod` here neither closes at column zero nor \
                     ends the file, so this guard cannot tell tests from production \
                     and refuses rather than guessing"
                );
                rest = "";
            }
        }
        production.push_str(rest);

        for (pattern, expected) in [
            // The declaration plus the three internal callers:
            // `record_failure`, `record_address_failure_unadmitted`, and
            // `record_relay_hop_unreached`, which learns `ticket.address()`
            // as `record_failure` does.
            ("learn_address(", 4usize),
            // Declaration only, ticket-carried like `record_failure`; a
            // second occurrence is a new transitive route.
            ("record_relay_hop_unreached(", 1),
            // Declarations only; each reaches `learn_address` internally, so
            // a second occurrence is a new transitive route.
            ("record_failure(", 1),
            ("record_address_failure_unadmitted(", 1),
            // Declaration only; reaches `learn_address` through nothing and
            // keys the book by removing from it instead.
            ("record_permanent_address_failure_unadmitted(", 1),
            // Ticket-carried, and keyed by `ticket.address()` all the same:
            // the book, the quarantine and the success score in that order.
            // `record_permanent_failure(` is not a substring of
            // `record_permanent_address_failure_unadmitted(` and does not
            // contain `record_failure(`, so the counts stay independent.
            ("record_permanent_failure(", 1),
            ("record_identity_mismatch(", 2),
            ("record_success(", 2),
            // The quarantine write itself, not only its two enclosing
            // declarations. Counted directly so a third caller fails.
            ("record_address_failure(", 2),
            // THE BOOK, so that "keyed in exactly three places" is a
            // mechanism rather than a sentence. `.book` and not `self.policy.book`,
            // because rustfmt wraps a long chain between the receiver and
            // the field and `dial_candidates` is wrapped that way already.
            // Eight: a read and an `entry` in `learn_address` (the victim is
            // chosen before the book is borrowed to change), a read in `dial_candidates`,
            // in `known_addresses` and in `hand_over`, and the `keys`/`get`/`contains_key` triple in
            // `retire_unclassified_book_peers` (review R3 on fa3eab8, #117
            // F3), which keys by the peer's CLASS and takes no address, so
            // it is not a route. It is a substring of no other pattern
            // here, and none of them contains it.
            (".book", 8),
            // Every way out of the book, through the one door that hands
            // a quarantine over or refuses (ADR-0011, amendment
            // 2026-09-28): `hand_over`'s declaration and its four callers
            // -- `learn_address`'s eviction, the retirement pass and the
            // two removers -- and its two calls into the policy.
            ("release_from_book(", 2),
            ("hand_over(", 5),
        ] {
            let calls = production.matches(pattern).count();
            assert_eq!(
                calls, expected,
                "this file holds `{pattern}` {calls} time(s), expected {expected}. \
                 If the count ROSE, a new path here keys the book or the quarantine \
                 from a caller-supplied address -- or, for `.book`, touches the book at \
                 all: add it to the route table in \
                 `no_production_path_learns_an_address_without_canonicalizing` \
                 (interweave-transport-libp2p) and raise the number here, or the \
                 address book and the quarantine map will key one route two ways. \
                 If it FELL, a route was removed or renamed: drop its expectation \
                 there and lower it here. If this is a TEST call, the module cut \
                 swallowed less than the whole module -- and note that this guard, \
                 alone of the four, does not require a column-zero `#[cfg(test)]` \
                 to be a module, because two non-module ones here stay counted as \
                 production. If it is a production DOC COMMENT, write the name \
                 without the parenthesis -- and for `.book`, which is the one pattern \
                 here that has no parenthesis to drop, write the field name without \
                 the dot."
            );
        }
    }
}
