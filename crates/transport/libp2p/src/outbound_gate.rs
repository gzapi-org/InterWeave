// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The other half of the dial gate: the one behaviours cannot walk past.
//!
//! # Two doors, and this one now answers with policy
//!
//! [`GatedSwarm`](crate::gated_swarm::GatedSwarm) closes the command
//! path: the raw `Swarm` is private, and `dial` needs an admission
//! ticket. That says nothing about dials a `NetworkBehaviour`
//! originates from inside the Swarm — Kademlia filling a bucket, the
//! AutoNAT server dialling back, Relay renewing a reservation. Those
//! never pass through the wrapper at all. (The AutoNAT CLIENT never
//! dials — pinned against the vendored source by
//! `tests/autonat_client_retest.rs`; the profile dials its servers on
//! the command path, under `AutonatProbe` — CLAUDE.md §1.)
//!
//! libp2p routes every dial, whatever asked for it, through
//! `NetworkBehaviour::handle_pending_outbound_connection`, and it does
//! so synchronously inside `Swarm::dial`. Until Stage 10 this behaviour
//! refused every dial without a ticket outright, which was correct
//! while nothing behaviour-originated dialled. Kademlia is the change
//! that makes something dial, so an unticketed dial is admitted through
//! the SAME root admission an ordinary dial passes —
//! `SnapshotHandle::admit` — and its ticket is deposited with the
//! runtime's in-flight set so the ordinary settlement path owns the
//! outcome. Trust, peer backoff, drain state and both ceilings all bind
//! (SPIKE-003 F1/F7/F8); nothing is admitted that the policy would
//! refuse, and nothing dials unaccounted.
//!
//! # Under WHICH origin is read, never inferred
//!
//! Stage 10 could infer it: Kademlia was the only dialling behaviour
//! compiled, so "no ticket" and "Kademlia" named the same set. Stage 11
//! adds three more and the inference then fails in the direction that
//! breaks the stack — `KademliaQuery` is data-plane, and a data-plane
//! origin toward a `ConnectivityInfrastructureOnly` peer is refused, so
//! every relay reservation and AutoNAT dial-back would be denied against
//! exactly the infrastructure the reachability stack exists to use.
//! SPIKE-004 measured that against a real relay client.
//!
//! So the origin comes from [`crate::attribution`]: a wrapper around
//! each dialling behaviour writes `ConnectionId -> DialOrigin` before
//! the Swarm acts on the dial, and this hook reads and consumes the
//! note. **A dial nobody claimed is refused** — fail-closed, and the
//! only signal that a dialling behaviour was added without a wrapper.
//!
//! # And every refusal is written down
//!
//! The Swarm DISCARDS the denial of a behaviour-originated dial:
//! `if let Ok(()) = self.dial(opts)` (libp2p-swarm 0.47.1
//! `lib.rs:1101`), so there is no `Dialing` and no
//! `OutgoingConnectionError`, and `ConnectionDenied`'s `Display` is the
//! bare string `connection denied`. A refusal not recorded here is
//! recorded nowhere, which is why every `Err` out of the pending hook
//! goes through [`OutboundAdmission::refuse`] and into
//! [`crate::refusals::DialRefusals`].
//!
//! # A dial that fails between the hook and the socket
//!
//! The ticket deposited at the pending hook is settled by the runtime
//! on `ConnectionEstablished` or `OutgoingConnectionError`, and for two
//! failures NEITHER arrives: the Swarm reports them to the behaviours
//! as `DialFailure` inside `Swarm::dial` and returns `Err`, which the
//! behaviour path discards (above). One is `DialError::Denied` from a
//! LATER field's pending hook -- step 4's dial-back target check is
//! such a field. The other is `DialError::NoAddresses`, raised AFTER
//! the hooks when every address the Swarm kept -- the dial's own, and
//! the behaviours' only when the dial extends through them
//! (`lib.rs:468-480`) -- was stripped as this node's own listener, or
//! there was none to keep (libp2p-swarm 0.47.1 `lib.rs:496-510`) --
//! reachable through Kademlia today, by a peer record that names this
//! node's own address, and each occurrence held a pending-dial slot
//! for the process's life. So
//! [`OutboundAdmission::on_swarm_event`] takes the ticket back on such
//! a `DialFailure` and drops it: `DialTicket::drop` releases both
//! reservations of an unsettled ticket, and this node's own decision
//! is not evidence about the network, so nothing is scored -- the same
//! answer `dialing.rs` gives a `Denied` that does reach a settlement.
//! Only a PLACEHOLDER ticket is taken: one re-bound at the established
//! hook belongs to a dial the pool accepted, whose failure arrives as
//! `OutgoingConnectionError` too. Both are written down as refusals.
//! `a_synchronous_failure_after_admission_releases_the_ticket` and its
//! control pin this; review of step 4's wrapper found the class.
//!
//! # What each hook can decide (F9)
//!
//! For a KADEMLIA dial libp2p calls the pending hook with an EMPTY
//! address list — the hook exists so each behaviour can contribute
//! addresses, and the union is dialled after it returns. So the
//! pending hook decides everything peer-scoped and global, on the
//! empty placeholder address, and the ADDRESS decision moves to
//! `handle_established_outbound_connection`, which is handed the
//! address the dial actually used: the ticket is re-bound to it
//! (F12), stripped of its `/p2p/` suffix first (F10), and the
//! quarantine is asked through the capacity-free
//! [`PolicySnapshot::address_dialable`] (F11). A quarantined route
//! therefore costs one TCP connect and is then refused — later than
//! the command path's check, and the only place a Kademlia dial has
//! one at all.
//!
//! **The empty list is Kademlia's, not every behaviour's.** SPIKE-004
//! measured a relay reservation and an AutoNAT dial-back each arriving
//! at this hook with ONE candidate address, so the slice is
//! origin-dependent and this module's `_addresses` parameter stops
//! being ignorable when Stage 11 adds those behaviours. It matters in
//! one direction especially: `AUTONAT.md` §7's dial-back restriction
//! is an SSRF check on a requester-chosen address, and deferring it to
//! the established hook means performing the connect the check exists
//! to prevent. The behaviour that adds those origins is the one that
//! must read the slice here; attribution (see [`crate::attribution`])
//! is what lets this hook tell which dial it is looking at.
//!
//! # What it is not
//!
//! Not a policy. The decisions are made by the root
//! [`PolicySnapshot::admit`] and
//! [`PolicySnapshot::address_dialable`]; this behaviour only makes
//! sure they are asked for THIS dial, at the moments they can be.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use libp2p::PeerId;
use libp2p::core::transport::PortUse;
use libp2p::core::{Endpoint, Multiaddr};
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, DialError, DialFailure, FromSwarm, NetworkBehaviour, THandler,
    THandlerInEvent, THandlerOutEvent, ToSwarm, dummy,
};

use interweave_transport_api::TransportIdentity;
use interweave_transport_runtime::{
    DialDenial, DialOrigin, DialRequest, DialTicket, SnapshotHandle,
};

use crate::attribution::DialAttribution;
use crate::refusals::{DialRefusals, Refusal};
use crate::runtime::canonical_for_peer;

/// What a refused behaviour dial is told when it names no peer.
const NO_PEER: &str = "a behaviour dial that names no peer cannot be classified";

/// A dial no behaviour claimed.
///
/// Reachable exactly two ways: a dialling behaviour was added without
/// an [`crate::Attributing`] wrapper, or a wrapper announced under a
/// different `ConnectionId` than the Swarm used. Both are bugs in this
/// crate rather than conditions a peer can provoke, and both fail
/// closed here.
const NO_ATTRIBUTION: &str = "a behaviour dial with no attribution cannot be \
     classified; the dialling behaviour is not wrapped";

/// The peer id is well-formed for libp2p and not for the neutral crates.
const NOT_NEUTRAL_IDENTITY: &str = "behaviour dial names an identity outside the neutral grammar";

/// The root admission said no.
const POLICY_REFUSED: &str = "the root admission refused this dial";

/// A later field's pending hook refused a dial this gate had admitted.
const DENIED_AFTER_ADMISSION: &str =
    "a behaviour's pending hook refused the dial after admission; the ticket is released";

/// The Swarm found nothing to dial after the hooks ran.
const NO_ADDRESSES_AFTER_ADMISSION: &str = "the Swarm had no address left to dial after \
     admission -- none was supplied, or every one supplied was this node's own listener; \
     the ticket is released";

/// Connection ids the root admission has issued a ticket for.
///
/// Shared between the wrapper that dials and the behaviour that
/// answers, because the two are the same decision seen from either end
/// of one synchronous call: `Swarm::dial` invokes the hook before it
/// returns, so an id is registered and consumed within a single
/// statement and the set is empty in between.
#[derive(Debug, Clone, Default)]
pub struct AdmittedDials {
    ids: Arc<Mutex<HashSet<ConnectionId>>>,
}

impl AdmittedDials {
    /// Announce that this connection id carries a valid admission.
    pub fn register(&self, id: ConnectionId) {
        self.lock().insert(id);
    }

    /// Consume the announcement, reporting whether there was one.
    pub fn take(&self, id: ConnectionId) -> bool {
        self.lock().remove(&id)
    }

    /// Drop an announcement that was never consumed.
    ///
    /// A dial libp2p refuses before reaching the hook leaves its id
    /// behind, and an id that stays is one a later dial could reuse
    /// without an admission. Cheap to call unconditionally, which is
    /// why the caller does.
    pub fn forget(&self, id: ConnectionId) {
        self.lock().remove(&id);
    }

    /// Ids currently announced. Zero everywhere except mid-dial.
    #[must_use]
    pub fn outstanding(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashSet<ConnectionId>> {
        // Poisoning is recovered rather than propagated: the protected
        // value is a set of ids with no invariant spanning two
        // operations, so a panic elsewhere must not turn every future
        // dial into a denial.
        self.ids.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Tickets for dials the Swarm has accepted and not yet reported on.
///
/// Keyed by the connection id the dial was built with, which is what
/// every outcome event carries back. Shared — the same `Arc` pattern as
/// [`AdmittedDials`] — because two parties genuinely hold it: the
/// runtime loop deposits and settles ordinary dials, and the gate
/// deposits behaviour dials in its pending hook and re-binds them in
/// its established hook. Bounded by `max_pending_dials`: every ticket
/// in here holds a pending-dial reservation, so the admission that
/// minted it already enforced the ceiling.
#[derive(Debug, Clone, Default)]
pub struct InFlightTickets {
    inner: Arc<Mutex<HashMap<ConnectionId, DialTicket>>>,
}

impl InFlightTickets {
    /// File a ticket under the connection that will settle it.
    pub fn deposit(&self, id: ConnectionId, ticket: DialTicket) {
        self.lock().insert(id, ticket);
    }

    /// Take the ticket a settlement owns, if this dial was ours.
    #[must_use]
    pub fn settle(&self, id: ConnectionId) -> Option<DialTicket> {
        self.lock().remove(&id)
    }

    /// Re-bind a PLACEHOLDER ticket to the address its dial used,
    /// returning the admitted peer when this was such a ticket.
    ///
    /// `None` for a connection that is not ours, and for an ordinary
    /// admitted dial — its address was decided at admission and
    /// [`DialTicket::rebind_address`] refuses to move it, so the
    /// established hook leaves it entirely alone.
    #[must_use]
    pub fn rebind_placeholder(&self, id: ConnectionId, used: &str) -> Option<TransportIdentity> {
        let mut held = self.lock();
        let ticket = held.get_mut(&id)?;
        if !ticket.rebind_address(used) {
            return None;
        }
        ticket.peer().cloned()
    }

    /// Take back a PLACEHOLDER ticket whose dial failed before the
    /// pool accepted it, so no settlement will ever arrive for it.
    ///
    /// `None` for a connection that is not ours and for a ticket
    /// already re-bound -- that dial reached the established hook, so
    /// the pool had it and its failure is reported as
    /// `OutgoingConnectionError` as well; taking it here would settle
    /// it twice.
    #[must_use]
    pub fn take_placeholder(&self, id: ConnectionId) -> Option<DialTicket> {
        let mut held = self.lock();
        if held.get(&id)?.address().is_empty() {
            held.remove(&id)
        } else {
            None
        }
    }

    /// Dials currently in flight.
    #[must_use]
    pub fn outstanding(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ConnectionId, DialTicket>> {
        // Recovered for the same reason as [`AdmittedDials::lock`]: no
        // invariant spans two operations on the map itself.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Strip TRAILING `/p2p/<peer>` components from an address before it
/// is scored (SPIKE-003's F10).
///
/// Only the TRAILING components go: a `/p2p/` in the middle of the
/// address is a relay path's inner hop, part of the route rather than
/// a claim about who answers.
///
/// A behaviour dial's address arrives with the peer appended — a query
/// result carries it — while the address book and the quarantine map
/// are keyed by whatever the ticket carries. Passing the suffixed form
/// to the policy LOOKED UP an address it had never seen, so every
/// quarantine silently MISSED.
///
/// PAST TENSE DELIBERATELY, and it is the third stale sentence found in
/// this one doc comment. It described F10 in the present tense in the
/// same doc comment as the `**FIXED.**` paragraph below, which says no
/// production path can hand the policy a suffixed form any more —
/// `attempt_dial` and `learn_route` both canonicalize and the established
/// hook uses `canonical_for_peer` — so a reader who stopped here took it for a
/// description of today's code. CLAUDE.md §7 names exactly this: "X is
/// answered Y" is false as soon as it is fixed, often in the same commit
/// series. Review finding on PR #86.
///
/// **`AdmittedDial` does NOT bind the bare address**, and this comment
/// said it did until PR #74's review. It binds `ticket.address()`
/// verbatim. `attempt_dial` USED to copy the caller's string into the
/// `DialRequest` unchanged; since PR #86 it canonicalizes first, so the
/// sentence that followed here was false for two commits.
///
/// The BEHAVIOUR path is canonicalized at the established hook below by
/// `runtime::dialing::canonical_for_peer`, which wraps
/// [`strip_own_suffix`] and adds the empty-result arm — NOT by this
/// function, whose only PRODUCTION call site is `settle_failed_dial`'s
/// peerless arm (the tests below call it directly), itself unreachable
/// through admission: a ticket naming no peer is refused
/// (`a_dial_that_names_no_peer_is_never_admitted`) and a named one
/// always parses
/// (`every_identity_the_neutral_grammar_accepts_libp2p_accepts`). So
/// this function runs in no production path at all today — those two
/// tests are what would say so if either premise stopped holding.
///
/// That paragraph said `strip_own_suffix` was called "at its call site
/// below" until a SECOND review of this same paragraph. The established
/// hook stopped calling it directly two commits earlier — it calls
/// `canonical_for_peer` now — so `strip_own_suffix` has no call site in
/// this file at all and is reached only through that wrapper. One
/// paragraph, corrected twice, for two different stale sentences: the
/// §7 shape is that the reasoning is right in the file you are editing
/// and its counterpart is a line you did not re-read. Review finding on
/// PR #86.
///
/// The COMMAND and SCHEDULER paths USED to be stripped by neither, so
/// one physical route reached both ways occupied two `(peer, address)`
/// entries: a quarantine earned on one did not suppress the other, and
/// both spent `max_addresses` in a map whose bound is the point. That
/// was F10's failure mode on the command path, recorded on PR #74 as a
/// deferred follow-up because stripping in `attempt_dial` changes what
/// admission and quarantine are keyed by -- a security boundary, and a
/// different change from resolving D1/D2/D3.
///
/// **FIXED.** `attempt_dial` now canonicalizes before it builds the
/// `DialRequest`, so all three paths agree on the key; see
/// `runtime::dialing::canonical_dial_address` for what it strips and
/// the four things it deliberately leaves alone. Every `attempt_dial`
/// caller goes through it, which is why that is the boundary rather
/// than each call site.
#[must_use]
pub fn strip_peer_suffix(address: &Multiaddr) -> String {
    let mut parts: Vec<_> = address.iter().collect();
    while matches!(parts.last(), Some(libp2p::multiaddr::Protocol::P2p(_))) {
        parts.pop();
    }
    parts.into_iter().collect::<Multiaddr>().to_string()
}

/// Strip trailing `/p2p/` components ONLY while they name `peer`.
///
/// The identity-checked variant for a connection whose peer is already
/// authenticated: a trailing claim that names someone else is the
/// address contradicting the connection, and stripping it would launder
/// the contradiction into the bare route — the policy would then score
/// and quarantine an address string the observation never honestly
/// described. The foreign claim stays in the key instead, so whatever
/// the policy records is recorded against the literal that lied.
#[must_use]
pub fn strip_own_suffix(address: &Multiaddr, peer: &PeerId) -> String {
    let mut parts: Vec<_> = address.iter().collect();
    while matches!(parts.last(), Some(libp2p::multiaddr::Protocol::P2p(claimed)) if claimed == peer)
    {
        parts.pop();
    }
    parts.into_iter().collect::<Multiaddr>().to_string()
}

/// Admits every outbound dial: ticketed dials by their ticket,
/// behaviour dials through the root policy.
#[derive(Debug)]
pub struct OutboundAdmission {
    admitted: AdmittedDials,
    /// Which behaviour asked, written by [`crate::Attributing`] before
    /// the Swarm acts on the dial.
    ///
    /// Read and CONSUMED here. A dial with no note is refused: nothing
    /// claimed it, so nothing can classify it, and guessing is what
    /// SPIKE-004 measured breaking the reachability stack.
    attribution: DialAttribution,
    /// Refusals this gate made, because nothing downstream records
    /// them — see [`crate::refusals`].
    refusals: DialRefusals,
    /// The root admission, through the non-blocking handle: this runs
    /// synchronously inside the Swarm poll, and ADR-0011 forbids the
    /// gate blocking on the policy there. The handle also retries a
    /// `PolicySuperseded` refusal, so a trust revision landing mid-dial
    /// is a reload rather than a spurious denial.
    admission: SnapshotHandle,
    /// Where an admitted behaviour dial's ticket goes, so the ordinary
    /// settlement path owns its outcome (F7/F8).
    in_flight: InFlightTickets,
    /// The SAME clock origin the runtime hands the policy. A second
    /// origin — or a frozen one — would timestamp admissions on a
    /// different axis than settlements: SPIKE-003's F8b measured the
    /// frozen version making every backoff permanent.
    started: tokio::time::Instant,
}

impl OutboundAdmission {
    /// Build the gate over the root admission.
    #[must_use]
    pub fn new(
        admission: SnapshotHandle,
        in_flight: InFlightTickets,
        attribution: DialAttribution,
        started: tokio::time::Instant,
    ) -> Self {
        Self {
            admitted: AdmittedDials::default(),
            attribution,
            refusals: DialRefusals::default(),
            admission,
            in_flight,
            started,
        }
    }

    /// A handle to the set this behaviour consults.
    #[must_use]
    pub fn admitted(&self) -> AdmittedDials {
        self.admitted.clone()
    }

    /// A handle to the attribution map this gate reads.
    ///
    /// Symmetric with [`Self::admitted`]: the composed behaviour's
    /// wrappers write here and this gate consumes.
    #[must_use]
    pub fn attribution(&self) -> DialAttribution {
        self.attribution.clone()
    }

    /// A handle to this gate's refusal record.
    ///
    /// The only place a denied behaviour dial is reported: the Swarm
    /// discards the denial, so a caller that wants to know reads this.
    #[must_use]
    pub fn refusals(&self) -> DialRefusals {
        self.refusals.clone()
    }

    /// Refuse, recording the refusal first.
    ///
    /// Every `Err` out of the pending hook goes through here, so
    /// "the gate records its own refusals" is one path rather than a
    /// discipline applied at four return sites.
    fn refuse(
        &self,
        origin: Option<DialOrigin>,
        denial: Option<DialDenial>,
        detail: &'static str,
        rendered: String,
    ) -> ConnectionDenied {
        self.refusals.record(Refusal {
            origin,
            denial,
            detail,
        });
        ConnectionDenied::new(std::io::Error::other(rendered))
    }

    /// Milliseconds since the runtime's clock origin.
    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

impl NetworkBehaviour for OutboundAdmission {
    type ConnectionHandler = dummy::ConnectionHandler;
    type ToSwarm = std::convert::Infallible;

    /// Every outbound dial, whoever asked for it.
    ///
    /// # Errors
    /// [`ConnectionDenied`] carrying the policy's own refusal when the
    /// root admission denies a behaviour-originated dial, or the
    /// no-peer refusal when there is no identity to classify.
    fn handle_pending_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: Option<PeerId>,
        _addresses: &[Multiaddr],
        _effective_role: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        if self.admitted.take(connection_id) {
            // No extra addresses: the admitted ones are bound into the
            // DialOpts by `AdmittedDial`, and adding any here would be
            // this behaviour deciding where an admitted dial goes.
            return Ok(Vec::new());
        }
        // NO TICKET means no root admission issued this dial, which is
        // the definition of behaviour-originated. WHICH behaviour is
        // read from the note the wrapper left, never inferred: Stage 10
        // could infer Kademlia because it was the only dialling
        // behaviour compiled, and SPIKE-004 measured what that
        // inference does to a relay reservation once there are four.
        //
        // An unattributed dial is refused. That is fail-closed and it
        // is also the signal that a dialling behaviour was added
        // without an `Attributing` wrapper, which nothing else would
        // report.
        let Some(origin) = self.attribution.resolve(connection_id) else {
            return Err(self.refuse(None, None, NO_ATTRIBUTION, NO_ATTRIBUTION.to_owned()));
        };
        let Some(peer) = peer else {
            return Err(self.refuse(Some(origin), None, NO_PEER, NO_PEER.to_owned()));
        };
        let Ok(identity) = TransportIdentity::parse(peer.to_base58()) else {
            return Err(self.refuse(
                Some(origin),
                None,
                NOT_NEUTRAL_IDENTITY,
                format!("behaviour dial names an identity outside the neutral grammar: {peer}"),
            ));
        };
        // THE EMPTY PLACEHOLDER, deliberately (F9): a Kademlia dial
        // reaches this hook with no addresses, so peer-scoped and
        // global policy — trust, backoff, drain, both ceilings — are
        // decided here, and the address decision waits for the
        // established hook, where an address exists.
        //
        // `_addresses` is still ignored, and now that is a CHOICE
        // rather than an inheritance: attribution names the origin, so
        // this hook could branch on it, and Kademlia — the only
        // behaviour wired to dial today — supplies nothing to branch
        // on. SPIKE-004 measured a relay reservation and an AutoNAT
        // dial-back arriving here with one candidate each, so the
        // behaviour that reads the slice is the one that adds them:
        // `AUTONAT.md` §7's dial-back restriction is an SSRF check on a
        // requester-chosen address and cannot wait for the established
        // hook, which runs after the connect it exists to prevent.
        //
        // F16's cost is accepted knowingly: the reservation cannot be
        // separated from the admission, so an address table full of
        // live quarantines refuses behaviour dials outright, which is
        // fail-closed.
        let request = DialRequest {
            peer: Some(identity),
            address: String::new(),
            origin,
        };
        match self.admission.admit(&request, self.now_ms()) {
            Ok(ticket) => {
                // DEPOSITED, not dropped: the ticket holds the pending
                // and connection reservations, and the runtime's
                // ordinary settlement path releases them when the
                // outcome event arrives (F7/F8).
                self.in_flight.deposit(connection_id, ticket);
                Ok(Vec::new())
            }
            Err(denial) => Err(self.refuse(
                Some(origin),
                Some(denial),
                POLICY_REFUSED,
                format!("{origin:?} dial refused: {denial:?}"),
            )),
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

    /// The ADDRESS decision the pending hook could not make (F9).
    ///
    /// Only a behaviour dial is judged here: an ordinary admitted dial
    /// bound its address into the `DialOpts` and was decided before it
    /// was made, and `rebind_placeholder` refuses to touch its ticket.
    /// The ticket is re-bound FIRST (F12), so whichever way this hook
    /// answers, the settlement that follows scores the address the
    /// dial actually used rather than the placeholder.
    ///
    /// # Errors
    /// [`ConnectionDenied`] when the address this dial actually used is
    /// one the policy suppresses. The refusal surfaces as an
    /// `OutgoingConnectionError`, and the re-bound ticket settles
    /// against the real address there.
    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        _role_override: Endpoint,
        _port_use: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        // THROUGH THE SHARED KEY, not the raw helper. This was a third
        // implementation of the `(peer, address)` key -- the string it
        // produces is written into the ticket by `rebind_placeholder` and
        // is then the key for every settlement and for `learn_route` --
        // and it differed from `canonical_for_peer` on the one input the
        // other two had already been reconciled over. A review found it
        // while checking the claim that there was only one
        // implementation. Review finding on PR #86.
        let used = canonical_for_peer(addr, &peer);
        let Some(peer) = self.in_flight.rebind_placeholder(connection_id, &used) else {
            return Ok(dummy::ConnectionHandler);
        };
        // CAPACITY-FREE, by construction (F11): `address_dialable`
        // reads the quarantine and nothing else, so there is no
        // capacity denial to discard — the probe-through-admit version
        // was refused at a full ceiling for the very slot this
        // connection occupies.
        if self
            .admission
            .load()
            .address_dialable(&peer, &used, self.now_ms())
        {
            Ok(dummy::ConnectionHandler)
        } else {
            // One TCP connect was spent learning this (F9's stated
            // cost); the connection is refused before a handler exists.
            Err(ConnectionDenied::new(std::io::Error::other(format!(
                "kademlia connection refused on its address: {used} is quarantined"
            ))))
        }
    }

    /// A dial that failed inside `Swarm::dial`, after this hook admitted
    /// it, gets no settlement from the runtime; its ticket is released
    /// here (module note).
    fn on_swarm_event(&mut self, event: FromSwarm<'_>) {
        let FromSwarm::DialFailure(DialFailure {
            connection_id,
            error,
            ..
        }) = event
        else {
            return;
        };
        let detail = match error {
            DialError::Denied { .. } => DENIED_AFTER_ADMISSION,
            DialError::NoAddresses => NO_ADDRESSES_AFTER_ADMISSION,
            _ => return,
        };
        let Some(ticket) = self.in_flight.take_placeholder(connection_id) else {
            return;
        };
        self.refusals.record_release(Refusal {
            origin: Some(ticket.origin()),
            denial: None,
            detail,
        });
        // DROPPED, not settled: `DialTicket::drop` returns the pending
        // and connection reservations of an unsettled ticket, and this
        // node's own refusal scores no address and no peer.
        drop(ticket);
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::refusals::RECENT_CAPACITY;
    use interweave_transport_runtime::{ConnectionManager, ConnectionPolicy, TrustSources};
    use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
    use libp2p::swarm::ConnectionId;

    const TRUSTED: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

    fn ident(s: &str) -> TransportIdentity {
        TransportIdentity::parse(s).expect("valid identity")
    }

    fn manager(trusted: &[&str]) -> ConnectionManager {
        let mut m = ConnectionManager::new(ConnectionPolicy::new(8, 8), 8);
        let _ = m.set_trust(
            TrustSources::new(
                PeerTrustPolicy::new(trusted.iter().map(|p| ident(p))).expect("small"),
                InfrastructureSet::default(),
            ),
            &[],
        );
        m
    }

    fn gate(m: &ConnectionManager) -> (OutboundAdmission, InFlightTickets) {
        let (gate, in_flight, _) = attributed_gate(m);
        (gate, in_flight)
    }

    /// The gate with its attribution map, for tests that need to say
    /// which behaviour asked.
    fn attributed_gate(
        m: &ConnectionManager,
    ) -> (OutboundAdmission, InFlightTickets, DialAttribution) {
        let in_flight = InFlightTickets::default();
        let attribution = DialAttribution::default();
        (
            OutboundAdmission::new(
                m.handle(),
                in_flight.clone(),
                attribution.clone(),
                tokio::time::Instant::now(),
            ),
            in_flight,
            attribution,
        )
    }

    /// A behaviour dial, ANNOUNCED the way `Attributing` announces one.
    ///
    /// The announcement is not optional dressing: the gate refuses a
    /// dial nobody claimed, so a test that skipped this would measure
    /// the refusal path and call it the admission path.
    fn behaviour_dial(
        gate: &mut OutboundAdmission,
        id: usize,
        peer: &str,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        gate.attribution()
            .announce(ConnectionId::new_unchecked(id), DialOrigin::KademliaQuery);
        gate.handle_pending_outbound_connection(
            ConnectionId::new_unchecked(id),
            Some(peer.parse().expect("valid PeerId")),
            &[],
            Endpoint::Dialer,
        )
    }

    fn established(
        gate: &mut OutboundAdmission,
        id: usize,
        peer: &str,
        addr: &str,
    ) -> Result<THandler<OutboundAdmission>, ConnectionDenied> {
        gate.handle_established_outbound_connection(
            ConnectionId::new_unchecked(id),
            peer.parse().expect("valid PeerId"),
            &addr.parse::<Multiaddr>().expect("valid multiaddr"),
            Endpoint::Dialer,
            PortUse::Reuse,
        )
    }

    #[test]
    fn the_established_hook_still_canonicalizes_the_rebound_address() {
        // THE SECOND TICKET ORIGIN, and the one the runtime module's guard
        // cannot see.
        //
        // `attempt_dial` canonicalizes with `canonical_dial_address`, and
        // `no_production_path_learns_an_address_without_canonicalizing`
        // counts that. But EVERY behaviour-originated dial is admitted with
        // the F9 placeholder instead and gets its address here, at the
        // established hook, from `canonical_for_peer` -- so this file is the
        // origin of most of the tickets the settlement recorders receive, and
        // it is not in that guard's scan, which covers `src/runtime/` only.
        //
        // An audit measured what happens without this: reverting the hook to
        // the raw `strip_own_suffix` leaves all 202 lib tests passing. Clippy
        // does catch it today, but only incidentally -- `canonical_for_peer`
        // becomes an unused import -- and that evaporates the moment anything
        // else in this file uses it. An incidental lint is not a guard.
        //
        // IT CUTS AT EVERY FILE-LEVEL TEST MODULE, not the first, and refuses
        // the two shapes it cannot read. A first version of this guard was a
        // bare `split_once`, which a review measured as a silent pass of its
        // own: Rust's conventional layout puts new code BELOW the test
        // module, and a second production `canonical_for_peer(` written there
        // is not in `production` at all, so the count stays 1 and the guard
        // agrees. The three protections are the sibling guard's, which was
        // fixed for each of them in turn -- an out-of-line
        // `#[cfg(test)] mod tests;` swallowing the rest of the file, a module
        // that runs to the end of the file with no column-zero closing brace,
        // and a column-zero `#[cfg(test)]` on something that is not a module.
        //
        // THE SECOND IS NARROWER THAN IT SOUNDS, said here rather than left
        // to be discovered: the assertion fires only when NO `"\n}"` follows
        // the module at all. A module closing at an indent with any later
        // column-zero `}` takes the other branch and cuts at that brace
        // instead, dropping whatever lies between -- production code
        // included -- with nothing raised. rustfmt does not produce that
        // shape, which is why it is a stated limit and not a fourth check.
        //
        // Reads this file's own source, so it cannot see a call built by a
        // macro, and it checks the count rather than the argument. Review
        // findings on PR #86.
        let source = include_str!("outbound_gate.rs");
        let mut production = String::new();
        let mut rest = source;
        while let Some((before, after)) = rest.split_once("\n#[cfg(test)]\nmod ") {
            production.push_str(before);
            let head: &str = after.split_once('{').map_or(after, |(h, _)| h);
            assert!(
                !head.contains(';'),
                "`#[cfg(test)] mod <name>;` declares its tests in another file, and this \
                 guard cannot tell where they end -- so it refuses. Use an inline \
                 `mod tests {{ ... }}`, or extend this guard to follow the file."
            );
            // `"\n}"` and not `"\n}\n"`: the surviving newline is the
            // separator the next search needs.
            match after.split_once("\n}") {
                Some((_, tail)) => rest = tail,
                None => {
                    assert!(
                        after.trim_end().ends_with('}'),
                        "a `#[cfg(test)] mod` here neither closes at column zero nor ends \
                         the file, so this guard cannot tell tests from production and \
                         refuses rather than guessing"
                    );
                    rest = "";
                }
            }
        }
        production.push_str(rest);
        for (i, _) in source.match_indices("\n#[cfg(test)]") {
            let after = &source[i + "\n#[cfg(test)]".len()..];
            assert!(
                after.starts_with("\nmod "),
                "a column-zero `#[cfg(test)]` that is not immediately followed by `mod ` \
                 -- this guard cannot tell where the test code ends, so it refuses rather \
                 than reading past it. Move a test-only `use`, `const` or `fn` inside the \
                 test module."
            );
        }
        assert_eq!(
            production.matches("canonical_for_peer(").count(),
            1,
            "the established hook must key the ticket through \
             `canonical_for_peer`, which is the shared spelling the book, \
             the quarantine and the ticket agree on. A raw `strip_own_suffix` \
             here answers `\"\"` for an address that is only the peer's own \
             suffix, and an empty replacement is one `rebind_address` refuses \
             -- so `rebind_placeholder` answers `None` and the hook returns a \
             handler BEFORE the `address_dialable` check, skipping the \
             quarantine lookup this hook exists for. `record_failure`'s own \
             early return on an empty address is the second-order effect, not \
             the first. If a second legitimate call site was added, raise this \
             count and say which."
        );
    }

    #[test]
    fn a_dial_no_behaviour_claimed_is_refused() {
        // THE FAIL-CLOSED INVARIANT, and the replacement for a guard
        // that used to parse the root manifest.
        //
        // That guard asserted `autonat`, `relay` and `dcutr` were off,
        // because the hook ASSUMED every unticketed dial was Kademlia's
        // and the assumption dies when a second dialling behaviour
        // appears. The hook no longer assumes: it reads the note
        // `Attributing` leaves, and a dial with no note is refused
        // here. So the condition that guard protected is now enforced
        // by the code rather than by a feature list, and the failure
        // mode it feared — a relay reservation admitted as a
        // data-plane Kademlia query — cannot occur without an explicit
        // announcement saying so.
        //
        // What remains true is that a dialling behaviour must be
        // WRAPPED. That is what this test pins: forget the wrapper and
        // every dial that behaviour makes is refused, loudly and in the
        // refusal record, rather than misclassified in silence.
        let m = manager(&[TRUSTED]);
        let (mut gate, in_flight, _attribution) = attributed_gate(&m);

        let refused = gate.handle_pending_outbound_connection(
            ConnectionId::new_unchecked(1),
            Some(TRUSTED.parse().expect("valid PeerId")),
            &[],
            Endpoint::Dialer,
        );
        assert!(
            refused.is_err(),
            "an unattributed dial must be refused even for a fully trusted peer"
        );
        assert_eq!(
            in_flight.outstanding(),
            0,
            "and must reserve nothing, or a refusal would leak a pending-dial slot"
        );

        // THE CONTROL: the same peer, the same gate, one announcement.
        gate.attribution()
            .announce(ConnectionId::new_unchecked(2), DialOrigin::KademliaQuery);
        assert!(
            gate.handle_pending_outbound_connection(
                ConnectionId::new_unchecked(2),
                Some(TRUSTED.parse().expect("valid PeerId")),
                &[],
                Endpoint::Dialer,
            )
            .is_ok(),
            "so the refusal above is the missing attribution and not the peer or the policy"
        );
    }

    #[test]
    fn the_gate_admits_under_the_origin_it_was_told() {
        // The point of the mechanism, stated as the difference it
        // makes: one peer, authorized for REACHABILITY only, and two
        // dials that differ in nothing but the announced origin.
        //
        // Under the old hook both were `KademliaQuery` — data-plane —
        // and both were refused. SPIKE-004 measured exactly that
        // against a real relay client, which is why this is step one of
        // Stage 11 rather than a refinement of it.
        let mut m = ConnectionManager::new(ConnectionPolicy::new(8, 8), 8);
        let _ = m.set_trust(
            TrustSources::new(
                PeerTrustPolicy::new([]).expect("empty"),
                InfrastructureSet::new([ident(TRUSTED)]).expect("small"),
            ),
            &[],
        );
        let (mut gate, _in_flight, _a) = attributed_gate(&m);

        gate.attribution()
            .announce(ConnectionId::new_unchecked(1), DialOrigin::RelayReservation);
        assert!(
            gate.handle_pending_outbound_connection(
                ConnectionId::new_unchecked(1),
                Some(TRUSTED.parse().expect("valid PeerId")),
                &[],
                Endpoint::Dialer,
            )
            .is_ok(),
            "a reservation with an infrastructure-only peer is what that class exists for"
        );

        gate.attribution()
            .announce(ConnectionId::new_unchecked(2), DialOrigin::KademliaQuery);
        assert!(
            gate.handle_pending_outbound_connection(
                ConnectionId::new_unchecked(2),
                Some(TRUSTED.parse().expect("valid PeerId")),
                &[],
                Endpoint::Dialer,
            )
            .is_err(),
            "and an origin that names an application destination is refused toward the \
             same peer — the class split is decided by the origin the gate was TOLD"
        );
    }

    #[test]
    fn a_note_is_consumed_so_it_cannot_attribute_the_next_dial() {
        // A `ConnectionId` names one dial attempt. libp2p reuses ids
        // across a Swarm's lifetime, so a note that outlived its dial
        // would classify whatever came next — and the origin it carried
        // would be one nobody announced for that dial.
        let m = manager(&[TRUSTED]);
        let (mut gate, _in_flight, attribution) = attributed_gate(&m);

        attribution.announce(ConnectionId::new_unchecked(7), DialOrigin::KademliaQuery);
        assert_eq!(attribution.outstanding(), 1);
        assert!(
            gate.handle_pending_outbound_connection(
                ConnectionId::new_unchecked(7),
                Some(TRUSTED.parse().expect("valid PeerId")),
                &[],
                Endpoint::Dialer,
            )
            .is_ok()
        );
        assert_eq!(attribution.outstanding(), 0, "reading the note consumes it");

        assert!(
            gate.handle_pending_outbound_connection(
                ConnectionId::new_unchecked(7),
                Some(TRUSTED.parse().expect("valid PeerId")),
                &[],
                Endpoint::Dialer,
            )
            .is_err(),
            "a second dial reusing the id inherits nothing and is refused"
        );
    }

    #[test]
    fn every_refusal_is_recorded_because_nothing_downstream_records_it() {
        // The Swarm discards the denial of a behaviour-originated dial
        // — `if let Ok(()) = self.dial(opts)` — so a refusal not
        // written down here is written down nowhere. SPIKE-004's F8.
        //
        // Both refusal shapes are exercised: one the gate makes before
        // asking the policy, and one the policy makes.
        let m = manager(&[]);
        let (mut gate, _in_flight, _a) = attributed_gate(&m);
        let refusals = gate.refusals();
        assert_eq!(refusals.total(), 0);

        // No attribution: refused before the policy is consulted.
        let _ = gate.handle_pending_outbound_connection(
            ConnectionId::new_unchecked(1),
            Some(TRUSTED.parse().expect("valid PeerId")),
            &[],
            Endpoint::Dialer,
        );
        // Attributed, and refused BY the policy: nobody is trusted.
        gate.attribution()
            .announce(ConnectionId::new_unchecked(2), DialOrigin::KademliaQuery);
        let _ = gate.handle_pending_outbound_connection(
            ConnectionId::new_unchecked(2),
            Some(TRUSTED.parse().expect("valid PeerId")),
            &[],
            Endpoint::Dialer,
        );

        assert_eq!(refusals.total(), 2, "both refusals are recorded");
        assert_eq!(
            refusals.released_after_admission(),
            0,
            "and neither is a release: the policy or the gate said no, no ticket existed"
        );
        let recent = refusals.recent();
        assert_eq!(recent.len(), 2);
        assert_eq!(
            recent[0].origin, None,
            "the unattributed refusal names no origin, because there was none"
        );
        assert_eq!(recent[0].denial, None, "and the policy was never asked");
        assert_eq!(
            recent[1].origin,
            Some(DialOrigin::KademliaQuery),
            "the policy refusal names the origin it was decided under"
        );
        assert_eq!(
            recent[1].denial,
            Some(DialDenial::Unauthorized),
            "and the reason, which `ConnectionDenied`'s own Display does not carry"
        );
        assert_eq!(
            refusals.counts().get(&(
                Some(DialOrigin::KademliaQuery),
                Some(DialDenial::Unauthorized)
            )),
            Some(&1)
        );
    }

    #[test]
    fn a_refusal_handle_taken_before_the_gate_moves_still_reads_it() {
        // `SwarmRuntime` clones `outbound.refusals()` and then moves
        // the gate into `SubstrateBehaviour`, inside a private
        // `GatedSwarm`, inside the Swarm task — after which nothing can
        // reach the gate again. The handle taken beforehand is the only
        // path to the record, so it has to be a VIEW of it.
        //
        // Review finding on PR #71: the record existed and the runtime
        // kept no handle, which left a denied behaviour dial exactly as
        // invisible as before.
        let m = manager(&[]);
        let (gate, _in_flight, _a) = attributed_gate(&m);
        let taken_before = gate.refusals();

        // The gate is CONSUMED here, the way `SubstrateBehaviour::new`
        // consumes it — it does not come back, and neither does any
        // path to its record except the handle above.
        fn swallow_the_gate(mut gate: OutboundAdmission) {
            gate.attribution()
                .announce(ConnectionId::new_unchecked(1), DialOrigin::KademliaQuery);
            let _ = gate.handle_pending_outbound_connection(
                ConnectionId::new_unchecked(1),
                Some(TRUSTED.parse().expect("valid PeerId")),
                &[],
                Endpoint::Dialer,
            );
        }
        swallow_the_gate(gate);

        assert_eq!(
            taken_before.total(),
            1,
            "the handle reads refusals made after it was taken"
        );
        assert_eq!(
            taken_before.recent()[0].denial,
            Some(DialDenial::Unauthorized)
        );
    }

    #[test]
    fn the_refusal_ring_is_bounded() {
        // An attacker chooses how many refusals to provoke. The counts
        // are a product of two small enums and bounded by
        // construction; the verbatim ring is not, so it is capped and
        // drops oldest-first.
        let m = manager(&[]);
        let (mut gate, _in_flight, _a) = attributed_gate(&m);
        let refusals = gate.refusals();
        for id in 0..(RECENT_CAPACITY * 3) {
            let _ = gate.handle_pending_outbound_connection(
                ConnectionId::new_unchecked(id),
                Some(TRUSTED.parse().expect("valid PeerId")),
                &[],
                Endpoint::Dialer,
            );
        }
        assert_eq!(
            refusals.recent().len(),
            RECENT_CAPACITY,
            "the ring holds at most its capacity however many refusals arrive"
        );
        assert_eq!(
            refusals.total(),
            (RECENT_CAPACITY * 3) as u64,
            "while the total still counts every one"
        );
    }

    #[tokio::test]
    async fn an_untrusted_behaviour_dial_is_refused_and_reserves_nothing() {
        // The spike's own mutation, against production: admit everything
        // and all three of these tests fail. ADR-0012's default admits
        // nobody, so a Kademlia walk to a stranger stops HERE.
        let m = manager(&[]);
        let (mut g, in_flight) = gate(&m);
        assert!(behaviour_dial(&mut g, 1, TRUSTED).is_err());
        assert_eq!(in_flight.outstanding(), 0, "a refusal deposits nothing");
        assert_eq!(
            m.handle().load().pending_dials(),
            0,
            "and reserves nothing — a denied dial cannot spend the ceiling"
        );
    }

    #[tokio::test]
    async fn a_trusted_behaviour_dial_is_admitted_and_its_ticket_deposited() {
        let m = manager(&[TRUSTED]);
        let (mut g, in_flight) = gate(&m);
        let contributed = behaviour_dial(&mut g, 1, TRUSTED).expect("trusted, fresh policy");
        assert!(
            contributed.is_empty(),
            "the gate never decides where a dial goes"
        );
        assert_eq!(in_flight.outstanding(), 1, "the ticket is deposited");
        assert_eq!(
            m.handle().load().pending_dials(),
            1,
            "and its reservation is REAL: dropped on receipt, the ceiling \
             would bound nothing (F8)"
        );
        let ticket = in_flight
            .settle(ConnectionId::new_unchecked(1))
            .expect("ours");
        assert_eq!(ticket.address(), "", "admitted on the placeholder (F9)");
        assert_eq!(ticket.origin(), DialOrigin::KademliaQuery);
    }

    #[tokio::test]
    async fn a_draining_manager_refuses_behaviour_dials() {
        let mut m = manager(&[TRUSTED]);
        m.begin_shutdown();
        let (mut g, in_flight) = gate(&m);
        assert!(behaviour_dial(&mut g, 1, TRUSTED).is_err());
        assert_eq!(in_flight.outstanding(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn peer_backoff_binds_and_lapses_on_the_live_clock() {
        // F8b: the spike's gate froze its clock at zero and every
        // backoff became permanent while every immediate-refusal
        // assertion stayed green. The lapse half of this test is what
        // that mutation fails.
        let mut m = manager(&[TRUSTED]);
        let peer = ident(TRUSTED);
        let ticket = m
            .handle()
            .admit(
                &interweave_transport_runtime::DialRequest {
                    peer: Some(peer),
                    address: "/ip4/192.0.2.1/tcp/1".to_owned(),
                    origin: DialOrigin::ConnectionManager,
                },
                0,
            )
            .expect("admitted");
        m.record_failure(ticket, 0);

        let (mut g, _in_flight) = gate(&m);
        assert!(
            behaviour_dial(&mut g, 1, TRUSTED).is_err(),
            "a peer in backoff is refused to Kademlia exactly as to everyone"
        );
        tokio::time::advance(std::time::Duration::from_secs(3_600)).await;
        behaviour_dial(&mut g, 2, TRUSTED)
            .expect("the backoff LAPSES: the gate reads a clock that moves");
    }

    #[tokio::test]
    async fn a_ticketed_dial_passes_exactly_once() {
        let m = manager(&[]);
        let (mut g, _in_flight) = gate(&m);
        let admitted = g.admitted();
        let id = ConnectionId::new_unchecked(11);
        admitted.register(id);
        assert!(
            g.handle_pending_outbound_connection(id, None, &[], Endpoint::Dialer)
                .is_ok(),
            "the admitted dial proceeds"
        );
        assert_eq!(admitted.outstanding(), 0, "and the registration is spent");
        assert!(
            g.handle_pending_outbound_connection(id, None, &[], Endpoint::Dialer)
                .is_err(),
            "the same id must not pass a second time — and with no peer \
             there is nothing to classify a policy admission from"
        );
    }

    #[tokio::test]
    async fn the_established_hook_rebinds_and_refuses_a_quarantined_route() {
        let mut m = manager(&[TRUSTED]);
        let peer = ident(TRUSTED);
        // Quarantine one address the ordinary way: it authenticated the
        // wrong identity once.
        let bad = m
            .handle()
            .admit(
                &interweave_transport_runtime::DialRequest {
                    peer: Some(peer.clone()),
                    address: "/ip4/192.0.2.1/tcp/1".to_owned(),
                    origin: DialOrigin::ConnectionManager,
                },
                0,
            )
            .expect("admitted");
        assert!(m.record_identity_mismatch(bad, 0));

        let (mut g, in_flight) = gate(&m);
        // The PENDING hook passes: the quarantine is address-scoped and
        // the placeholder names no address (F16, structurally).
        behaviour_dial(&mut g, 1, TRUSTED).expect("peer-scoped policy holds");
        // The dial lands on the quarantined route, peer suffix and all.
        let refused = established(
            &mut g,
            1,
            TRUSTED,
            &format!("/ip4/192.0.2.1/tcp/1/p2p/{TRUSTED}"),
        );
        assert!(
            refused.is_err(),
            "the address the dial actually used is one the policy suppresses"
        );
        let ticket = in_flight
            .settle(ConnectionId::new_unchecked(1))
            .expect("ours");
        assert_eq!(
            ticket.address(),
            "/ip4/192.0.2.1/tcp/1",
            "re-bound BEFORE the verdict — the settlement scores the real \
             route, stripped of its /p2p suffix (F10/F12)"
        );

        // THE CONTROL: the same peer's other address establishes.
        behaviour_dial(&mut g, 2, TRUSTED).expect("admitted again");
        let kept = established(
            &mut g,
            2,
            TRUSTED,
            &format!("/ip4/192.0.2.2/tcp/1/p2p/{TRUSTED}"),
        );
        assert!(
            kept.is_ok(),
            "quarantine is a fact about ONE address, not about the peer"
        );
        let ticket = in_flight
            .settle(ConnectionId::new_unchecked(2))
            .expect("ours");
        assert_eq!(ticket.address(), "/ip4/192.0.2.2/tcp/1");
    }

    #[tokio::test]
    async fn an_ordinary_admitted_dial_is_untouched_at_establishment() {
        let m = manager(&[TRUSTED]);
        let (mut g, in_flight) = gate(&m);
        let ticket = m
            .handle()
            .admit(
                &interweave_transport_runtime::DialRequest {
                    peer: Some(ident(TRUSTED)),
                    address: "/ip4/192.0.2.9/tcp/1".to_owned(),
                    origin: DialOrigin::ConnectionManager,
                },
                0,
            )
            .expect("admitted");
        in_flight.deposit(ConnectionId::new_unchecked(3), ticket);
        // Even a quarantined-looking establishment address changes
        // nothing: the address was DECIDED at admission, and the hook
        // refuses to move or judge it.
        let kept = established(&mut g, 3, TRUSTED, "/ip4/192.0.2.7/tcp/7");
        assert!(kept.is_ok());
        let ticket = in_flight
            .settle(ConnectionId::new_unchecked(3))
            .expect("ours");
        assert_eq!(
            ticket.address(),
            "/ip4/192.0.2.9/tcp/1",
            "an ordinary ticket's address never moves"
        );
    }

    #[tokio::test]
    async fn a_foreign_suffix_is_not_laundered_at_establishment() {
        // The established hook's variant of the identity check: the
        // connection authenticated TRUSTED, and the address claims
        // someone else. The claim stays in the settlement key — the
        // policy records the literal that lied, never the bare route it
        // was lying about.
        let m = manager(&[TRUSTED]);
        let (mut g, in_flight) = gate(&m);
        behaviour_dial(&mut g, 4, TRUSTED).expect("admitted");
        const OTHER: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";
        let kept = established(
            &mut g,
            4,
            TRUSTED,
            &format!("/ip4/192.0.2.1/tcp/1/p2p/{OTHER}"),
        );
        assert!(kept.is_ok(), "no quarantine exists for that literal");
        let ticket = in_flight
            .settle(ConnectionId::new_unchecked(4))
            .expect("ours");
        assert_eq!(
            ticket.address(),
            format!("/ip4/192.0.2.1/tcp/1/p2p/{OTHER}"),
            "a claim naming another identity is not stripped into the bare route"
        );
    }

    #[test]
    fn the_peer_suffix_strip_is_trailing_only() {
        let plain: Multiaddr = "/ip4/192.0.2.1/tcp/1".parse().expect("valid");
        assert_eq!(strip_peer_suffix(&plain), "/ip4/192.0.2.1/tcp/1");
        let suffixed: Multiaddr = format!("/ip4/192.0.2.1/tcp/1/p2p/{TRUSTED}")
            .parse()
            .expect("valid");
        assert_eq!(strip_peer_suffix(&suffixed), "/ip4/192.0.2.1/tcp/1");
        // A relay path's INNER hop is part of the route, not a claim.
        let relayed: Multiaddr =
            format!("/ip4/192.0.2.1/tcp/1/p2p/{TRUSTED}/p2p-circuit/p2p/{TRUSTED}")
                .parse()
                .expect("valid");
        assert_eq!(
            strip_peer_suffix(&relayed),
            format!("/ip4/192.0.2.1/tcp/1/p2p/{TRUSTED}/p2p-circuit"),
            "only the trailing component is the dial's own peer claim"
        );
    }

    fn failure<'a>(id: usize, error: &'a DialError) -> FromSwarm<'a> {
        FromSwarm::DialFailure(DialFailure {
            peer_id: Some(TRUSTED.parse().expect("valid PeerId")),
            error,
            connection_id: ConnectionId::new_unchecked(id),
        })
    }

    #[tokio::test]
    async fn a_synchronous_failure_after_admission_releases_the_ticket() {
        // The two failures the Swarm reports inside `Swarm::dial`, after
        // this hook admitted the dial, with no settlement to follow: a
        // later field's pending-hook denial, and no address left once
        // this node's own listeners were stripped. Each held a pending
        // slot for the process's life; each now releases it, and is
        // written down.
        let m = manager(&[TRUSTED]);
        let (mut g, in_flight) = gate(&m);
        let refusals = g.refusals();
        let snapshot = m.handle();
        let denied = DialError::Denied {
            cause: ConnectionDenied::new(std::io::Error::other("a later field said no")),
        };
        for (id, error, detail) in [
            (1, &denied, DENIED_AFTER_ADMISSION),
            (2, &DialError::NoAddresses, NO_ADDRESSES_AFTER_ADMISSION),
        ] {
            behaviour_dial(&mut g, id, TRUSTED).expect("admitted");
            assert_eq!(in_flight.outstanding(), 1);
            assert_eq!(snapshot.load().pending_dials(), 1, "the slot is held");
            assert_eq!(snapshot.load().connections(), 1, "and the connection slot");
            g.on_swarm_event(failure(id, error));
            assert_eq!(in_flight.outstanding(), 0, "the ticket is taken back");
            assert_eq!(
                snapshot.load().pending_dials(),
                0,
                "and dropping it returned the pending slot"
            );
            assert_eq!(
                snapshot.load().connections(),
                0,
                "and the connection slot -- both reservations, one drop"
            );
            let last = refusals.recent().pop().expect("written down");
            assert_eq!(last.detail, detail);
            assert_eq!(last.origin, Some(DialOrigin::KademliaQuery));
        }
        assert_eq!(refusals.total(), 2);
        assert_eq!(
            refusals.released_after_admission(),
            2,
            "counted apart from a refusal the policy or the gate made"
        );

        // THE CONTROLS. A failure the pool reports -- a transport error
        // -- is followed by `OutgoingConnectionError`, and the runtime
        // settles that; the ticket stays for it.
        behaviour_dial(&mut g, 3, TRUSTED).expect("admitted");
        g.on_swarm_event(failure(3, &DialError::Transport(Vec::new())));
        assert_eq!(in_flight.outstanding(), 1, "not this gate's to settle");
        // And a `Denied` from an ESTABLISHED hook comes after this gate
        // re-bound the ticket -- the pool had the dial, so its failure
        // is reported as `OutgoingConnectionError` as well.
        let _ = established(
            &mut g,
            3,
            TRUSTED,
            &format!("/ip4/192.0.2.2/tcp/1/p2p/{TRUSTED}"),
        );
        g.on_swarm_event(failure(3, &denied));
        assert_eq!(
            in_flight.outstanding(),
            1,
            "a re-bound ticket is left alone"
        );
        assert_eq!(refusals.total(), 2, "and neither control was written down");
        assert_eq!(refusals.released_after_admission(), 2);
        // A failure for a dial that was never ours.
        g.on_swarm_event(failure(9, &DialError::NoAddresses));
        assert_eq!(in_flight.outstanding(), 1);
    }

    /// A behaviour that emits one dial to whatever it is told, once.
    struct DialOnce {
        opts: Option<libp2p::swarm::dial_opts::DialOpts>,
    }

    impl NetworkBehaviour for DialOnce {
        type ConnectionHandler = dummy::ConnectionHandler;
        type ToSwarm = std::convert::Infallible;

        fn handle_established_inbound_connection(
            &mut self,
            _: ConnectionId,
            _: PeerId,
            _: &Multiaddr,
            _: &Multiaddr,
        ) -> Result<THandler<Self>, ConnectionDenied> {
            Ok(dummy::ConnectionHandler)
        }

        fn handle_established_outbound_connection(
            &mut self,
            _: ConnectionId,
            _: PeerId,
            _: &Multiaddr,
            _: Endpoint,
            _: PortUse,
        ) -> Result<THandler<Self>, ConnectionDenied> {
            Ok(dummy::ConnectionHandler)
        }

        fn on_swarm_event(&mut self, _: FromSwarm<'_>) {}

        fn on_connection_handler_event(
            &mut self,
            _: PeerId,
            _: ConnectionId,
            _: THandlerOutEvent<Self>,
        ) {
        }

        fn poll(
            &mut self,
            _: &mut Context<'_>,
        ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
            match self.opts.take() {
                Some(opts) => Poll::Ready(ToSwarm::Dial { opts }),
                None => Poll::Pending,
            }
        }
    }

    #[derive(NetworkBehaviour)]
    struct GateThenDialer {
        gate: OutboundAdmission,
        dialer: crate::attribution::Attributing<DialOnce>,
    }

    #[tokio::test]
    async fn the_swarm_itself_reports_no_addresses_for_a_dial_to_our_own_listener_and_the_slot_returns()
     {
        // THE MECHANISM, not a lookalike: a real Swarm, a behaviour dial
        // whose only address is this node's own listener. The Swarm
        // runs the pending hooks (the gate admits, deposits), strips the
        // address as a listened one, and reports `NoAddresses` to the
        // behaviours with no `Dialing` and no `OutgoingConnectionError`
        // (libp2p-swarm 0.47.1 `lib.rs:496-510`). Before this fix the
        // ticket stayed in flight for ever.
        use futures::StreamExt as _;
        let m = manager(&[TRUSTED]);
        let (gate, in_flight, attribution) = attributed_gate(&m);
        let refusals = gate.refusals();
        let snapshot = m.handle();
        let mut swarm = libp2p::SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                libp2p::tcp::Config::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )
            .expect("transport")
            .with_behaviour(|_| GateThenDialer {
                gate,
                dialer: crate::attribution::Attributing::new(
                    DialOnce { opts: None },
                    crate::attribution::always(DialOrigin::KademliaQuery),
                    attribution,
                ),
            })
            .expect("behaviour")
            .build();
        swarm
            .listen_on("/ip4/127.0.0.1/tcp/0".parse().expect("a listen address"))
            .expect("listens");
        let listener = loop {
            if let libp2p::swarm::SwarmEvent::NewListenAddr { address, .. } =
                swarm.select_next_some().await
            {
                break address;
            }
        };
        // The dial: a trusted peer, at OUR address.
        swarm.behaviour_mut().dialer.inner_mut().opts = Some(
            libp2p::swarm::dial_opts::DialOpts::peer_id(TRUSTED.parse().expect("valid PeerId"))
                .addresses(vec![listener])
                .build(),
        );
        // Drive until the refusal is written down. Polled in short
        // slices: the failure produces NO Swarm event, so nothing wakes
        // `select_next_some` once the refusal has been recorded inside
        // its poll.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while refusals.total() == 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the Swarm never reported the failure"
            );
            let _ = tokio::time::timeout(
                std::time::Duration::from_millis(50),
                swarm.select_next_some(),
            )
            .await;
        }
        assert_eq!(
            refusals.recent().pop().expect("one").detail,
            NO_ADDRESSES_AFTER_ADMISSION
        );
        assert_eq!(in_flight.outstanding(), 0, "the ticket was taken back");
        assert_eq!(snapshot.load().pending_dials(), 0, "and the slot returned");
        assert_eq!(snapshot.load().connections(), 0, "both of them");
        assert_eq!(refusals.released_after_admission(), 1);
        assert_eq!(
            swarm.behaviour().gate.attribution().outstanding(),
            0,
            "and the attribution note was forgotten"
        );
    }
}
