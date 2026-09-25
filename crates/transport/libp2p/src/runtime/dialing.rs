// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! Dialling, and what a dial outcome does to policy.
//!
//! Split out of `runtime.rs` unchanged. Every outbound dial in this
//! crate passes through here, which is the point: `GatedSwarm::dial`
//! takes an `AdmittedDial` that can only be derived from a `DialTicket`,
//! so a path that forgets to ask the root admission gate does not
//! misbehave — it does not compile.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use libp2p::core::transport::{ListenerId, TransportError};
use libp2p::swarm::DialError;
use libp2p::swarm::SwarmEvent as Libp2pSwarmEvent;
use libp2p::{Multiaddr, PeerId, identify};
use tokio::sync::oneshot;

use interweave_transport_api::TransportIdentity;
use interweave_transport_runtime::reachability::{CandidateRefusal, is_advertised_address};
use interweave_transport_runtime::{
    ConnectionClass, ConnectionManager, ConnectionSlot, DialOrigin, DialRequest, DialTicket,
    Revoked,
};

use crate::behaviour::SubstrateBehaviourEvent;
use crate::gated_swarm::{AdmittedDial, GatedSwarm, UndialableAdmission};
use crate::outbound_gate::{InFlightTickets, strip_own_suffix, strip_peer_suffix};

use super::messages::{DialRefusal, PathChange, PeerPath, SwarmEvent};
use super::to_transport_identity;

/// Admit one dial, bind it to its ticket, and hand it to the Swarm.
///
/// The single place a dial happens, whoever asked: the command path,
/// the address-book path, and the retry scheduler all arrive here. A
/// second copy of this sequence is how one of them would end up
/// skipping the ticket, the binding, or the settlement.
pub(super) fn attempt_dial(
    swarm: &mut GatedSwarm,
    manager: &mut ConnectionManager,
    in_flight: &InFlightTickets,
    peer: &TransportIdentity,
    address: &str,
    origin: DialOrigin,
    now_ms: u64,
) -> Result<(), DialRefusal> {
    let request = DialRequest {
        peer: Some(peer.clone()),
        // CANONICAL BEFORE ADMISSION, so one physical route cannot hold
        // two quarantine entries. A caller reaching the same socket as
        // `/ip4/A/tcp/P` and as `/ip4/A/tcp/P/p2p/<dest>` used to earn a
        // quarantine on one spelling and keep dialling the other, and
        // both spent a slot in a map whose bound is the point. See
        // [`canonical_dial_address`] for what is and is not stripped.
        address: canonical_dial_address(peer, address),
        origin,
    };
    // ADMITTED BEFORE A SOCKET IS OPENED. A quarantined address costs
    // nothing, which is the whole point of checking here rather than
    // after the connection fails.
    //
    // THE CLASS IS NOT THIS SITE'S TO ASSERT. It used to be a hardcoded
    // `DataPlaneTrusted` on every dial, which is the ADR-0036
    // separation stated in the policy and discarded by its only caller.
    // The gate classifies from the trust sources it publishes, and
    // there is no longer an argument through which a call site could
    // say otherwise.
    let ticket = manager
        .handle()
        .admit(&request, now_ms)
        .map_err(DialRefusal::Policy)?;

    // DERIVED FROM THE ADMISSION, not paired with it. The destination
    // is read back out of the ticket rather than rebuilt from the
    // caller's own peer and address, so there is no second copy of the
    // destination that could disagree with the one the gate admitted.
    let admitted = match AdmittedDial::from_ticket(ticket) {
        Ok(a) => a,
        Err(boxed) => {
            return Err(DialRefusal::Backend(settle_undialable(
                manager, *boxed, now_ms,
            )));
        }
    };
    let id = admitted.connection_id();
    match swarm.dial(admitted) {
        Ok(ticket) => {
            // Held until the outcome event settles it. Dropping it here
            // would release the pending slot the instant the dial
            // began, and the ceiling would bound nothing but the rate
            // of the loop.
            in_flight.deposit(id, ticket);
            Ok(())
        }
        Err(boxed) => {
            let (e, ticket) = *boxed;
            // A synchronous refusal produces no event, so the admission
            // is settled here or never.
            if is_permanent_dial_error(&e) {
                manager.record_permanent_failure(ticket, now_ms);
            } else {
                manager.record_failure(ticket, now_ms);
            }
            Err(DialRefusal::Backend(e.to_string()))
        }
    }
}

/// The spelling a route is admitted, scored, quarantined AND remembered
/// under.
///
/// ALL FOUR, which is the correction: an earlier version of this ran at
/// `attempt_dial` only, so the ticket and the quarantine keyed on the
/// stripped form while `learn_address` went on storing whatever string
/// arrived. That is worse than not stripping at all. A quarantined route
/// no longer filtered out of `preferred_addresses`, so the scheduler
/// re-offered it every tick and the gate refused it every tick;
/// `record_permanent_failure`'s `known.remove` missed, so an undialable
/// address held one of eight per-peer slots forever; one route occupied
/// two book entries and `is_known_good` missed on the one the candidate
/// list actually carried; and `learn_address` could evict nothing,
/// because an entry whose quarantine it cannot see reads as dialable.
/// Review finding on PR #86. Every site that writes a `(peer, address)`
/// key for a KNOWN peer resolves it through this function or through
/// [`canonical_for_peer`] beneath it -- one implementation, reached two
/// ways, rather than two implementations that agree today. The peerless
/// settlement arm is the exception and is unreachable through admission.
///
/// Strips a trailing `/p2p/<peer>` when it names the peer being dialled.
/// The suffix is redundant there -- the dial already names the peer
/// through its own argument, which is what the gate classifies on -- so
/// two spellings of one route collapse to one key.
///
/// THE STRIPPED COMPONENT IS PUT BACK BEFORE ANYTHING IS DIALLED, and
/// this is the property that makes using one string as both the policy key
/// and the dial address safe. `AdmittedDial` binds `ticket.address()` and
/// `Swarm::dial` then calls `Multiaddr::with_p2p(peer)` on it
/// (`libp2p-swarm-0.48.0/src/lib.rs:519`), which appends `/p2p/<peer>`
/// whenever the address does not already end in one
/// (`multiaddr-0.18.2/src/lib.rs:137-143`). So the transport sees the
/// caller's original address, not the key.
///
/// IT MATTERS MOST FOR A CIRCUIT. `libp2p-relay`'s client transport
/// refuses an address with no destination component --
/// `dst_peer_id.ok_or(Error::MissingDstPeerId)`
/// (`libp2p-relay-0.22.0/src/priv_client/transport.rs:205`) -- and the key
/// for `/ip4/A/tcp/P/p2p/<relay>/p2p-circuit/p2p/<dest>` has that
/// component stripped. The last component of the key is `P2pCircuit`
/// rather than `P2p`, so `with_p2p` takes its appending branch and
/// reconstructs the original exactly. Two reviewers disagreed about this
/// and one of them was reasoning from the key alone; it is pinned by
/// `the_stripped_suffix_is_restored_before_the_transport_sees_it` rather
/// than left to be re-argued.
///
/// IT ALSO RE-SERIALIZES, and the earlier wording said "and nothing
/// else", which was false: the address goes through `Multiaddr` and back
/// even when no component is popped, so `/ip6/2001:db8:0:0:0:0:0:1/tcp/1`
/// comes back as `/ip6/2001:db8::1/tcp/1` and `/tcp/0080` as `/tcp/80`.
/// That collapses more spellings of one route, which is what a key is
/// for, so it is kept deliberately rather than worked around -- but it is
/// a second way two inputs become one key, and it is now tested instead
/// of being an accident of the helper this calls.
///
/// FOUR THINGS ARE DELIBERATELY LEFT ALONE, and each is a different
/// reason:
///
/// - **A `/p2p/<relay>` BEFORE a `/p2p-circuit`.** That component is
///   part of the route, not a claim about the destination: it says which
///   relay carries the circuit, and a different relay is a different
///   path that can fail and be quarantined on its own. Stripping it
///   would collapse every relay into one key, so a single bad relay
///   would suppress the destination through all of them.
///   [`strip_own_suffix`] cannot reach it -- it pops only TRAILING
///   `P2p` components, and a circuit's relay sits behind the
///   `/p2p-circuit` marker -- but the property is the reason this
///   function is allowed to call it, so it is tested rather than
///   inferred from the helper's shape.
/// - **A trailing claim naming someone ELSE.** The address is then
///   contradicting the dial, and stripping it would launder the
///   contradiction into the bare route: the policy would score and
///   quarantine a string the caller never actually asked for. The
///   foreign claim stays in the key, so whatever is recorded is
///   recorded against the literal that lied. This is
///   [`strip_own_suffix`]'s own rule and the reason the peerless
///   [`strip_peer_suffix`] is not used here.
/// - **Anything that does not parse.** A non-multiaddr address and a
///   non-`PeerId` peer are returned unchanged so they reach
///   [`AdmittedDial::from_ticket`] intact and settle through
///   [`settle_undialable`] exactly as before. Canonicalizing is not the
///   place to change how a malformed value is classified.
///
/// - **An address that is nothing BUT the peer's own suffix.** Stripping
///   it yields the empty multiaddr, which no longer parses, so returning
///   it unchanged keeps WHICH undialable it is. This one was a trailing
///   paragraph rather than a bullet, which is how two other files came to
///   say "three things". Review finding on PR #86.
///
/// Review finding, recorded as a deferred follow-up on PR #74 because
/// this is a keying change to a security boundary: it decides what the
/// quarantine map and the address book agree about.
pub(super) fn canonical_dial_address(peer: &TransportIdentity, address: &str) -> String {
    let Ok(parsed) = address.parse::<Multiaddr>() else {
        return address.to_owned();
    };
    let Ok(expected) = peer.as_str().parse::<PeerId>() else {
        return address.to_owned();
    };
    canonical_for_peer(&parsed, &expected)
}

/// The same key, for a caller that already holds parsed values.
///
/// ONE IMPLEMENTATION, which is the point of it existing separately.
/// `settle_failed_dial` and `OutboundAdmission`'s established hook each
/// computed this key their own way over `strip_own_suffix`, and the three
/// did not all agree: both of those answered `""` where stripping yields
/// the empty multiaddr, because that is what `strip_own_suffix` returns,
/// while THIS returns the original so the undialable keeps its identity.
/// Two agreed with each other and the third was right, which an earlier
/// version of this paragraph flattened into "all three already disagreed"
/// -- and it said "that closure" with no closure named anywhere above it.
/// Harmless today, and exactly the shape of agreement-by-coincidence that
/// a review had just finished naming elsewhere in this file, so BOTH
/// duplicate COMPUTATIONS are gone rather than documented, in two commits:
/// `settle_failed_dial`'s closure still exists and is one of the three
/// counted call sites, but it delegates here instead of walking the
/// components itself, and the established hook's raw call was replaced.
/// This said "the second copy", singular, from when only the first had been
/// removed, and the correction
/// to the sentence above it left that three lines down untouched. Review
/// findings on PR #86.
pub(crate) fn canonical_for_peer(address: &Multiaddr, peer: &PeerId) -> String {
    let stripped = strip_own_suffix(address, peer);
    if stripped.is_empty() {
        // An address that is nothing but the peer's own suffix: stripping
        // yields the empty multiaddr, which no longer parses, so keeping
        // the original preserves WHICH undialable it is.
        return address.to_string();
    }
    stripped
}

/// Remember a route, under the one spelling everything else keys by.
///
/// THE ONLY PLACE THIS MODULE CALLS `learn_address` outside its own tests,
/// and `no_production_path_learns_an_address_without_canonicalizing` is
/// what keeps that true. THE MODULE, not the crate: that guard reads the
/// files `runtime/mod.rs` declares and nothing else, so a call appearing in
/// `outbound_gate.rs` or `gated_swarm.rs` would pass unseen. It is true of
/// the crate today -- nothing outside this module calls it -- but the test
/// is not what holds that, and an earlier version of this sentence said it
/// was. Review finding on PR #86. The first version of PR #86's fix canonicalized
/// at `attempt_dial` and left two learn sites raw, which is how a
/// half-applied key rule got shipped; one wrapper means the rule cannot be
/// applied to some callers and not others.
///
/// Idempotent, so a caller holding an address that is already canonical --
/// `settle_established_outbound`, reading it back off the ticket -- passes
/// it straight through rather than having to know which kind it holds.
pub(super) fn learn_route(
    manager: &mut ConnectionManager,
    peer: &TransportIdentity,
    address: &str,
    now_ms: u64,
) -> bool {
    manager.learn_address(peer, &canonical_dial_address(peer, address), now_ms)
}

/// Settle an admission that could not be turned into a dial, and say why.
///
/// PERMANENT, not transient. Every way `AdmittedDial::from_ticket`
/// fails is a deterministic property of the ticket itself -- it names
/// no peer, its peer is not a libp2p `PeerId`, its address is not a
/// multiaddr, or its origin and its address disagree about whether the
/// dial is a circuit -- so the same ticket converts the same way every
/// time, whatever the network does. `record_failure` reschedules, so a
/// trusted peer with a remembered address retried that identical
/// conversion failure forever once the scheduler became active.
///
/// The ADDRESS case is the reachable one: an address is an opaque string
/// to every neutral type it passes through, so a configured or discovered
/// value that is not a multiaddr arrives here intact.
///
/// The `PeerId` case USED to be reachable the same way — the neutral
/// grammar checked a prefix, an alphabet and a length while libp2p
/// decoded the multihash, so `Qm` plus 44 base58 characters satisfied the
/// first and failed the second. `TransportIdentity::parse` now decodes
/// too, and `every_identity_the_neutral_grammar_accepts_libp2p_accepts`
/// is what says the two agree. The branch stays as a fail-closed guard on
/// a conversion this module does not own; it is unreachable rather than
/// untested.
///
/// The CIRCUIT PAIRING case is the one to be careful with, because a
/// mislabelling here is not free. `from_ticket` refuses both
/// directions of the disagreement -- a `/p2p-circuit` address admitted
/// under some other origin, and `RelayCircuit` on an address with no
/// circuit in it -- and both arrive here. `from_ticket`'s own comment
/// says which mistake each one is; this said it a second time and said
/// it differently, naming the relay's class as what the first was
/// judged against when the ticket names the destination in both
/// directions. `record_permanent_failure` then FORGETS THE ADDRESS
/// (`connection_manager.rs`, `known.remove(ticket.address())`), which
/// is right for an address that cannot be dialled and wrong for a good
/// circuit address that a caller labelled badly. Permanent is still
/// the correct class, since the pairing is a property of the ticket
/// and would fail identically on every retry; the note is that the
/// blast radius of a caller-side bug is a forgotten route, not a
/// refused dial. Reachable from one direction: a circuit address under
/// an origin other than `RelayCircuit`, from a caller that skipped
/// `origin_for`'s classification -- every book-and-command dial goes
/// through it since step 7, and the AutoNAT adapter's own target
/// addresses, which may carry a circuit from Identify, are refused
/// here on purpose; `RelayCircuit` on an address with no circuit comes
/// from nowhere in the tree. Review finding on PR #74.
pub(super) fn settle_undialable(
    manager: &mut ConnectionManager,
    undialable: UndialableAdmission,
    now_ms: u64,
) -> String {
    // The refusal is still an admission that reserved a slot, so it is
    // settled here rather than dropped on the floor.
    manager.record_permanent_failure(undialable.ticket, now_ms);
    undialable.reason
}

/// Settle one ESTABLISHED outbound dial: keep it, or say it must go.
///
/// REVALIDATED, not merely recorded. Admission happened when the dial
/// was ADMITTED; the handshake that just finished could have taken long
/// enough for a trust revocation or a drain to land in between.
/// Retaining the connection because it was admitted once would hold it
/// open under authority that no longer exists.
///
/// THE ORIGIN IS PART OF THE QUESTION. An infrastructure-only peer is
/// authorized for reachability and refused as an application
/// destination, so asking only what the peer is authorized FOR — the
/// inbound predicate, which has no origin to consult — closed relay
/// reservations and AutoNAT probes that admission had correctly
/// permitted. (It closed relay circuits and DCUtR hole punches too,
/// but those were admitted WRONGLY — SPIKE-004's D2 and D1, refused at
/// admission since Stage 11 step 2, so revalidation no longer sees
/// them for such a peer at all.) `authorizes_for` takes the ticket's
/// own origin,
/// so a `KademliaQuery` connection is revalidated by the SAME line that
/// revalidates every other — the genericity
/// `a_revoked_kademlia_dial_is_refused_at_establishment` proves rather
/// than assumes.
///
/// Extracted from the event arm so it is reachable from a test:
/// `SwarmEvent` is `#[non_exhaustive]` and cannot be constructed.
pub(super) fn settle_established_outbound(
    manager: &mut ConnectionManager,
    peer: &TransportIdentity,
    ticket: DialTicket,
    path: PeerPath,
    now_ms: u64,
) -> Option<(ConnectionSlot, DialOrigin, ConnectionClass)> {
    let class = manager.classify(peer);
    // THE PATH DECIDES THE QUESTION, not the ticket alone (step 7): a
    // connection that came up over a circuit is judged under
    // `RelayCircuit` whatever origin dialled it, so a reservation ask
    // that the other behaviours' address cache extended through a
    // circuit -- the relay reached through a relay -- is refused here
    // rather than retained under `RelayReservation` toward an
    // infrastructure-only far end, the row ADR-0036's amendment forbids.
    // `a_relayed_outbound_is_judged_under_relay_circuit_whatever_dialled_it`
    // pins it.
    let origin = retention_origin(path, ticket.origin());
    if !manager.authorizes_for(class, origin) {
        manager.record_authorization_withdrawn(ticket, now_ms);
        return None;
    }
    // THE ADDRESS THAT WORKED. Learned from the ticket rather than from
    // anything the peer said, so a route this profile has actually
    // authenticated is in the book even if the peer never advertises it.
    let address = ticket.address().to_owned();
    let slot = manager.record_success(ticket, now_ms);
    let _ = learn_route(manager, peer, &address, now_ms);
    Some((slot, origin, class))
}

/// Settle one established INBOUND connection against the manager: the
/// retention question under `asked_under` -- the origin the path or
/// the infrastructure closure named, or the origin-less question --
/// then the ceiling, and the record the runtime keeps of it.
///
/// Extracted from the event arm so it is reachable from a test, as
/// [`settle_established_outbound`] was: the arm takes a
/// `#[non_exhaustive]` Swarm event no test can build. What the record
/// says is the point: `origin` is `None` WHATEVER the question was
/// asked under, relayed or direct, because `Some` means "this profile
/// dialled it" -- the AutoNAT adapter reads it to tell a server it
/// can probe from a peer that dialled us, and a relayed inbound that
/// recorded `RelayCircuit` read as a dial this profile made (PR #101
/// round 1). The origin-less question a trust change re-asks agrees
/// with `RelayCircuit` for every class. Pinned by
/// `an_inbound_is_recorded_origin_less_whatever_it_was_retained_under`.
pub(super) fn settle_established_inbound(
    manager: &mut ConnectionManager,
    peer: TransportIdentity,
    class: ConnectionClass,
    path: PeerPath,
    asked_under: Option<DialOrigin>,
    now_ms: u64,
) -> Option<OpenConnection> {
    let authorized = match asked_under {
        Some(origin) => manager.authorizes_for(class, origin),
        None => manager.authorizes(class),
    };
    if !authorized {
        return None;
    }
    let slot = manager.admit_inbound()?;
    Some(OpenConnection {
        peer,
        slot,
        origin: None,
        admitted_class: class,
        path,
        punched: false,
        since_ms: now_ms,
        retiring: false,
    })
}

/// Settle one failed outbound dial against the manager.
///
/// Extracted from the event arm so it is reachable from a test —
/// `SwarmEvent` is `#[non_exhaustive]` and cannot be constructed, while
/// `DialError` can.
///
/// A BEHAVIOUR dial that failed before its established hook ran still
/// carries the empty placeholder address (F9), and the error itself is
/// the only place the attempted addresses exist: `WrongPeerId` names
/// the address that authenticated wrong, and `DialError::Transport`
/// carries one entry per address exhausted. The ticket is re-bound to
/// the first (F12) and the REST are scored through the admission-free
/// path (F15) — recording only the first leaves the others unscored
/// and immediately retryable. A placeholder that no error names an
/// address for settles as exactly that, and `record_failure` scores
/// nothing for it.
pub(super) fn settle_failed_dial(
    manager: &mut ConnectionManager,
    mut ticket: DialTicket,
    error: &DialError,
    now_ms: u64,
) {
    // A HOLE-PUNCH DIAL SETTLES AND SCORES NOTHING, whatever failed and
    // however many addresses it tried: its addresses are the far end's
    // candidates for one attempt, not routes, and the peer is connected
    // over the relayed connection the attempt runs on. Scoring the rest
    // of a multi-address batch here (below) would put that peer in
    // backoff the way `record_failure` used to; what a failed punch
    // costs is the DCUtR adapter's cooldown (`record_failure`'s own
    // note, PR #102 round 1).
    // `a_hole_punch_dials_failure_scores_none_of_its_addresses` pins it.
    if ticket.origin() == DialOrigin::DcutrHolePunch {
        manager.record_failure(ticket, now_ms);
        return;
    }
    let expected = ticket
        .peer()
        .and_then(|p| p.as_str().parse::<libp2p::PeerId>().ok());
    let strip = |address: &Multiaddr| match &expected {
        // The connection's peer is authenticated knowledge: only ITS
        // claim strips, so a foreign claim stays in the settlement key
        // and the policy records the literal that lied.
        //
        // THROUGH THE SHARED HELPER, so this path cannot drift from what
        // `attempt_dial` admitted and `learn_route` remembered.
        Some(peer) => canonical_for_peer(address, peer),
        // No peer to compare against, so the identity-checked rule has
        // nothing to check and every trailing claim goes. Unreachable
        // through admission -- a ticket naming no peer is refused -- which
        // is why it cannot use the helper above and does not need to.
        None => strip_peer_suffix(address),
    };
    if ticket.address().is_empty() {
        match error {
            DialError::WrongPeerId { address, .. } => {
                let stripped = strip(address);
                let _ = ticket.rebind_address(&stripped);
            }
            DialError::Transport(attempts) if !attempts.is_empty() => {
                let _ = ticket.rebind_address(&strip(&attempts[0].0));
                // EACH ATTEMPT SETTLES BY ITS OWN CLASS. The aggregate
                // answer exists for the single-address dial; here every
                // address carries its own error, and scoring a
                // structural route as transient — the only option the
                // old admission-free path had — kept it in the book and
                // retryable forever, while a mixed batch's aggregate
                // mis-labelled every member.
                if let Some(peer) = ticket.peer().cloned() {
                    for (address, attempt_error) in &attempts[1..] {
                        let stripped = strip(address);
                        if attempt_is_structural(address, attempt_error) {
                            manager.record_permanent_address_failure_unadmitted(&peer, &stripped);
                        } else {
                            manager.record_address_failure_unadmitted(&peer, &stripped, now_ms);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    // NOT EVERY FAILURE IS THE ADDRESS'S FAULT. A peer that answered
    // with a different key is not an unreachable route to be retried on
    // backoff -- it is an address that is serving somebody else, and
    // ADR-0011 puts that into quarantine rather than into the retry
    // schedule. Passing it to `record_failure` like any timeout made
    // `record_identity_mismatch` unreachable, so the quarantine existed
    // only as a method nobody called.
    //
    // THE TICKET'S OWN CLASS IS ITS OWN ATTEMPT'S. The ticket was
    // re-bound to the FIRST attempted address above, so a multi-address
    // error classifies it by that attempt's error rather than by the
    // batch's aggregate — the aggregate said "transient" whenever the
    // batch was mixed, which retried a structural route forever.
    let ticket_is_permanent = match error {
        DialError::Transport(attempts) if !attempts.is_empty() => {
            attempt_is_structural(&attempts[0].0, &attempts[0].1)
        }
        other => is_permanent_dial_error(other),
    };
    if matches!(error, DialError::Denied { .. }) {
        // THIS NODE REFUSED IT, so this node's policy is not evidence
        // about the network. `DialError::Denied` is what a behaviour's
        // `ConnectionDenied` comes back as, and the one that reaches a
        // ticket is the outbound gate's established hook rejecting an
        // address the quarantine suppresses. Scored as an ordinary
        // failure it extended that quarantine — a suppression this node
        // keeps re-testing could then never lapse — and advanced a
        // trusted peer toward punitive backoff over one address this
        // node declined to use.
        manager.record_locally_refused(ticket, now_ms);
    } else if matches!(error, DialError::WrongPeerId { .. }) {
        let _ = manager.record_identity_mismatch(ticket, now_ms);
    } else if ticket_is_permanent {
        // STRUCTURAL, not transient. The same address fails the same
        // way every time this process asks, so treating it as an
        // ordinary network failure -- punitive backoff, a rescheduled
        // retry -- retries a thing retrying cannot fix. The paused-time
        // scheduler test caught this: a UDP address on a TCP-only Swarm
        // was retried forever.
        manager.record_permanent_failure(ticket, now_ms);
    } else {
        // ADDRESS-SCOPED, not peer-scoped. ADR-0011: a failure against
        // one address must not advance a trusted peer into punitive
        // backoff while a known-good route remains, and
        // `record_failure` is the path that keeps that distinction.
        manager.record_failure(ticket, now_ms);
    }
}

/// Whether ONE transport attempt is structural: this process's own
/// stack refusing the address's shape, which no retry changes.
fn attempt_is_structural(address: &Multiaddr, error: &TransportError<std::io::Error>) -> bool {
    matches!(error, TransportError::MultiaddrNotSupported(_))
        || address_has_no_transport_in_this_build(address)
}

/// Can this build's transport stack dial `address` at all?
///
/// ASKED OF THE ADDRESS BECAUSE THE ERROR STOPPED ANSWERING. Until the
/// DNS transport was built, libp2p answered an address no transport
/// understood with `TransportError::MultiaddrNotSupported`, and
/// matching that variant was the whole of the structural test. It is
/// not any more: `libp2p-dns` wraps the base transport, converts the
/// inner `MultiaddrNotSupported` into its OWN
/// `dns::Error::MultiaddrNotSupported`, and surfaces that as
/// `TransportError::Other` (`libp2p-dns 0.45.0` `src/lib.rs:298`;
/// `libp2p-core 0.44.0` `transport/boxed.rs:173` then boxes it into an
/// `io::Error`). The kind survives as a VALUE inside
/// `dns::Error<TInner::Error>`, whose generic parameter is the
/// builder's authenticated, multiplexed transport and so cannot be
/// named here to downcast to. Matching the Display string is the trap
/// this repository has already been caught by once.
///
/// So the question is asked directly, which is what
/// [`attempt_is_structural`]'s own documentation always said it was
/// about: "THIS PROCESS's transport stack rather than the remote end's
/// availability" is a fact about our composition, and we compose it.
/// The Swarm builder wraps TCP in DNS resolution and, when a relay
/// client is configured, adds the circuit transport -- so an address
/// naming neither a `tcp` hop nor a `p2p-circuit` can never be dialled
/// by this process, whatever the network does.
///
/// CONSERVATIVE ON PURPOSE. It answers "certainly not" or "do not
/// know", never "certainly yes": a false "certainly not" would drop a
/// usable route out of the address book, which is the more expensive
/// mistake. Anything it is unsure about falls through to the error
/// match above and then to `record_failure`, exactly as before.
fn address_has_no_transport_in_this_build(address: &Multiaddr) -> bool {
    !address.iter().any(|p| {
        matches!(
            p,
            libp2p::multiaddr::Protocol::Tcp(_) | libp2p::multiaddr::Protocol::P2pCircuit
        )
    })
}

/// Whether `error` describes THIS PROCESS's transport stack rather than
/// the remote end's availability.
///
/// `MultiaddrNotSupported` is libp2p's own name for "no configured
/// transport understands this address" -- a UDP address handed to a
/// TCP-only Swarm, for instance. It is not a fact about the network:
/// the same address fails the same way every time, on every attempt,
/// whatever the remote end does. Retrying it is not a smaller version
/// of retrying a timed-out connection; it is retrying a question this
/// process has already answered.
///
/// `DialError::Transport` carries one entry per address the dial
/// considered, so ALL of them must be the structural kind for the whole
/// attempt to be structural -- a mix means at least one address reached
/// the network and failed there, which is the ordinary case
/// `record_failure` exists for.
///
/// THAT AGGREGATE RULE GOVERNS ONE CALLER, and it is worth naming
/// because a second one now answers the same question differently.
/// [`attempt_dial`]'s synchronous-refusal path is the aggregate's: an
/// `AdmittedDial` binds exactly ONE address into its `DialOpts`, so
/// `attempts` is a single entry there and "all" and "the first" are the
/// same claim.
///
/// [`settle_failed_dial`] is where multi-address errors actually
/// arrive, and it does NOT use this arm. Each attempt settles by its
/// own class through the admission-free path, and the ticket takes the
/// class of the attempt it was re-bound to -- the first. The aggregate
/// answer was wrong for that job: it labelled every member of a mixed
/// batch transient, which retried a structural route forever.
pub(super) fn is_permanent_dial_error(error: &DialError) -> bool {
    match error {
        DialError::NoAddresses | DialError::LocalPeerId { .. } => true,
        DialError::Transport(attempts) => {
            !attempts.is_empty()
                && attempts
                    .iter()
                    .all(|(address, e)| attempt_is_structural(address, e))
        }
        _ => false,
    }
}

/// Release the admission a connection outcome belongs to.
///
/// The origin an inbound from `peer` is retained under when this
/// profile is connectivity infrastructure for it, given the connections
/// open; `None` asks the origin-less question.
pub(super) type InfrastructureOrigin<'a> = dyn Fn(
        &TransportIdentity,
        &HashMap<libp2p::swarm::ConnectionId, OpenConnection>,
    ) -> Option<DialOrigin>
    + 'a;

/// What the Identify learn site has done, across the runtime's life.
///
/// ADR-0052 A 2026-09-20 rule 8 makes every path by which a
/// peer-supplied address enters the address book an instance, and
/// Identify's `listen_addrs` is one. These counters are how a refusal
/// is visible at all: rule 5 keeps the address out of logs, so a
/// refused address leaves no other trace.
///
/// `admitted` beside `refused` is deliberate. Without it a quiet peer
/// and a boundary refusing everything read the same, which is the
/// shape that lets a wrong predicate look like a working one.
#[derive(Debug, Default)]
pub(super) struct AdvertisedCounters {
    /// Advertised addresses that passed and reached the book.
    pub admitted: usize,
    /// Those refused, by [`CandidateRefusal::label`].
    pub refused: std::collections::BTreeMap<&'static str, usize>,
}

impl AdvertisedCounters {
    /// Every refusal, whatever its class.
    #[cfg(test)]
    pub(super) fn refused_total(&self) -> usize {
        self.refused.values().sum()
    }
}

/// The boundary as one Identify event sees it: rule 3's input, and
/// where the outcome is filed.
///
/// The listener set is rebuilt per event rather than held, because it
/// changes underneath: a node that binds a private interface after the
/// peer's first Identify must judge the next one against the listeners
/// it has THEN. Holding a snapshot is how rule 3 would silently answer
/// yesterday's question.
pub(super) struct AdvertisedBoundary<'a> {
    /// This node's own bound listeners, for rule 3's
    /// private-with-a-private-listener clause.
    pub own_listeners: &'a [String],
    /// Where the outcome is filed; outlives this event.
    pub counters: &'a mut AdvertisedCounters,
}

impl AdvertisedBoundary<'_> {
    fn refuse(&mut self, class: CandidateRefusal) {
        *self.counters.refused.entry(class.label()).or_default() += 1;
    }
}

/// Put the addresses a peer advertised into the book, minus the ones
/// ADR-0052's boundary refuses.
///
/// A NAMED FUNCTION RATHER THAN A MATCH ARM, so the boundary being
/// WIRED is testable and not only the predicate being right. A correct
/// predicate behind an unwired hook is the exact shape this repository
/// has shipped before -- a helper whose own documentation explained
/// what a caller skipping it would get, called by nothing.
fn learn_advertised(
    manager: &mut ConnectionManager,
    peer: &TransportIdentity,
    advertised: &[Multiaddr],
    boundary: &mut AdvertisedBoundary<'_>,
    now_ms: u64,
) {
    for address in advertised {
        // A peer asserts its own addresses with its own `/p2p/` suffix
        // as often as not, so this is a suffixed input by convention
        // rather than by accident.
        let text = address.to_string();
        // BEFORE THE BOOK, not before a dial: Identify originates none,
        // and the book is what the retry scheduler dials from. A
        // refused address never becomes an entry at all, so there is
        // nothing for a later relaxation to launder.
        if let Err(class) =
            is_advertised_address(&text, boundary.own_listeners.iter().map(String::as_str))
        {
            boundary.refuse(class);
            continue;
        }
        boundary.counters.admitted += 1;
        let _ = learn_route(manager, peer, &text, now_ms);
    }
}

/// The two events that end an outbound attempt are the established
/// connection and the outgoing error. Both carry the `ConnectionId` the
/// dial was built with, which is why the ticket is filed under it: no
/// matching by address, no guessing from a peer that may appear twice.
///
/// An event for a connection this runtime did not dial -- anything
/// inbound -- finds no ticket and does nothing, which is correct rather
/// than merely harmless: inbound connections were never admitted
/// through the dial gate and have no slot to return.
///
/// Its arguments are the Swarm task's per-event state, each owned by a
/// different part of the runtime, and it takes them separately for the
/// reason `commands::handle_command` does: a struct bundling them would
/// exist only to satisfy the argument count, and would hide which of
/// them each arm actually touches.
#[allow(clippy::too_many_arguments)]
pub(super) fn settle_outcome(
    event: &Libp2pSwarmEvent<SubstrateBehaviourEvent>,
    manager: &mut ConnectionManager,
    in_flight: &InFlightTickets,
    open: &mut HashMap<libp2p::swarm::ConnectionId, OpenConnection>,
    refuse: &mut Vec<libp2p::swarm::ConnectionId>,
    infrastructure_origin: &InfrastructureOrigin<'_>,
    boundary: &mut AdvertisedBoundary<'_>,
    now_ms: u64,
) -> Announce {
    match event {
        Libp2pSwarmEvent::ConnectionEstablished {
            connection_id,
            peer_id,
            endpoint,
            ..
        } => {
            let path = path_of(endpoint);
            // The peer is AUTHENTICATED by this point -- Noise has run
            // -- which is what makes classifying it here meaningful and
            // classifying it any earlier impossible.
            let Ok(peer) = to_transport_identity(peer_id) else {
                // A PeerId the neutral grammar rejects cannot be
                // classified, recorded, or revoked later. Refusing is
                // the only answer that does not leave an unaccountable
                // connection open.
                refuse.push(*connection_id);
                return Announce::Suppress;
            };
            match in_flight.settle(*connection_id) {
                // Outbound: the slot was reserved when the dial was
                // admitted, and the connection takes it over.
                Some(ticket) => {
                    match settle_established_outbound(manager, &peer, ticket, path, now_ms) {
                        Some((slot, origin, admitted_class)) => {
                            open.insert(
                                *connection_id,
                                OpenConnection {
                                    peer,
                                    slot,
                                    origin: Some(origin),
                                    admitted_class,
                                    path,
                                    punched: false,
                                    since_ms: now_ms,
                                    retiring: false,
                                },
                            );
                        }
                        None => {
                            refuse.push(*connection_id);
                            return Announce::Suppress;
                        }
                    }
                }
                // INBOUND HAS NO ADMISSION. ADR-0011: the same current
                // authorization that governs outbound applies before an
                // inbound data-plane connection is retained -- arriving
                // is not an authorization. The ceiling is the second
                // question, because a connection this profile will not
                // keep should not spend a slot to find that out.
                None => {
                    let class = manager.classify(&peer);
                    // ROUTE 3 (CLAUDE.md §1), NARROWLY. An AutoNAT server
                    // answers a probe by dialling us back, and that is an
                    // inbound from a peer that may be infrastructure-only
                    // -- a class the origin-less `authorizes` refuses
                    // outright. For a peer the AutoNAT adapter holds as a
                    // server (dialled by this profile under
                    // `AutonatProbe`, advertising the protocol), the
                    // question is asked under that origin instead, and the
                    // connection is retained: class-gated, so it carries
                    // Identify and the autonat protocols and nothing else.
                    // Every other inbound is asked as before, which is the
                    // control `tests/connectivity` keeps green. Keyed on
                    // "is a server this profile holds an outbound to"
                    // rather than "has a probe outstanding" because the
                    // crate emits no probe-start event and nothing tracks
                    // probes in flight (owner, 2026-09-17) -- and on the
                    // outbound, not on the server set alone, so a server
                    // that went away does not keep the door open (round 4).
                    //
                    // AND THE SERVER ROLE WIDENS IT (step 4). When this
                    // profile SERVES probes, every authorized peer may
                    // ask, and a request arrives on the asker's inbound:
                    // the closure answers true for every peer then, so
                    // an infrastructure-only client's inbound is retained
                    // under `AutonatProbe` -- class-gated, offered
                    // Identify and the dial-request protocol and nothing
                    // else. `Unauthorized` is refused under either origin.
                    // `tests/connectivity/tests/autonat_server.rs` pins
                    // the three classes with the server on, and the
                    // established-then-closed control with it off.
                    //
                    // AND THE RELAY SERVER WIDENS IT THE SAME WAY (step
                    // 6): a reservation rides the requester's inbound,
                    // so with `relay_server` configured every authorized
                    // inbound is retained under `RelayReservation` --
                    // class-gated, offered Identify and the hop protocol
                    // and nothing else. The closure names the origin;
                    // `tests/connectivity/tests/relay_server.rs` pins it.
                    //
                    // EXCEPT A RELAYED INBOUND (step 7). A circuit that
                    // arrives through this profile's reservation is an
                    // APPLICATION path whose far end is the source: it
                    // is asked under `RelayCircuit` (`retention_origin`),
                    // which admits only a data-plane peer, whatever the
                    // closure would say -- else a relay or AutoNAT server
                    // would retain an infrastructure-only source over a
                    // circuit, the row ADR-0036's amendment forbids.
                    // `tests/connectivity/tests/relayed_paths.rs` pins it
                    // with the servers on.
                    let asked_under = match path {
                        PeerPath::Relayed => Some(DialOrigin::RelayCircuit),
                        PeerPath::Direct => infrastructure_origin(&peer, open),
                    };
                    match settle_established_inbound(
                        manager,
                        peer,
                        class,
                        path,
                        asked_under,
                        now_ms,
                    ) {
                        Some(connection) => {
                            open.insert(*connection_id, connection);
                        }
                        None => {
                            refuse.push(*connection_id);
                            return Announce::Suppress;
                        }
                    }
                }
            }
        }
        Libp2pSwarmEvent::ConnectionClosed { connection_id, .. } => {
            // The other half of the pair, and only for a connection
            // that was actually counted: a refused inbound reports a
            // close too, and releasing a slot it never held would let
            // the ceiling drift upward one refusal at a time.
            //
            // The SAME condition decides whether to announce it. A
            // connection this runtime refused was never announced as
            // `Connected`, so announcing its close would hand a
            // consumer a `Disconnected` for a peer it was never told
            // about -- which reads as a peer going away rather than as
            // one that was never admitted.
            let Some(connection) = open.remove(connection_id) else {
                return Announce::Suppress;
            };
            manager.record_connection_closed(connection.slot);
        }
        Libp2pSwarmEvent::OutgoingConnectionError {
            connection_id,
            error,
            ..
        } => {
            if let Some(ticket) = in_flight.settle(*connection_id) {
                settle_failed_dial(manager, ticket, error, now_ms);
            }
        }
        // ADVISORY, and bounded. These are addresses the peer asserted
        // about itself: not authorization, not proof of reachability,
        // and not permission to dial -- every dial still passes
        // admission. Remembered only for a peer the trust sources
        // classify, and at most eight of them, because the list is
        // written by the party being described.
        //
        // AND INSIDE ADR-0052'S BOUNDARY, which this path had no hook
        // for until A 2026-09-20. A `listen_addr` is peer-supplied by
        // rule 1's own words -- the peer chose it, this node dials it
        // -- but it enters the BOOK rather than a dial, so every
        // earlier instance, which hooked a dial, passed over it. The
        // build could not dial the interesting half anyway: a
        // `/dns4/` name failed `MultiaddrNotSupported` and was
        // evicted, so the refusal looked like a rule while it was an
        // accident of `with_tcp` alone. Building the DNS transport
        // removed the accident and left the rule to be written.
        Libp2pSwarmEvent::Behaviour(SubstrateBehaviourEvent::Identify(
            identify::Event::Received { peer_id, info, .. },
        )) => {
            if let Ok(peer) = to_transport_identity(peer_id) {
                learn_advertised(manager, &peer, &info.listen_addrs, boundary, now_ms);
            }
        }
        _ => {}
    }
    Announce::Yes
}

/// Whether the event this runtime just settled should be reported to
/// the consumer.
///
/// A connection REFUSED at establishment -- authorization withdrawn
/// mid-handshake, an inbound peer this profile will not retain, a
/// ceiling with no room, a PeerId the neutral grammar rejects -- was
/// settled and queued for closing, but `translate` is a pure shape
/// conversion and would happily emit `Connected` for it anyway. A
/// consumer would then see a peer become available and start work
/// against it, moments before the close it was never told was coming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Announce {
    /// Report it: the ordinary case.
    Yes,
    /// Say nothing. This connection is not one the consumer was told
    /// about, and telling it now would describe a state that never
    /// existed.
    Suppress,
}

/// Milliseconds since the runtime task started.
///
/// Monotonic and relative. The policy is a state machine over elapsed
/// time, so an origin of zero is as good as any epoch and immune to a
/// wall-clock adjustment moving a quarantine deadline.
/// Unix-epoch milliseconds.
///
/// Distinct from [`now_ms`], and both exist because they answer
/// different questions. This one can step backwards — NTP, an operator
/// — so nothing that must not go backwards may read it: rate buckets,
/// dedup TTLs and deadlines all use the monotonic clock. What it is for
/// is a RECEIPT TIME, which has to survive a restart and order against
/// another process lifetime.
///
/// A clock before 1970 is not a receipt time either, so the error case
/// answers zero rather than panicking in a transport daemon.
pub(super) fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

pub(super) fn now_ms(started: tokio::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Which open connections a trust change actually withdraws.
///
/// THE NEW CLASS, PER CONNECTION, AGAINST ITS OWN ORIGIN. ADR-0036's
/// separation is an origin/class PAIR, so this cannot be decided from
/// the class alone. A peer in both trust sets that loses only its
/// data-plane trust is still infrastructure: `set_trust` reports it
/// revoked, while `authorizes_for` goes on permitting its relay
/// reservation, relay circuit and AutoNAT probes. Closing every
/// connection to a reported peer dropped exactly those -- the
/// reachability that peer is still trusted for.
///
/// Inbound carries no origin because arriving is not a dial. It was
/// admitted by the origin-less `authorizes` -- or, for an AutoNAT
/// server's inbound, under `AutonatProbe` (route 3), or for a relayed
/// one under `RelayCircuit` (step 7) -- and is re-asked the origin-less
/// question here. That is stricter for the server case,
/// deliberately: a revocation that reaches the data plane still closes
/// it. A server whose infrastructure trust is unchanged by a trust
/// change is not in `revoked` (`permits(Infra, Infra)` holds) and is
/// left alone; one that loses that trust IS listed
/// (`permits(Unauthorized, Infra)` does not hold) and closes here, and
/// so does one demoted from data-plane trust, both because the
/// origin-less question refuses their class -- after which the adapter
/// re-dials a static one and the retention arm decides afresh. Pinned
/// by `a_server_that_loses_its_infrastructure_trust_is_closed`.
pub(super) fn connections_to_close<'a>(
    manager: &ConnectionManager,
    revoked: &[Revoked],
    open: impl Iterator<
        Item = (
            libp2p::swarm::ConnectionId,
            &'a TransportIdentity,
            Option<DialOrigin>,
            ConnectionClass,
        ),
    >,
) -> BTreeSet<libp2p::swarm::ConnectionId> {
    let revoked_class: BTreeMap<&TransportIdentity, ConnectionClass> = revoked
        .iter()
        .map(|entry| (&entry.peer, entry.now))
        .collect();

    let mut closing = BTreeSet::new();
    for (id, peer, origin, admitted_class) in open {
        let Some(now) = revoked_class.get(peer) else {
            continue;
        };
        let still_authorized = match origin {
            Some(origin) => manager.authorizes_for(*now, origin),
            None => manager.authorizes(*now),
        };
        // AUTHORIZATION IS NOT THE ONLY REASON TO CLOSE, and this is the
        // second one: a connection's PROTOCOL SET is decided once, by
        // `ClassGated`, from the class the peer held when the connection
        // was admitted -- and libp2p never rebuilds a handler. A
        // connection admitted while the peer was data-plane trusted
        // therefore carries every data-plane protocol for its whole
        // life, and the origin check above would keep exactly such a
        // connection when the peer is demoted to infrastructure-only,
        // because a reservation with an infrastructure peer is what
        // should survive.
        //
        // ADR-0036 settles it rather than this file inventing an answer:
        // "If atomic in-place reconciliation is not safe in the pinned
        // library, close the connection and re-establish it under the
        // new class rather than allowing a transient privilege mix."
        // Whatever wanted the reachability connection re-establishes it,
        // correctly gated.
        //
        // REACHABLE SINCE STEP 3'S ADAPTER, and the distinction matters
        // for reading this. `now` is never `DataPlaneTrusted` here —
        // `permits` admits every promotion — so `gating_changed` reduces
        // to `admitted_class == DataPlaneTrusted`, and a non-DPT
        // `admitted_class` requires a RETAINED infrastructure-only
        // connection: an AutoNAT server's inbound retained under
        // `AutonatProbe` (the route-3 arm above), or its outbound
        // dialled under that origin. Such a connection appears here
        // when its peer is in `revoked` -- it lost data-plane trust, or
        // it lost the infrastructure trust it held -- and in both cases
        // `still_authorized` is what closes it, since `gating_changed`
        // is false for a class that was never data-plane. Until the
        // adapter landed this branch was reachable by a TEST alone.
        //
        // THE COMPARISON IS AGAINST `admitted_class`, NOT `Revoked::was`,
        // and that is what keeps the origin check alive. `was` is the
        // class before the latest change, which for a peer admitted
        // while infrastructure-only, promoted, then demoted again says
        // `DataPlaneTrusted` -- while the connection has carried a
        // denying handler throughout. Judging from `was` closed that
        // connection too, and closed every revoked connection whatever
        // its origin, which left `OpenConnection::origin` deciding
        // nothing at all. Review finding on PR #77.
        //
        // DECIDED HERE rather than inside `ClassGated`, so that
        // `set_trust`'s count includes it. That count is ADR-0012's
        // observable -- "a revocation whose only effect was on the next
        // dial would leave the revoked peer connected" -- and a closure
        // the wrapper performed on its own would be invisible to it.
        let gating_changed = (admitted_class == ConnectionClass::DataPlaneTrusted)
            != (*now == ConnectionClass::DataPlaneTrusted);
        if !still_authorized || gating_changed {
            closing.insert(id);
        }
    }
    closing
}

/// A connection this process holds open.
///
/// The slot is the accounting; the peer is what makes a revocation
/// actionable. Kept together because releasing one without the other is
/// exactly the drift that turns a ceiling into a leak.
#[derive(Debug)]
pub(super) struct OpenConnection {
    pub(super) peer: TransportIdentity,
    pub(super) slot: ConnectionSlot,
    /// The class this profile granted the peer WHEN THE CONNECTION WAS
    /// ADMITTED, which is what its protocol set was chosen from.
    ///
    /// `ClassGated` decides at establishment whether a connection is
    /// offered the data-plane behaviours, and libp2p never rebuilds a
    /// handler — so whether a connection's protocols have gone stale is
    /// a question about the class it was ADMITTED under, not about the
    /// class the peer held before the latest trust change.
    ///
    /// Those two differ, which is the whole reason this field exists
    /// rather than reading `Revoked::was`. A peer admitted while
    /// infrastructure-only, promoted, then demoted again reports
    /// `was = DataPlaneTrusted` for that last change while its
    /// connection has carried a denying handler throughout: nothing is
    /// stale, and closing it would drop reachability for no reason.
    ///
    /// **THIS IS A SECOND CLASSIFICATION, and it agrees with
    /// `ClassGated`'s only because of where the two sit in the loop.**
    /// The wrapper classifies inside its established hook, which the
    /// Swarm calls while `select_next_some()` is being polled; this
    /// value is taken in the arm body that handles the
    /// `ConnectionEstablished` the same poll returned. One `select!`
    /// iteration, no await between them, so no `SetTrust` command can be
    /// processed in the gap and the two readings cannot disagree.
    ///
    /// That is a property of the runtime loop rather than of this
    /// struct, and it is stated here because nothing else would say it:
    /// move the classification to a later iteration, or add an await,
    /// and a connection could be recorded under a class it was not
    /// gated on -- which decides wrongly at the next trust change, in
    /// silence.
    pub(super) admitted_class: ConnectionClass,
    /// Why this connection was opened, or `None` for one that arrived.
    ///
    /// ADR-0036's separation is an origin/class PAIR, so a trust change
    /// cannot be re-evaluated from the class alone. Without this a peer
    /// that lost only its data-plane trust -- still infrastructure --
    /// had every connection to it closed, including relay reservations
    /// and AutoNAT probes that `authorizes_for` would still permit.
    ///
    /// Inbound is `None` because arriving is not a dial -- a relayed
    /// inbound too, though its retention was asked under `RelayCircuit`
    /// (`retention_origin`): it was admitted with the origin-less
    /// `authorizes` or under an origin the closure named, and it is
    /// re-evaluated with the origin-less question, which agrees with
    /// `RelayCircuit` for every class. The AutoNAT adapter reads
    /// `Some` as "this profile dialled it" and installs its probe
    /// handler on exactly those, so a stored origin here would turn a
    /// server that dialled us over a circuit into one we believe we can
    /// probe (PR #101 round 1).
    /// `an_inbound_is_recorded_origin_less_whatever_it_was_retained_under`
    /// pins the record, and the adapter's
    /// `a_server_is_offered_only_when_dialled_and_advertising_the_protocol`
    /// is fed that shape.
    pub(super) origin: Option<DialOrigin>,
    /// Whether this connection runs over a relay's circuit, read from
    /// the endpoint at establishment: it decides the peer's path
    /// (`contracts/CONNECTIVITY.md` §5) and, for an inbound, the origin
    /// the retention question is asked under.
    pub(super) path: PeerPath,
    /// Whether this connection's establishment ended a DCUtR attempt
    /// (step 8's `take_punched`): a punched direct connection is the
    /// peer's path only once it has held for the stability interval
    /// (step 9), and the move it makes is a `HolePunched`.
    pub(super) punched: bool,
    /// When it was established, on the runtime's clock: the stability
    /// interval is measured from here.
    pub(super) since_ms: u64,
    /// Whether the runtime has already asked the Swarm to close it as
    /// a redundant relayed connection (step 9's retirement). A close
    /// is a request to the connection task, not the closure: until
    /// `ConnectionClosed` removes it from the open set the tick would
    /// find it retirable again and announce the one retirement once
    /// per tick (PR #103 round 1). `retirable`'s reading of the flag is
    /// pinned by
    /// `a_relayed_connection_is_retired_only_behind_a_stable_direct_and_only_when_safe`;
    /// the runtime SETTING it is not observable on one host, where a
    /// circuit's close completes within a tick (`dcutr.rs`'s punch test
    /// asserts the once-only report but passes without the flag).
    pub(super) retiring: bool,
}

impl OpenConnection {
    /// This connection as the path derivation reads it at `now_ms`,
    /// under `stability_ms`.
    pub(super) const fn sample(&self, now_ms: u64, stability_ms: u64) -> PathSample {
        PathSample {
            path: self.path,
            punched: self.punched,
            stable: !self.punched || now_ms.saturating_sub(self.since_ms) >= stability_ms,
        }
    }

    /// Whether this is a direct connection that CARRIES THE DATA PLANE
    /// -- admitted under `DataPlaneTrusted`, so its handler set offers
    /// the application protocols. `DialPeer` reuses such a connection
    /// instead of dialling and the head-start race is won by one
    /// (`transport/libp2p/CONNECTIVITY.md` §12, step 9); a direct
    /// connection to an infrastructure-only peer -- the relay this
    /// profile reserves on, an AutoNAT server it probes -- is direct
    /// and class-gated to no data-plane protocol at all, so answering
    /// a `DialPeer` with it would report a data-plane path that does
    /// not exist where the gate used to refuse `NotAuthorizedForDataPlane`
    /// (PR #103 round 1). Pinned by `a_connection_reused_for_dial_peer_
    /// carries_the_data_plane` and, on the wire, by
    /// `tests/connectivity/tests/path_race.rs`'s
    /// `an_infrastructure_only_peers_direct_connection_is_not_reused_for_the_data_plane`.
    #[must_use]
    pub(super) fn is_direct_data_plane(&self) -> bool {
        self.path == PeerPath::Direct
            && self.admitted_class
                == interweave_transport_runtime::ConnectionClass::DataPlaneTrusted
    }
}

/// The origin a command-path dial is judged under: `RelayCircuit` for
/// an address through a relay, so the far end is judged as an
/// APPLICATION destination -- an infrastructure-only one refused (D2)
/// -- and the gate's pairing of origin and address holds; `Manual` for
/// a direct one. Pinned by `a_circuit_address_from_a_command_is_a_relay_circuit_dial`.
#[must_use]
pub(super) fn command_origin(address: &Multiaddr) -> DialOrigin {
    origin_for(address, DialOrigin::Manual)
}

/// `RelayCircuit` for an address through a relay, `otherwise` for a
/// direct one: the one classification every caller that dials from
/// the address BOOK goes through -- `DialPeer` and the retry scheduler
/// alike -- because the gate's pairing check refuses a circuit address
/// under any other origin and the refusal forgets the route as a
/// structural failure (PR #101 round 1: the scheduler dialled a learned
/// circuit route under `ConnectionManager`, and the first transient
/// failure of a circuit scrubbed it). `a_circuit_route_that_failed_is_
/// retried_as_a_relay_circuit` pins the scheduler's half.
#[must_use]
pub(super) fn origin_for(address: &Multiaddr, otherwise: DialOrigin) -> DialOrigin {
    if address
        .iter()
        .any(|p| matches!(p, libp2p::multiaddr::Protocol::P2pCircuit))
    {
        DialOrigin::RelayCircuit
    } else {
        otherwise
    }
}

/// [`origin_for`] over the book's string form; an address the book
/// holds that does not parse is dialled as `otherwise`, and fails as
/// it always did.
#[must_use]
pub(super) fn book_origin(address: &str, otherwise: DialOrigin) -> DialOrigin {
    address
        .parse::<Multiaddr>()
        .map_or(otherwise, |a| origin_for(&a, otherwise))
}

/// The origin a connection's retention is asked under: `RelayCircuit`
/// for one that runs over a circuit, whatever dialled it or answered
/// it -- the far end of a circuit is an application destination and
/// only a data-plane peer may be reached over one (ADR-0036's
/// amendment) -- and the dial's own origin otherwise.
#[must_use]
pub(super) const fn retention_origin(path: PeerPath, dialled: DialOrigin) -> DialOrigin {
    match path {
        PeerPath::Relayed => DialOrigin::RelayCircuit,
        PeerPath::Direct => dialled,
    }
}

/// The path a connection runs over, from its endpoint.
#[must_use]
pub(super) fn path_of(endpoint: &libp2p::core::ConnectedPoint) -> PeerPath {
    if endpoint.is_relayed() {
        PeerPath::Relayed
    } else {
        PeerPath::Direct
    }
}

/// One open connection as the path derivation reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PathSample {
    pub(super) path: PeerPath,
    /// Whether this connection's establishment ended a DCUtR attempt
    /// (the wrapper's `take_punched`).
    pub(super) punched: bool,
    /// Whether a punched direct connection has held for the stability
    /// interval (`DCUTR.md` §4, `contracts/CONNECTIVITY.md` §5): until
    /// it has, it is a CANDIDATE, ranked below a relayed connection so
    /// the relay stays the announced path -- and above nothing, since a
    /// candidate alone is still a direct connection to the peer. Always
    /// true for an unpunched connection.
    pub(super) stable: bool,
}

impl PathSample {
    /// Where this connection ranks: a stable direct first, a relayed
    /// second, a punched direct still in its stability interval last.
    const fn rank(self) -> u8 {
        match (self.path, self.stable) {
            (PeerPath::Direct, true) => 2,
            (PeerPath::Relayed, _) => 1,
            (PeerPath::Direct, false) => 0,
        }
    }
}

/// The best path to `peer` over the connections `open`, with whether
/// the connection providing it is a punched one: a stable direct if
/// any, else relayed if any, else a punched direct still stabilising,
/// else none. Pinned by `path_events_are_once_per_logical_peer` and
/// `a_punched_direct_ranks_below_the_relay_until_it_is_stable`.
#[must_use]
pub(super) fn best_path<'a>(
    open: impl Iterator<Item = (&'a TransportIdentity, PathSample)>,
    peer: &TransportIdentity,
) -> Option<PathSample> {
    open.filter(|(p, _)| *p == peer)
        .map(|(_, sample)| sample)
        .max_by_key(|sample| (sample.rank(), !sample.punched))
}

/// The events a change of `peer`'s best path owes the consumer
/// (`contracts/CONNECTIVITY.md` §5): `Connected` once when the first
/// usable connection appears, `PeerPathChanged` when the best path
/// moves while the peer stays connected, `Disconnected` once when the
/// last goes; nothing when nothing changed. `open` is every connection
/// still open, as `best_path` reads it; `paths` is the last best path
/// announced per peer, kept by the caller and updated here. A move to
/// direct provided by a punched connection is a `HolePunched`, and it
/// happens only once that connection is stable (step 9's gate over
/// step 8's punch); by any other direct connection a
/// `DirectEstablished`. Pinned by `path_events_are_once_per_logical_peer`.
pub(super) fn path_events<'a>(
    open: impl Iterator<Item = (&'a TransportIdentity, PathSample)>,
    paths: &mut HashMap<TransportIdentity, PeerPath>,
    peer: &TransportIdentity,
) -> Option<SwarmEvent> {
    let now = best_path(open, peer);
    let before = paths.get(peer).copied();
    match (before, now) {
        (None, Some(sample)) => {
            paths.insert(peer.clone(), sample.path);
            Some(SwarmEvent::Connected {
                peer: peer.clone(),
                path: sample.path,
            })
        }
        (Some(_), None) => {
            paths.remove(peer);
            Some(SwarmEvent::Disconnected { peer: peer.clone() })
        }
        (Some(previous), Some(sample)) if previous != sample.path => {
            paths.insert(peer.clone(), sample.path);
            Some(SwarmEvent::PeerPathChanged {
                peer: peer.clone(),
                previous,
                current: sample.path,
                reason: match (sample.path, sample.punched) {
                    (PeerPath::Direct, true) => PathChange::HolePunched,
                    (PeerPath::Direct, false) => PathChange::DirectEstablished,
                    (PeerPath::Relayed, _) => PathChange::DirectLost,
                },
            })
        }
        _ => None,
    }
}

/// The relayed connections to `peer` that are redundant and safe to
/// retire (`transport/libp2p/CONNECTIVITY.md` §13's last arrow and
/// §12's lost race, step 9): every relayed one not already asked to
/// close (`retiring`), when the peer's best path is a STABLE direct
/// connection -- a punched one past its interval, or a dialled or
/// accepted one, stable from its handshake -- and `awaiting` --
/// whether an exchange this profile started with the peer still awaits
/// its answer -- is false; nothing otherwise. Read as "any stable
/// direct" rather than "the punched one" because a second direct
/// connection beside the punch -- the far end's crate retrying its
/// stalled punch dial after the attempt ended, measured with a bare
/// far end -- provides the path over the punched one and would
/// otherwise keep the redundant relayed connection open for as long
/// as the peer is in use, since request-response spreads streams over
/// every connection to a peer (PR #103 round 1). Pinned by
/// `a_relayed_connection_is_retired_only_behind_a_stable_direct_and_only_when_safe`.
#[must_use]
pub(super) fn retirable<'a>(
    open: impl Iterator<
        Item = (
            libp2p::swarm::ConnectionId,
            &'a TransportIdentity,
            PathSample,
            bool,
        ),
    > + Clone,
    peer: &TransportIdentity,
    awaiting: bool,
) -> Vec<libp2p::swarm::ConnectionId> {
    if awaiting {
        return Vec::new();
    }
    let preferred_direct = best_path(open.clone().map(|(_, p, s, _)| (p, s)), peer)
        .is_some_and(|s| s.stable && s.path == PeerPath::Direct);
    if !preferred_direct {
        return Vec::new();
    }
    open.filter(|(_, p, s, retiring)| *p == peer && s.path == PeerPath::Relayed && !retiring)
        .map(|(id, _, _, _)| id)
        .collect()
}

/// Listen commands whose bound address has not arrived yet.
pub(super) type PendingListens = HashMap<ListenerId, oneshot::Sender<Result<Multiaddr, String>>>;

/// Listeners that have bound and are still serving.
///
/// The runtime used to forget a listener the moment its `listen` reply
/// was answered, and every listener defect followed from that single
/// omission: nothing could bound how many were open, nothing could close
/// one, and a listener dying after it bound was reported to no one.
///
/// Keyed by id and holding every address that listener bound, because a
/// caller names one by an address `listen` handed back.
pub(super) type ActiveListeners = HashMap<ListenerId, Vec<Multiaddr>>;

#[cfg(test)]
mod tests {
    use super::{
        AdvertisedBoundary, AdvertisedCounters, OpenConnection, PathSample, best_path, book_origin,
        canonical_dial_address, command_origin, connections_to_close, is_permanent_dial_error,
        learn_advertised, learn_route, path_events, retirable, settle_established_inbound,
        settle_established_outbound, settle_failed_dial, settle_undialable,
    };
    use crate::gated_swarm::AdmittedDial;
    use crate::runtime::messages::{PathChange, PeerPath, SwarmEvent};
    use interweave_transport_api::TransportIdentity;
    use interweave_transport_runtime::{ConnectionClass, DialRequest, DialTicket};
    use interweave_transport_runtime::{
        ConnectionManager, ConnectionPolicy, DialOrigin, TrustSources,
    };
    use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
    use libp2p::Multiaddr;
    use libp2p::core::transport::TransportError;
    use libp2p::swarm::{ConnectionId, DialError};
    use std::collections::HashMap;

    const RELAY: &str = "12D3KooWCLxLXFHqvfsHVLDcNsSpZBQq1M1KMRgQRLLLnHTv7oQD";
    /// A peer at the far end of a circuit through `RELAY`.
    const FAR: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTA";

    fn ident(text: &str) -> TransportIdentity {
        TransportIdentity::parse(text).expect("a valid peer id")
    }

    fn manager(data_plane: &[&str], infrastructure: &[&str]) -> ConnectionManager {
        let mut m = ConnectionManager::new(ConnectionPolicy::default(), 8);
        m.set_trust(trust(data_plane, infrastructure), &[]);
        m
    }

    fn trust(data_plane: &[&str], infrastructure: &[&str]) -> TrustSources {
        TrustSources::new(
            PeerTrustPolicy::new(data_plane.iter().map(|p| ident(p))).expect("small"),
            InfrastructureSet::new(infrastructure.iter().map(|p| ident(p))).expect("small"),
        )
    }

    /// The origin a command's dial is judged under is decided by the
    /// ADDRESS: through a relay it is a `RelayCircuit`, so the far end
    /// is an application destination and an infrastructure-only one is
    /// refused (D2) before any socket. The control is the same address
    /// without the circuit marker, which stays `Manual`.
    #[test]
    fn a_circuit_address_from_a_command_is_a_relay_circuit_dial() {
        let circuit: Multiaddr =
            format!("/ip4/10.0.0.1/tcp/4001/p2p/{RELAY}/p2p-circuit/p2p/{FAR}")
                .parse()
                .expect("a circuit address");
        assert_eq!(command_origin(&circuit), DialOrigin::RelayCircuit);
        // The marker alone, however the relay is named.
        let bare: Multiaddr = "/p2p-circuit".parse().expect("the marker alone");
        assert_eq!(command_origin(&bare), DialOrigin::RelayCircuit);
        // THE CONTROL: the same relay address with no circuit is a
        // direct dial of the relay itself.
        let direct: Multiaddr = format!("/ip4/10.0.0.1/tcp/4001/p2p/{RELAY}")
            .parse()
            .expect("a direct address");
        assert_eq!(command_origin(&direct), DialOrigin::Manual);

        // AND THE GATE REFUSES THE CIRCUIT TO AN INFRASTRUCTURE-ONLY
        // FAR END under that origin, where the same circuit to a
        // data-plane peer is admitted: the pairing this classification
        // exists to reach. `RELAY` is data-plane trusted here and `FAR`
        // infrastructure-only; the relay in the address is irrelevant
        // to the gate, which judges the far end.
        let mut m = ConnectionManager::new(ConnectionPolicy::new(8, 8), 8);
        m.set_trust(trust(&[RELAY], &[FAR]), &[]);
        let request = |peer: &str, address: &Multiaddr| DialRequest {
            peer: Some(ident(peer)),
            address: address.to_string(),
            origin: command_origin(address),
        };
        let denied = m.handle().admit(&request(FAR, &circuit), 1);
        assert!(
            denied.is_err(),
            "a circuit terminating at an infrastructure-only peer is refused: {denied:?}"
        );
        let through_far: Multiaddr =
            format!("/ip4/10.0.0.2/tcp/4001/p2p/{FAR}/p2p-circuit/p2p/{RELAY}")
                .parse()
                .expect("a circuit address");
        let admitted = m.handle().admit(&request(RELAY, &through_far), 1);
        assert!(
            admitted.is_ok(),
            "a circuit terminating at a data-plane peer is admitted: {admitted:?}"
        );
        // THE CONTROL: the infrastructure-only peer's own address under
        // `Manual` is refused as it always was, so the circuit refusal
        // above is the origin's rule and not a widening of it.
        let manual: Multiaddr = format!("/ip4/10.0.0.2/tcp/4001/p2p/{FAR}")
            .parse()
            .expect("a direct address");
        assert!(m.handle().admit(&request(FAR, &manual), 1).is_err());
    }

    /// A connection as the derivation reads it: direct or relayed, an
    /// ordinary one.
    const fn plain(path: PeerPath) -> PathSample {
        PathSample {
            path,
            punched: false,
            stable: true,
        }
    }

    /// `Connected` once when a peer's first connection opens, nothing
    /// for a second on the same path, `PeerPathChanged` when a direct
    /// connection joins a relayed one and when the last direct one
    /// leaves, `Disconnected` once when the last of any path closes
    /// (`contracts/CONNECTIVITY.md` §5); a punched direct connection
    /// names the move `HolePunched` -- once it is stable, and a loss is
    /// a loss whatever the flag says. Another peer's connections are
    /// the control: they never move this peer's answer.
    #[test]
    fn path_events_are_once_per_logical_peer() {
        let peer = ident(RELAY);
        let other = ident(FAR);
        let mut paths = HashMap::new();
        // (peer, sample) per open connection, as the runtime's open set
        // reads; `other` is connected throughout.
        let events = |open: &[(&TransportIdentity, PathSample)],
                      paths: &mut HashMap<TransportIdentity, PeerPath>| {
            path_events(open.iter().copied(), paths, &peer)
        };
        let relayed = plain(PeerPath::Relayed);
        let direct = plain(PeerPath::Direct);

        // The first connection is relayed: Connected{Relayed}.
        let open = [(&other, direct), (&peer, relayed)];
        assert_eq!(
            events(&open, &mut paths),
            Some(SwarmEvent::Connected {
                peer: peer.clone(),
                path: PeerPath::Relayed
            })
        );
        // A second relayed connection: nothing.
        let open = [(&other, direct), (&peer, relayed), (&peer, relayed)];
        assert_eq!(
            events(&open, &mut paths),
            None,
            "a second connection on the same path is silent"
        );
        // A direct one joins: the path moves up.
        let open = [(&other, direct), (&peer, relayed), (&peer, direct)];
        assert_eq!(
            events(&open, &mut paths),
            Some(SwarmEvent::PeerPathChanged {
                peer: peer.clone(),
                previous: PeerPath::Relayed,
                current: PeerPath::Direct,
                reason: PathChange::DirectEstablished,
            })
        );
        // The relayed one closes under it: nothing, direct is still best.
        let open = [(&other, direct), (&peer, direct)];
        assert_eq!(
            events(&open, &mut paths),
            None,
            "losing the worse path is silent"
        );
        // A relayed one returns and the direct one goes: the path moves down.
        let open = [(&other, direct), (&peer, relayed)];
        assert_eq!(
            events(&open, &mut paths),
            Some(SwarmEvent::PeerPathChanged {
                peer: peer.clone(),
                previous: PeerPath::Direct,
                current: PeerPath::Relayed,
                reason: PathChange::DirectLost,
            })
        );
        // A PUNCHED direct one joins, still stabilising: nothing -- the
        // relay stays the announced path (step 9's gate).
        let punched_young = PathSample {
            path: PeerPath::Direct,
            punched: true,
            stable: false,
        };
        let open = [(&other, direct), (&peer, relayed), (&peer, punched_young)];
        assert_eq!(
            events(&open, &mut paths),
            None,
            "a punched direct connection is a candidate until it is stable"
        );
        // It becomes stable: the move is named the punch (step 8).
        let punched_stable = PathSample {
            stable: true,
            ..punched_young
        };
        let open = [(&other, direct), (&peer, relayed), (&peer, punched_stable)];
        assert_eq!(
            events(&open, &mut paths),
            Some(SwarmEvent::PeerPathChanged {
                peer: peer.clone(),
                previous: PeerPath::Relayed,
                current: PeerPath::Direct,
                reason: PathChange::HolePunched,
            })
        );
        // And losing it again is a loss whatever the flag says.
        let open = [(&other, direct), (&peer, relayed)];
        assert!(matches!(
            events(&open, &mut paths),
            Some(SwarmEvent::PeerPathChanged {
                reason: PathChange::DirectLost,
                ..
            })
        ));
        // The last closes: Disconnected once.
        let open = [(&other, direct)];
        assert_eq!(
            events(&open, &mut paths),
            Some(SwarmEvent::Disconnected { peer: peer.clone() })
        );
        // And asked again with nothing open for it: nothing -- a
        // connection refused at establishment reaches here with no
        // entry and is never announced as a disconnection.
        assert_eq!(
            events(&open, &mut paths),
            None,
            "a peer never announced is never disconnected"
        );
        assert!(
            !paths.contains_key(&peer),
            "the record is dropped with the last connection"
        );
        // THE CONTROL: `other` was open throughout and was never
        // announced, because nothing asked about it.
        assert!(!paths.contains_key(&other));
    }

    /// The retirement: the relayed connections to a peer whose best path
    /// is a stable direct one -- punched past its interval, or dialled
    /// -- when nothing awaits an answer; nothing behind a young punched
    /// one, or while an exchange is in flight; and never another peer's.
    #[test]
    fn a_relayed_connection_is_retired_only_behind_a_stable_direct_and_only_when_safe() {
        let peer = ident(RELAY);
        let other = ident(FAR);
        let id = ConnectionId::new_unchecked;
        let relayed = plain(PeerPath::Relayed);
        let stable_punch = PathSample {
            path: PeerPath::Direct,
            punched: true,
            stable: true,
        };
        let young_punch = PathSample {
            stable: false,
            ..stable_punch
        };
        let dialled = plain(PeerPath::Direct);
        let open = [
            (id(1), &peer, relayed, false),
            (id(2), &peer, stable_punch, false),
            (id(3), &other, relayed, false),
        ];
        assert_eq!(retirable(open.iter().copied(), &peer, false), vec![id(1)]);
        // ASKED TO CLOSE ONCE: a relayed connection already retiring is
        // not retired again on the next tick while the Swarm completes
        // the close.
        let closing = [
            (id(1), &peer, relayed, true),
            (id(2), &peer, stable_punch, false),
            (id(5), &peer, relayed, false),
        ];
        assert_eq!(
            retirable(closing.iter().copied(), &peer, false),
            vec![id(5)],
            "only the one not yet asked"
        );
        assert!(
            retirable(open.iter().copied(), &peer, true).is_empty(),
            "not while an exchange awaits its answer"
        );
        assert!(
            retirable(open.iter().copied(), &other, false).is_empty(),
            "the other peer's relayed connection has no direct beside it"
        );
        let open = [
            (id(1), &peer, relayed, false),
            (id(2), &peer, young_punch, false),
        ];
        assert!(
            retirable(open.iter().copied(), &peer, false).is_empty(),
            "not behind a punched direct still stabilising"
        );
        let open = [
            (id(1), &peer, relayed, false),
            (id(2), &peer, dialled, false),
        ];
        assert_eq!(
            retirable(open.iter().copied(), &peer, false),
            vec![id(1)],
            "behind a dialled direct too: a lost race's circuit, or a dial while relayed"
        );
        let open = [
            (id(1), &peer, relayed, false),
            (id(2), &peer, dialled, false),
            (id(4), &peer, stable_punch, false),
        ];
        assert_eq!(
            retirable(open.iter().copied(), &peer, false),
            vec![id(1)],
            "a dialled direct beside the punch provides the path, and the relayed is still redundant"
        );
        let open = [
            (id(1), &peer, relayed, false),
            (id(2), &peer, dialled, false),
            (id(4), &peer, young_punch, false),
        ];
        assert_eq!(
            retirable(open.iter().copied(), &peer, false),
            vec![id(1)],
            "the dialled direct is the stable path whatever the punch's age"
        );
    }

    /// A punched direct connection ranks below a relayed one until it
    /// is stable, and above nothing: alone, it is the peer's path from
    /// the start -- there is no relay preference to retain -- and the
    /// move is still the punch's.
    #[test]
    fn a_punched_direct_ranks_below_the_relay_until_it_is_stable() {
        let peer = ident(RELAY);
        let young = PathSample {
            path: PeerPath::Direct,
            punched: true,
            stable: false,
        };
        let relayed = plain(PeerPath::Relayed);
        assert_eq!(
            best_path([(&peer, relayed), (&peer, young)].into_iter(), &peer).map(|s| s.path),
            Some(PeerPath::Relayed)
        );
        assert_eq!(
            best_path([(&peer, young)].into_iter(), &peer),
            Some(young),
            "alone, the candidate is the path"
        );
        let mut paths = HashMap::new();
        assert_eq!(
            path_events([(&peer, young)].into_iter(), &mut paths, &peer),
            Some(SwarmEvent::Connected {
                peer: peer.clone(),
                path: PeerPath::Direct
            })
        );
        // THE RELAYED CONNECTION GOES FIRST: with the relay the announced
        // path and the punched direct still young, the relay's close --
        // the far end retired it at its own instant, or the relay
        // dropped it -- moves the path to the young direct AT ONCE, by
        // the punch, rather than leaving a path announced that no
        // connection carries; the interval gates the move only while
        // the relayed connection stands.
        let mut paths = HashMap::new();
        assert_eq!(
            path_events(
                [(&peer, relayed), (&peer, young)].into_iter(),
                &mut paths,
                &peer
            ),
            Some(SwarmEvent::Connected {
                peer: peer.clone(),
                path: PeerPath::Relayed
            })
        );
        assert_eq!(
            path_events([(&peer, young)].into_iter(), &mut paths, &peer),
            Some(SwarmEvent::PeerPathChanged {
                peer: peer.clone(),
                previous: PeerPath::Relayed,
                current: PeerPath::Direct,
                reason: PathChange::HolePunched,
            }),
            "the relay's close hands the path to the young punched direct"
        );
        // A stable punched one beside an unpunched direct: the unpunched
        // provides the path, so the move it would make is not a punch.
        let stable = PathSample {
            stable: true,
            ..young
        };
        let direct = plain(PeerPath::Direct);
        assert_eq!(
            best_path([(&peer, stable), (&peer, direct)].into_iter(), &peer),
            Some(direct)
        );
        // THE SAMPLE'S OWN CLOCK: an unpunched connection is stable at
        // once, a punched one after the interval.
        let mut m = ConnectionManager::new(ConnectionPolicy::new(8, 8), 8);
        let slot = m.admit_inbound().expect("a slot");
        let mut c = OpenConnection {
            peer: peer.clone(),
            slot,
            origin: None,
            admitted_class: ConnectionClass::DataPlaneTrusted,
            path: PeerPath::Direct,
            punched: true,
            since_ms: 100,
            retiring: false,
        };
        assert!(!c.sample(100, 10_000).stable);
        assert!(!c.sample(10_099, 10_000).stable);
        assert!(c.sample(10_100, 10_000).stable);
        c.punched = false;
        assert!(c.sample(100, 10_000).stable);
    }

    /// `DialPeer`'s reuse and the race's win read the ADMITTED CLASS
    /// beside the path: a direct connection to an infrastructure-only
    /// peer is not a data-plane path (PR #103 round 1).
    #[test]
    fn a_connection_reused_for_dial_peer_carries_the_data_plane() {
        let mut m = ConnectionManager::new(ConnectionPolicy::new(8, 8), 8);
        let slot = m.admit_inbound().expect("a slot");
        let mut c = OpenConnection {
            peer: TransportIdentity::parse(FAR).expect("valid"),
            slot,
            origin: Some(DialOrigin::RelayReservation),
            admitted_class: ConnectionClass::ConnectivityInfrastructureOnly,
            path: PeerPath::Direct,
            punched: false,
            since_ms: 0,
            retiring: false,
        };
        assert!(
            !c.is_direct_data_plane(),
            "the reservation's control connection is direct and offers no data plane"
        );
        c.admitted_class = ConnectionClass::DataPlaneTrusted;
        assert!(c.is_direct_data_plane());
        c.path = PeerPath::Relayed;
        assert!(!c.is_direct_data_plane(), "a circuit is not a direct path");
    }

    /// A peer trusted BOTH ways loses only its data-plane trust.
    ///
    /// **This test asserted the opposite until `ClassGated` landed, and
    /// the reversal is deliberate.** Its reasoning was, and remains,
    /// correct about AUTHORIZATION: ADR-0036 keeps the two separate, so
    /// this peer is still infrastructure and its reservation is still
    /// authorized. What it could not know is that the connection also
    /// carries a PROTOCOL SET, chosen once at establishment from the
    /// class the peer held then, which libp2p never rebuilds. Keeping
    /// the connection keeps every data-plane protocol advertised on it.
    ///
    /// ADR-0036 anticipates exactly this and orders it: remove the peer
    /// from application protocol state before retaining any eligible
    /// connectivity-control connection, and "if atomic in-place
    /// reconciliation is not safe in the pinned library, close the
    /// connection and re-establish it under the new class rather than
    /// allowing a transient privilege mix". In-place reconciliation is
    /// not safe here, so the fallback applies.
    ///
    /// **The narrowness matters.** Only a connection ADMITTED under
    /// data-plane trust closes; one admitted while the peer was already
    /// infrastructure-only keeps its reservation, which is the case the
    /// origin check was built for and which
    /// `a_connection_admitted_while_gated_survives_a_downgrade_its_origin_permits`
    /// pins. What cannot happen is a connection established under
    /// data-plane trust being silently re-purposed as a
    /// reachability-only one.
    #[test]
    fn a_server_that_loses_its_infrastructure_trust_is_closed() {
        // Infra -> Unauthorized IS a revocation: `permits(Unauthorized,
        // Infra)` does not hold, so the server is listed, and its
        // AutonatProbe connection -- admitted infrastructure-only, so
        // never data-plane gated -- closes because the origin-less
        // question now refuses its class. A comment once said such a
        // server was never in `revoked`. Review finding on PR #89.
        let mut m = manager(&[], &[RELAY]);
        let peer = ident(RELAY);
        let revoked = m.set_trust(trust(&[], &[]), std::slice::from_ref(&peer));
        assert_eq!(
            revoked.len(),
            1,
            "losing infrastructure trust IS a revocation"
        );
        let probe = ConnectionId::new_unchecked(7);
        let closing = connections_to_close(
            &m,
            &revoked,
            [(
                probe,
                &peer,
                Some(DialOrigin::AutonatProbe),
                ConnectionClass::ConnectivityInfrastructureOnly,
            )]
            .into_iter(),
        );
        assert!(closing.contains(&probe));
        // THE CONTROL: a change that keeps its infrastructure trust
        // lists nothing and closes nothing.
        let mut m = manager(&[], &[RELAY]);
        let revoked = m.set_trust(trust(&[], &[RELAY]), std::slice::from_ref(&peer));
        assert!(revoked.is_empty(), "unchanged trust is not a revocation");
    }

    #[test]
    fn partial_revocation_closes_the_connection_whose_protocols_went_stale() {
        let mut m = manager(&[RELAY], &[RELAY]);
        let peer = ident(RELAY);
        let revoked = m.set_trust(trust(&[], &[RELAY]), std::slice::from_ref(&peer));
        assert_eq!(revoked.len(), 1, "the data-plane loss IS a revocation");

        let reservation = ConnectionId::new_unchecked(1);
        let closing = connections_to_close(
            &m,
            &revoked,
            [(
                reservation,
                &peer,
                Some(DialOrigin::RelayReservation),
                ConnectionClass::DataPlaneTrusted,
            )]
            .into_iter(),
        );
        assert!(
            closing.contains(&reservation),
            "the reservation is still AUTHORIZED, and is closed anyway: it carries the \
             data-plane handlers it was given while the peer was trusted, and they \
             cannot be withdrawn from a live connection. Re-established, it comes back \
             correctly gated."
        );
    }

    /// The other half: the same revocation MUST close the data plane.
    ///
    /// Without this, "keep reachability" is satisfied by keeping
    /// everything, which is the bug in the opposite direction.
    #[test]
    fn partial_revocation_still_closes_the_data_plane() {
        let mut m = manager(&[RELAY], &[RELAY]);
        let peer = ident(RELAY);
        let revoked = m.set_trust(trust(&[], &[RELAY]), std::slice::from_ref(&peer));

        let data = ConnectionId::new_unchecked(2);
        let closing = connections_to_close(
            &m,
            &revoked,
            [(
                data,
                &peer,
                Some(DialOrigin::ConnectionManager),
                ConnectionClass::DataPlaneTrusted,
            )]
            .into_iter(),
        );
        assert!(
            closing.contains(&data),
            "the data-plane connection is exactly what was withdrawn"
        );
    }

    /// Inbound carries no origin, and is re-asked the question it was
    /// admitted with rather than being kept by default.
    #[test]
    fn an_inbound_connection_is_reevaluated_without_an_origin() {
        let mut m = manager(&[RELAY], &[RELAY]);
        let peer = ident(RELAY);
        let revoked = m.set_trust(trust(&[], &[RELAY]), std::slice::from_ref(&peer));

        let inbound = ConnectionId::new_unchecked(3);
        let closing = connections_to_close(
            &m,
            &revoked,
            [(inbound, &peer, None, ConnectionClass::DataPlaneTrusted)].into_iter(),
        );
        assert!(
            closing.contains(&inbound),
            "arriving is not an authorization: the data-plane loss closes it"
        );
    }

    /// A peer that was not revoked at all is untouched, whatever its
    /// origin.
    #[test]
    fn a_peer_that_kept_its_trust_keeps_every_connection() {
        let mut m = manager(&[RELAY], &[RELAY]);
        let peer = ident(RELAY);
        let revoked = m.set_trust(trust(&[RELAY], &[RELAY]), std::slice::from_ref(&peer));
        assert!(revoked.is_empty(), "nothing changed, nothing revoked");

        let closing = connections_to_close(
            &m,
            &revoked,
            [(
                ConnectionId::new_unchecked(4),
                &peer,
                None,
                ConnectionClass::DataPlaneTrusted,
            )]
            .into_iter(),
        );
        assert!(closing.is_empty());
    }

    /// A downgrade closes a connection whose ORIGIN would keep it.
    ///
    /// The origin check exists so a relay reservation or AutoNAT probe
    /// survives a peer losing only its data-plane trust -- that peer is
    /// still infrastructure, and the reachability connection is exactly
    /// what should live on. But `ClassGated` decides a connection's
    /// protocol set once, at establishment, from the class the peer held
    /// THEN, and libp2p never rebuilds a handler. So a connection kept
    /// by the origin check would be kept carrying every data-plane
    /// protocol, which is the isolation invariant defeated by the
    /// mechanism that was meant to preserve reachability.
    ///
    /// ADR-0036: close and re-establish "rather than allowing a
    /// transient privilege mix". Decided HERE rather than inside the
    /// wrapper so `set_trust`'s count includes it -- ADR-0012 makes that
    /// count the observable, and a closure the wrapper performed on its
    /// own would be invisible to it. Review finding on PR #77.
    #[test]
    fn a_downgraded_peer_is_closed_even_where_its_origin_still_permits() {
        let mut m = manager(&[RELAY], &[RELAY]);
        let peer = ident(RELAY);
        // Data-plane trust withdrawn; infrastructure kept.
        let revoked = m.set_trust(trust(&[], &[RELAY]), std::slice::from_ref(&peer));
        assert_eq!(revoked.len(), 1, "the downgrade must be reported");

        // THE CONTROL FIRST: the origin genuinely still permits this
        // connection, so the authorization check alone would keep it.
        assert!(
            m.authorizes_for(
                interweave_transport_runtime::ConnectionClass::ConnectivityInfrastructureOnly,
                DialOrigin::RelayReservation
            ),
            "the premise: a reservation with an infrastructure peer stays authorized"
        );

        let id = ConnectionId::new_unchecked(7);
        let closing = connections_to_close(
            &m,
            &revoked,
            [(
                id,
                &peer,
                Some(DialOrigin::RelayReservation),
                ConnectionClass::DataPlaneTrusted,
            )]
            .into_iter(),
        );
        assert!(
            closing.contains(&id),
            "a connection established while the peer was data-plane trusted carries \
             every data-plane handler, and a handler cannot be rebuilt -- so losing \
             that trust must close it even though its origin still permits it"
        );
    }

    /// A connection ADMITTED while the peer was infrastructure-only
    /// survives that peer's later downgrade to... itself.
    ///
    /// **The case that proves the origin check still decides something.**
    /// `Revoked::was` says `DataPlaneTrusted` here — the peer was
    /// promoted after this connection was admitted, and demoted again —
    /// so a rule written against `was` closes it. But this connection
    /// has carried a DENYING handler since establishment: nothing about
    /// it is stale, and closing it drops reachability the peer is still
    /// trusted for, which is exactly what `OpenConnection::origin` was
    /// added to prevent.
    ///
    /// An earlier version of this file judged from `was` and closed
    /// every revoked connection whatever its origin, leaving that field
    /// deciding nothing at all. Review finding on PR #77.
    #[test]
    fn a_connection_admitted_while_gated_survives_a_downgrade_its_origin_permits() {
        // Admitted while infrastructure-only, so `ClassGated` gave it a
        // denying handler.
        let mut m = manager(&[RELAY], &[RELAY]);
        let peer = ident(RELAY);
        let revoked = m.set_trust(trust(&[], &[RELAY]), std::slice::from_ref(&peer));
        assert_eq!(
            revoked.len(),
            1,
            "the premise: this IS reported as a data-plane loss"
        );
        assert_eq!(
            revoked[0].was,
            ConnectionClass::DataPlaneTrusted,
            "and `was` says data-plane trusted, which is what a rule written against it \
             would close on"
        );

        let reservation = ConnectionId::new_unchecked(9);
        let closing = connections_to_close(
            &m,
            &revoked,
            [(
                reservation,
                &peer,
                Some(DialOrigin::RelayReservation),
                // ADMITTED while infrastructure-only.
                ConnectionClass::ConnectivityInfrastructureOnly,
            )]
            .into_iter(),
        );
        assert!(
            closing.is_empty(),
            "a connection whose handler was denying all along has nothing stale to \
             withdraw, and its origin still authorizes it -- closing it would drop \
             reachability for no reason, which is the defect `OpenConnection::origin` \
             exists to prevent"
        );
    }

    /// A peer that was ALREADY infrastructure-only keeps its
    /// reachability connection.
    ///
    /// A peer whose class did not change produces no revoked row at all.
    ///
    /// **This is a guard test, not a control**, and an earlier version
    /// claimed otherwise: it said it was "the case the origin check was
    /// built for", which it is not -- `connections_to_close` exits at
    /// `revoked_class.get(peer)` before reaching either the origin check
    /// or the gating comparison, so the body could be replaced with an
    /// unconditional `closing.insert(id)` and this would still pass.
    /// What it actually pins is `set_trust`'s `was != now` filter.
    /// Review finding on PR #77; the same
    /// documented-its-branch-instead-of-exercising-it shape a commit
    /// earlier on this branch was written to fix.
    ///
    /// The case the origin check WAS built for is
    /// `a_connection_admitted_while_gated_survives_a_downgrade_its_origin_permits`,
    /// which reaches the branch.
    #[test]
    fn a_peer_whose_class_did_not_change_produces_no_revoked_row() {
        let mut m = manager(&[], &[RELAY]);
        let peer = ident(RELAY);
        // The SAME trust sources the manager already holds.
        let revoked = m.set_trust(trust(&[], &[RELAY]), std::slice::from_ref(&peer));
        assert!(
            revoked.is_empty(),
            "the premise, and the whole of what this test proves: an unchanged class \
             is filtered out before `connections_to_close` is reached"
        );

        let id = ConnectionId::new_unchecked(8);
        let closing = connections_to_close(
            &m,
            &revoked,
            [(
                id,
                &peer,
                Some(DialOrigin::RelayReservation),
                // WHAT THE CALLER ACTUALLY HOLDS. This peer is
                // infrastructure-only, so `ClassGated::admits` returned
                // false and the runtime recorded that class -- passing
                // `DataPlaneTrusted` here described a state this
                // scenario cannot produce.
                ConnectionClass::ConnectivityInfrastructureOnly,
            )]
            .into_iter(),
        );
        assert!(
            closing.is_empty(),
            "and so nothing is closed -- but by the filter above, not by anything this \
             function decided"
        );
    }

    /// A ticket libp2p cannot dial is not retried forever.
    ///
    /// Every `from_ticket` failure is a deterministic property of the
    /// ticket, so `record_failure` -- which reschedules -- meant a
    /// trusted peer with a remembered address repeated the identical
    /// conversion failure on every tick once the scheduler was active.
    ///
    /// The case is reachable, not theoretical: `TransportIdentity`
    /// checks a prefix, an alphabet and a length; libp2p decodes the
    /// multihash. This `Qm` identity satisfies the first and fails the
    /// second.
    #[test]
    fn a_ticket_libp2p_cannot_dial_is_settled_permanently() {
        // Through the ADDRESS branch, which is the one still reachable:
        // an address is an opaque string to every neutral type it
        // crosses, so a configured or discovered value that is not a
        // multiaddr arrives at the conversion intact. This test used to
        // go through the PeerId branch instead, on `Qm` plus 44 base58
        // characters — a string the neutral grammar took and libp2p
        // refused. `TransportIdentity::parse` now decodes the base58btc
        // and checks the multihash, so no such string exists any more;
        // the property being asserted is unchanged.
        let mut m = ConnectionManager::new(ConnectionPolicy::new(8, 8), 8);
        m.set_trust(trust(&[RELAY], &[]), &[]);
        let ticket: DialTicket = m
            .handle()
            .load()
            .admit(
                &DialRequest {
                    peer: Some(ident(RELAY)),
                    address: "127.0.0.1:4001".to_owned(),
                    origin: DialOrigin::ConnectionManager,
                },
                0,
            )
            .expect("a trusted peer with a fresh policy is admitted");

        let undialable =
            AdmittedDial::from_ticket(ticket).expect_err("libp2p cannot build a dial from it");
        let reason = settle_undialable(&mut m, *undialable, 0);
        assert!(reason.contains("not a multiaddr"), "it says why: {reason}");
        assert_eq!(
            m.scheduled_retries(),
            0,
            "nothing to retry: the same ticket converts the same way every time"
        );
    }

    /// A CIRCUIT PAIRING REFUSAL FORGETS THE ADDRESS, which is the
    /// consequence two comments assert and no test drove.
    ///
    /// `from_ticket` has four failure modes and only the malformed
    /// address one was ever settled through here. The pairing mode is
    /// the one whose blast radius is a good route rather than a bad
    /// string: `record_permanent_failure` removes the address from the
    /// peer's book, so a caller that labels a real circuit with the
    /// wrong origin loses it. Both `settle_undialable`'s doc and
    /// `gated_swarm`'s guard comment say so; this is what says it if
    /// it stops being true. Review finding on PR #74.
    #[test]
    fn a_circuit_pairing_refusal_forgets_the_address_it_refused() {
        let mut m = ConnectionManager::new(ConnectionPolicy::new(8, 8), 8);
        m.set_trust(trust(&[RELAY], &[]), &[]);
        let circuit = format!("/ip4/192.0.2.1/tcp/4001/p2p-circuit/p2p/{RELAY}");
        m.learn_address(&ident(RELAY), &circuit, 0);
        // A SECOND ROUTE THE REFUSAL MUST NOT TOUCH. With one address
        // in the book, "gone" and "the whole peer was dropped" are the
        // same number. Review findings on PR #74.
        m.learn_address(&ident(RELAY), "/ip4/198.51.100.9/tcp/4001", 0);
        assert_eq!(
            m.known_addresses(&ident(RELAY)),
            2,
            "both routes are in the book before the refusal"
        );

        // Admitted under the WRONG origin: a circuit address must claim
        // `RelayCircuit`, and `ConnectionManager` is what the scheduler
        // supplies.
        let ticket: DialTicket = m
            .handle()
            .load()
            .admit(
                &DialRequest {
                    peer: Some(ident(RELAY)),
                    address: circuit.clone(),
                    origin: DialOrigin::ConnectionManager,
                },
                0,
            )
            .expect("a trusted peer with a fresh policy is admitted");

        // AND A SCHEDULED RETRY THE REFUSAL MUST NOT CANCEL.
        // `record_permanent_failure`'s own comment records the
        // regression: it used to remove the peer's whole RETRY entry,
        // so a manual dial to one bad address cancelled the reconnect
        // that would have tried the others. That is a different map
        // from the book, and asserting `scheduled_retries() == 0` on a
        // fixture that never schedules one cannot observe a removal
        // from an empty map -- which is what the previous version of
        // this test did while its comment claimed the scheduler was
        // covered. One is scheduled here so the assertion after the
        // refusal has something to lose.
        //
        // It pins EXISTENCE, not shape: `scheduled_retries()` is
        // `retries.len()` and this fixture has one peer, so a mutant
        // that re-scheduled RELAY -- moving `due_at_ms`, bumping
        // `attempts` -- leaves the length alone and survives here. If
        // it re-schedules by INSERTING it dies in
        // `a_ticket_libp2p_cannot_dial_is_settled_permanently`, whose
        // retry map is empty so any insert shows, whichever direction
        // it moves the due time.
        //
        // What follows is about the other kind: a mutant that edits
        // the existing entry IN PLACE, through `get_mut`. That one
        // leaves the length at one here and zero there, so neither
        // `scheduled_retries()` assertion sees it, and it is covered
        // only in part.
        //
        // `settle_undialable` calls `record_permanent_failure`, and an
        // in-place mutant THERE that moves `due_at_ms` LATER dies in
        // `a_claimed_permanent_failure_releases_rather_than_strands_the_claim`
        // and `a_later_candidate_starting_takes_the_claim_back` -- both
        // in `crates/transport/runtime/src/connection_manager.rs`,
        // which is a different crate from this one -- because both read
        // the retry back through `take_due_retries(30_000, 8)`, and a
        // later due time makes the peer not due.
        //
        // A move EARLIER, or a change to `attempts`, dies in no CALLER
        // OF `release_retry_claim`: none of them reads `attempts`
        // afterwards, and none follows one with a second
        // `record_failure` whose backoff would expose a reset counter.
        // (`record_failure` itself releases a scheduler-owned claim by
        // re-scheduling it unclaimed, and
        // `a_transient_failure_reschedules_and_keeps_attempts` DOES
        // read `attempts` back through `is_retry_due` on that path --
        // which is why this names the callers rather than saying
        // "after a release".)
        //
        // `release_retry_claim` cannot host the in-place mutant at all:
        // its body only sets `claimed = false`. Its doc claims
        // `due_at_ms` and `attempts` are "left exactly as they were",
        // and only `due_at_ms` is pinned -- only against moving
        // LATER, since every test that reads it back does so through
        // `take_due_retries`, which is blind to a move earlier.
        // Review findings on PR #74 and #75.
        let other: DialTicket = m
            .handle()
            .load()
            .admit(
                &DialRequest {
                    peer: Some(ident(RELAY)),
                    address: "/ip4/198.51.100.9/tcp/4001".to_owned(),
                    origin: DialOrigin::ConnectionManager,
                },
                0,
            )
            .expect("a trusted peer with a fresh policy is admitted");
        m.record_failure(other, 0);
        assert_eq!(
            m.scheduled_retries(),
            1,
            "the peer has a reconnect scheduled before the refusal"
        );
        let undialable = AdmittedDial::from_ticket(ticket)
            .expect_err("a circuit address under another origin is refused");
        let reason = settle_undialable(&mut m, *undialable, 0);
        // `contains("RelayCircuit")` would NOT discriminate: both of
        // `from_ticket`'s circuit messages carry that word — one says
        // "must be admitted as RelayCircuit", the other "admission
        // claims RelayCircuit but address … carries no /p2p-circuit" —
        // so a branch swap would pass it while printing the message
        // for the other input shape. Review finding on PR #74.
        assert!(
            reason.contains("is a relay circuit"),
            "it names the ADDRESS as the circuit, which is the direction \
             refused here: {reason}"
        );
        // THE SURVIVOR IS NAMED, not counted. An earlier version of
        // this asked `address_dialable` for the other route, which is
        // `is_none_or` over the quarantine map -- and nothing in this
        // test ever writes that map, so it answered `true` for any
        // string, including the address just proved gone. It could not
        // fail. Counting is not enough either: `known.remove(..)` as
        // `known.pop_last()` drops the SURVIVOR and keeps the refused
        // circuit, and both the count and the dialable check stay
        // green. Review finding on PR #74.
        assert_eq!(
            m.dial_candidates(&ident(RELAY), 0),
            vec!["/ip4/198.51.100.9/tcp/4001".to_owned()],
            "the refused circuit is gone and ONLY the peer's other route survives"
        );
        // BOTH QUESTIONS, because they are different ones and the doc
        // above claims both. `dial_candidates` is book MINUS
        // quarantine, so on its own it cannot tell "removed from the
        // book" from "still there and suppressed" -- swapping
        // `record_permanent_failure` for `record_identity_mismatch`,
        // which quarantines and RELEASES rather than removing, passes
        // it while the address goes on spending `max_addresses` and
        // returns when the quarantine expires. An earlier version of
        // this test replaced the count with the candidates rather than
        // adding it, and lost exactly that. Review finding on PR #74.
        assert_eq!(
            m.known_addresses(&ident(RELAY)),
            1,
            "and it is GONE FROM THE BOOK, not merely undialable"
        );
        assert_eq!(
            m.scheduled_retries(),
            1,
            "and the peer's reconnect SURVIVES: the failure is address-scoped, \
             so one bad label does not cancel the retry that would try the rest"
        );
    }

    #[test]
    fn every_identity_the_neutral_grammar_accepts_libp2p_accepts() {
        // What makes `from_ticket`'s PeerId branch unreachable, and the
        // guard that says so if the neutral grammar is ever loosened.
        // The two parsers are independent implementations of the same
        // rule, so agreement is a property to assert rather than assume.
        let mut seed = 0x2545_F491_4F6C_DD1D_u64;
        let mut accepted = 0u32;
        for _ in 0..2_000u32 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let mut bytes = [0_u8; 38];
            for (i, b) in bytes.iter_mut().enumerate() {
                *b = ((seed >> ((i % 8) * 8)) as u8) ^ (i as u8);
            }
            // Both accepted forms, and the identity form's fixed header
            // so the sample is not all rejections.
            let mut identity_form = bytes;
            identity_form[..6].copy_from_slice(&[0x00, 0x24, 0x08, 0x01, 0x12, 0x20]);
            for candidate in [
                bs58::encode(&bytes[..34]).into_string(),
                bs58::encode(&bytes[..]).into_string(),
                bs58::encode(&identity_form[..]).into_string(),
            ] {
                if TransportIdentity::parse(candidate.clone()).is_ok() {
                    accepted += 1;
                    assert!(
                        candidate.parse::<libp2p::PeerId>().is_ok(),
                        "the neutral grammar accepted {candidate}, libp2p did not"
                    );
                }
            }
        }
        assert!(
            accepted > 1_000,
            "only {accepted} candidates were accepted; a sample that rejects \
             everything would pass this test while proving nothing"
        );

        // NEGATIVE CONTROL: the string this test's neighbour used to be
        // built on. Both parsers refuse it, which is the agreement in the
        // other direction.
        let shaped_only = format!("Qm{}", "z".repeat(44));
        assert!(TransportIdentity::parse(shaped_only.clone()).is_err());
        assert!(shaped_only.parse::<libp2p::PeerId>().is_err());
    }

    fn addr() -> Multiaddr {
        "/ip4/127.0.0.1/tcp/1".parse().expect("valid")
    }

    fn unsupported() -> (Multiaddr, TransportError<std::io::Error>) {
        (addr(), TransportError::MultiaddrNotSupported(addr()))
    }

    fn network(kind: std::io::ErrorKind) -> (Multiaddr, TransportError<std::io::Error>) {
        (addr(), TransportError::Other(std::io::Error::from(kind)))
    }

    #[test]
    fn a_single_unsupported_address_is_permanent() {
        assert!(is_permanent_dial_error(&DialError::Transport(vec![
            unsupported()
        ])));
    }

    #[test]
    fn a_single_network_failure_is_not_permanent() {
        assert!(!is_permanent_dial_error(&DialError::Transport(vec![
            network(std::io::ErrorKind::ConnectionRefused)
        ])));
    }

    #[test]
    fn one_network_failure_among_several_unsupported_addresses_is_not_permanent() {
        // THE quantifier this classification rests on. A dial that
        // tried several addresses and reached the network on even one
        // of them is not a structural failure -- `.all()`, not `.any()`,
        // is what a mix has to fall through to `record_failure` rather
        // than being cleared as unfixable.
        assert!(!is_permanent_dial_error(&DialError::Transport(vec![
            unsupported(),
            network(std::io::ErrorKind::TimedOut),
        ])));
    }

    #[test]
    fn every_address_unsupported_is_permanent_even_with_several() {
        assert!(is_permanent_dial_error(&DialError::Transport(vec![
            unsupported(),
            unsupported(),
        ])));
    }

    /// THE REGRESSION CI FOUND ON #111, as a unit test.
    ///
    /// Building the DNS transport re-shaped every dial's error, not
    /// just a name's: an address no transport understands now arrives
    /// as `TransportError::Other` carrying `libp2p-dns`'s own wrapper,
    /// so the variant match answers `false` and an undialable address
    /// is retried instead of dropped from the book (ADR-0010's
    /// address-versus-peer distinction).
    ///
    /// Delete the `address_has_no_transport_in_this_build` arm and this
    /// fails, with the exact error text the CI run produced.
    #[test]
    fn an_undialable_address_is_permanent_however_the_transport_shaped_the_error() {
        let udp: Multiaddr = "/ip4/127.0.0.1/udp/1".parse().expect("valid");
        let wrapped = || {
            TransportError::Other(std::io::Error::other(
                "Multiple dial errors occurred:\n - Unsupported resolved address: \
                 /ip4/127.0.0.1/udp/1",
            ))
        };

        assert!(
            is_permanent_dial_error(&DialError::Transport(vec![(udp, wrapped())])),
            "no configured transport can ever dial a bare /udp address, so retrying it \
             is retrying a question this process has already answered"
        );

        // THE CONTROL, and it is the one that matters: the same error
        // shape on an address this build CAN dial must stay transient.
        // Without it this test would pass for a predicate that called
        // everything structural.
        assert!(
            !is_permanent_dial_error(&DialError::Transport(vec![(addr(), wrapped())])),
            "a /tcp address that failed is a fact about the network, not about this build"
        );
    }

    /// The `p2p-circuit` arm is load-bearing, not decoration.
    ///
    /// A relayed address reaches its relay over the relay's own
    /// transport, which need not be one this predicate can see -- a
    /// QUIC relay hop carries no `tcp` component at all. Without the
    /// circuit arm such an address reads as undialable and is dropped
    /// from the book the first time the relay is unreachable.
    #[test]
    fn a_circuit_address_without_a_tcp_hop_is_not_structural() {
        let over_quic: Multiaddr = "/ip4/192.0.2.1/udp/4001/quic-v1/p2p-circuit"
            .parse()
            .expect("valid");
        assert!(
            !super::address_has_no_transport_in_this_build(&over_quic),
            "a circuit is dialable through the relay transport whatever the relay hop is"
        );
    }

    #[test]
    fn no_addresses_is_permanent() {
        assert!(is_permanent_dial_error(&DialError::NoAddresses));
    }

    #[test]
    fn dialing_the_local_peer_is_permanent() {
        assert!(is_permanent_dial_error(&DialError::LocalPeerId {
            address: addr()
        }));
    }

    #[test]
    fn a_timeout_is_not_permanent() {
        assert!(!is_permanent_dial_error(&DialError::Aborted));
    }

    /// A manager whose ceilings admit — `ConnectionPolicy::default()`
    /// reserves nothing, which is right for the trust tests above and
    /// wrong for tests that need a ticket.
    fn admitting_manager() -> ConnectionManager {
        let mut m = ConnectionManager::new(ConnectionPolicy::new(8, 8), 8);
        m.set_trust(trust(&[RELAY], &[]), &[]);
        m
    }

    /// THE HOOK, not the predicate: does a refused advertised address
    /// actually stay out of the address book?
    ///
    /// ADR-0052 A 2026-09-20 makes Identify's `listen_addrs` an
    /// instance of rule 1, and the book is what the retry scheduler
    /// dials from. `every_address_the_punch_boundary_refuses_the_
    /// advertised_boundary_refuses_too` says the predicate is right;
    /// this says it is WIRED. Delete the `is_advertised_address` call
    /// in `learn_advertised` and this fails on the first row.
    #[test]
    fn a_peers_advertised_name_and_loopback_never_reach_the_address_book() {
        let mut m = admitting_manager();
        let peer = ident(RELAY);
        let mut counters = AdvertisedCounters::default();
        // No private listener, so rule 3 refuses a private address too.
        let own: Vec<String> = Vec::new();
        let mut boundary = AdvertisedBoundary {
            own_listeners: &own,
            counters: &mut counters,
        };

        let advertised: Vec<Multiaddr> = [
            // THE FINDING. Before the DNS transport this was undialable
            // by accident; now it would be resolved and dialled.
            "/dns4/whatever-the-peer-chose.invalid/tcp/4001",
            "/ip4/127.0.0.1/tcp/4001",
            "/ip4/169.254.169.254/tcp/80",
            "/ip6/fe80::1/tcp/4001",
            "/ip4/10.0.0.1/tcp/4001",
        ]
        .iter()
        .map(|a| a.parse().expect("valid"))
        .collect();

        learn_advertised(&mut m, &peer, &advertised, &mut boundary, 0);

        assert_eq!(
            m.known_addresses(&peer),
            0,
            "a peer-supplied name or special-use address must never become a book entry: \
             the scheduler dials the book unprompted"
        );
        assert_eq!(counters.admitted, 0);
        assert_eq!(counters.refused_total(), 5);
        assert_eq!(counters.refused.get("not_literal").copied(), Some(1));
        assert_eq!(counters.refused.get("special_use").copied(), Some(3));
        assert_eq!(
            counters
                .refused
                .get("private_without_private_listener")
                .copied(),
            Some(1)
        );
    }

    /// THE CONTROL, and it is what stops the test above passing for a
    /// hook that refuses everything -- which would look identical from
    /// the book's side.
    #[test]
    fn a_peers_advertised_global_address_still_reaches_the_address_book() {
        let mut m = admitting_manager();
        let peer = ident(RELAY);
        let mut counters = AdvertisedCounters::default();
        let own: Vec<String> = Vec::new();
        let mut boundary = AdvertisedBoundary {
            own_listeners: &own,
            counters: &mut counters,
        };

        let advertised: Vec<Multiaddr> = ["/ip4/8.8.8.8/tcp/4001", "/ip6/2606:4700::1/tcp/4001"]
            .iter()
            .map(|a| a.parse().expect("valid"))
            .collect();

        learn_advertised(&mut m, &peer, &advertised, &mut boundary, 0);

        assert_eq!(m.known_addresses(&peer), 2);
        assert_eq!(counters.admitted, 2);
        assert_eq!(counters.refused_total(), 0);
    }

    /// Rule 3 through the hook: the LAN peer case the boundary must not
    /// break, judged against the listeners this node actually holds.
    #[test]
    fn a_lan_peers_private_address_reaches_the_book_when_this_node_is_on_a_lan() {
        let mut m = admitting_manager();
        let peer = ident(RELAY);
        let mut counters = AdvertisedCounters::default();
        let own = vec!["/ip4/192.168.7.20/tcp/4001".to_owned()];
        let mut boundary = AdvertisedBoundary {
            own_listeners: &own,
            counters: &mut counters,
        };

        let advertised: Vec<Multiaddr> = ["/ip4/192.168.7.31/tcp/4001"]
            .iter()
            .map(|a| a.parse().expect("valid"))
            .collect();

        learn_advertised(&mut m, &peer, &advertised, &mut boundary, 0);

        assert_eq!(
            m.known_addresses(&peer),
            1,
            "a LAN peer advertising its RFC 1918 address is exactly what rule 3 admits \
             when this node holds a private listener of the same family"
        );
        assert_eq!(counters.admitted, 1);
    }

    /// A placeholder ticket the way the outbound gate mints one.
    fn placeholder_ticket(m: &ConnectionManager) -> DialTicket {
        m.handle()
            .admit(
                &DialRequest {
                    peer: Some(ident(RELAY)),
                    address: String::new(),
                    origin: DialOrigin::KademliaQuery,
                },
                0,
            )
            .expect("a trusted peer is admitted on the placeholder")
    }

    #[test]
    fn a_multi_address_failure_settles_every_address() {
        // F15's whole point: `DialError::Transport` carries one entry
        // per exhausted address, and recording only the first leaves
        // the rest unscored and immediately retryable.
        let mut m = admitting_manager();
        let peer = ident(RELAY);
        let ticket = placeholder_ticket(&m);
        let a1: Multiaddr = "/ip4/192.0.2.1/tcp/1".parse().expect("valid");
        let a2: Multiaddr = "/ip4/192.0.2.2/tcp/2".parse().expect("valid");
        let error = DialError::Transport(vec![
            (
                a1,
                TransportError::Other(std::io::Error::from(std::io::ErrorKind::ConnectionRefused)),
            ),
            (
                a2,
                TransportError::Other(std::io::Error::from(std::io::ErrorKind::TimedOut)),
            ),
        ]);
        settle_failed_dial(&mut m, ticket, &error, 0);
        assert_eq!(
            m.known_addresses(&peer),
            2,
            "BOTH exhausted addresses were scored and learned, not only the \
             one the ticket settled"
        );
        assert_eq!(
            m.handle().load().pending_dials(),
            0,
            "and the one reservation is settled"
        );
    }

    #[test]
    fn a_wrong_peer_answer_quarantines_the_address_it_used() {
        let mut m = admitting_manager();
        let peer = ident(RELAY);
        let ticket = placeholder_ticket(&m);
        let used: Multiaddr = format!("/ip4/192.0.2.1/tcp/1/p2p/{RELAY}")
            .parse()
            .expect("valid");
        let error = DialError::WrongPeerId {
            obtained: libp2p::PeerId::random(),
            address: used,
        };
        settle_failed_dial(&mut m, ticket, &error, 0);
        assert!(
            !m.handle()
                .load()
                .address_dialable(&peer, "/ip4/192.0.2.1/tcp/1", 0),
            "the quarantine binds to the REAL address, stripped of its suffix \
             — settled on the placeholder it would bind to nothing"
        );
        assert!(
            m.handle()
                .load()
                .address_dialable(&peer, "/ip4/192.0.2.9/tcp/1", 0),
            "and to nothing else"
        );
    }

    #[test]
    fn our_own_gate_refusal_does_not_deepen_the_quarantine_that_caused_it() {
        // The outbound gate's established hook rejects an address the
        // quarantine suppresses, and libp2p reports that back as
        // `DialError::Denied`. Settled as an ordinary failure, the
        // address score extended the very suppression that produced the
        // refusal — so a route this node keeps re-testing could never
        // lapse out of quarantine — and the peer backoff riding with it
        // advanced a trusted peer over one address this node declined
        // to use.
        let mut m = admitting_manager();
        let peer = ident(RELAY);
        let refused = "/ip4/192.0.2.4/tcp/1";

        let ticket = m
            .handle()
            .admit(
                &DialRequest {
                    peer: Some(peer.clone()),
                    address: refused.to_owned(),
                    origin: DialOrigin::KademliaQuery,
                },
                0,
            )
            .expect("admitted");
        settle_failed_dial(
            &mut m,
            ticket,
            &DialError::Denied {
                cause: libp2p::swarm::ConnectionDenied::new(std::io::Error::other("quarantined")),
            },
            0,
        );

        assert_eq!(
            m.scheduled_retries(),
            0,
            "this node's own refusal is not a reason to retry"
        );
        assert_eq!(
            m.handle().load().pending_dials(),
            0,
            "but the slot is settled"
        );
        let again = m
            .handle()
            .admit(
                &DialRequest {
                    peer: Some(peer.clone()),
                    address: "/ip4/192.0.2.5/tcp/1".to_owned(),
                    origin: DialOrigin::KademliaQuery,
                },
                1,
            )
            .expect("a known-good route is not suppressed by our refusal of another");
        drop(again);
    }

    #[test]
    fn a_placeholder_with_no_address_information_settles_clean() {
        let mut m = admitting_manager();
        let peer = ident(RELAY);
        let ticket = placeholder_ticket(&m);
        settle_failed_dial(&mut m, ticket, &DialError::Aborted, 0);
        assert_eq!(m.known_addresses(&peer), 0, "no address exists to learn");
        assert_eq!(m.scheduled_retries(), 0, "or to retry");
        assert_eq!(m.handle().load().pending_dials(), 0, "the slot is settled");
    }

    /// A punch dial that exhausted three candidates -- one refused, one
    /// timed out, one structurally undialable -- scores none of them,
    /// learns none, schedules no retry and leaves the peer dialable;
    /// the same batch under any other origin scores every one.
    #[test]
    fn a_hole_punch_dials_failure_scores_none_of_its_addresses() {
        let peer = ident(RELAY);
        let batch = |origin: DialOrigin, m: &ConnectionManager| {
            let ticket = m
                .handle()
                .admit(
                    &DialRequest {
                        peer: Some(peer.clone()),
                        address: String::new(),
                        origin,
                    },
                    0,
                )
                .expect("admitted");
            let error = DialError::Transport(vec![
                (
                    "/ip4/192.0.2.1/tcp/1".parse().expect("addr"),
                    TransportError::Other(std::io::Error::other("refused")),
                ),
                (
                    "/ip4/192.0.2.2/tcp/1".parse().expect("addr"),
                    TransportError::Other(std::io::Error::other("timeout")),
                ),
                (
                    "/ip4/192.0.2.3/udp/1".parse().expect("addr"),
                    TransportError::MultiaddrNotSupported(
                        "/ip4/192.0.2.3/udp/1".parse().expect("addr"),
                    ),
                ),
            ]);
            (ticket, error)
        };
        let mut m = admitting_manager();
        let (ticket, error) = batch(DialOrigin::DcutrHolePunch, &m);
        settle_failed_dial(&mut m, ticket, &error, 5);
        assert_eq!(m.scheduled_retries(), 0);
        assert_eq!(m.known_addresses(&peer), 0);
        assert!(
            m.handle()
                .admit(
                    &DialRequest {
                        peer: Some(peer.clone()),
                        address: "/ip4/192.0.2.9/tcp/1".to_owned(),
                        origin: DialOrigin::Manual,
                    },
                    6
                )
                .is_ok(),
            "the peer is not in backoff"
        );
        // THE CONTROL.
        let mut m = admitting_manager();
        let (ticket, error) = batch(DialOrigin::KademliaQuery, &m);
        settle_failed_dial(&mut m, ticket, &error, 5);
        assert_eq!(m.scheduled_retries(), 1);
        assert!(m.known_addresses(&peer) >= 1);
    }

    #[test]
    fn a_revoked_kademlia_dial_is_refused_at_establishment() {
        // The plan's genericity proof: revoked-mid-dial reclassification
        // covers the KademliaQuery origin with ZERO new code, because
        // `authorizes_for` takes the ticket's own origin. This test adds
        // the origin the settlement path had never seen and watches the
        // same line refuse it.
        let mut m = admitting_manager();
        let peer = ident(RELAY);
        let mut ticket = placeholder_ticket(&m);
        assert!(ticket.rebind_address("/ip4/192.0.2.1/tcp/1"));
        // Trust revoked between admission and the completed handshake.
        let _ = m.set_trust(trust(&[], &[]), std::slice::from_ref(&peer));
        assert!(
            settle_established_outbound(&mut m, &peer, ticket, PeerPath::Direct, 5).is_none(),
            "authority that no longer exists retains nothing"
        );
        assert_eq!(
            m.scheduled_retries(),
            0,
            "withdrawn is not a network failure; nothing is rescheduled"
        );

        // The control: with trust intact the same shape is kept, and
        // the rebound address enters the book.
        let mut m = admitting_manager();
        let mut ticket = placeholder_ticket(&m);
        assert!(ticket.rebind_address("/ip4/192.0.2.1/tcp/1"));
        let (slot, origin, _class) =
            settle_established_outbound(&mut m, &peer, ticket, PeerPath::Direct, 5)
                .expect("trusted and kept");
        assert_eq!(origin, DialOrigin::KademliaQuery);
        assert_eq!(
            m.known_addresses(&peer),
            1,
            "the address that worked is in the book (F12's whole point)"
        );
        drop(slot);
    }

    /// A reservation ask admitted toward an infrastructure-only relay
    /// that came up over a CIRCUIT -- the address cache extended the
    /// ask through another relay -- is refused at establishment: the
    /// far end of a circuit is an application destination, and the
    /// relay is not one. The same ask over a direct connection is
    /// retained under its own origin (the control), and the origin the
    /// relayed one is kept under, when the far end IS a data-plane
    /// peer, is `RelayCircuit`.
    #[test]
    fn a_relayed_outbound_is_judged_under_relay_circuit_whatever_dialled_it() {
        let peer = ident(RELAY);
        let ask = |m: &ConnectionManager| {
            m.handle()
                .admit(
                    &DialRequest {
                        peer: Some(peer.clone()),
                        address: "/ip4/192.0.2.1/tcp/1".to_owned(),
                        origin: DialOrigin::RelayReservation,
                    },
                    0,
                )
                .expect("a reservation ask toward an infrastructure-only relay is admitted")
        };
        // Infrastructure-only relay: admitted under RelayReservation.
        let mut m = ConnectionManager::new(ConnectionPolicy::new(8, 8), 8);
        m.set_trust(trust(&[], &[RELAY]), &[]);
        let ticket = ask(&m);
        assert!(
            settle_established_outbound(&mut m, &peer, ticket, PeerPath::Relayed, 5).is_none(),
            "over a circuit the ask reaches an application destination it is not authorized for"
        );
        assert_eq!(m.scheduled_retries(), 0, "withdrawn, not failed");
        // THE CONTROL: the same ask over a direct connection is kept
        // under the origin that dialled it.
        let ticket = ask(&m);
        let (slot, origin, _) =
            settle_established_outbound(&mut m, &peer, ticket, PeerPath::Direct, 5)
                .expect("direct, the reservation is retained");
        assert_eq!(origin, DialOrigin::RelayReservation);
        drop(slot);
        // A data-plane far end over a circuit is retained, and under
        // `RelayCircuit` -- the origin a trust change re-asks.
        let mut m = ConnectionManager::new(ConnectionPolicy::new(8, 8), 8);
        m.set_trust(trust(&[RELAY], &[RELAY]), &[]);
        let ticket = ask(&m);
        let (slot, origin, _) =
            settle_established_outbound(&mut m, &peer, ticket, PeerPath::Relayed, 5)
                .expect("a data-plane far end over a circuit is retained");
        assert_eq!(origin, DialOrigin::RelayCircuit);
        drop(slot);
    }

    /// The record of an inbound is origin-less whatever question it was
    /// retained under -- `RelayCircuit` for a relayed one, the closure's
    /// `AutonatProbe` for a direct one with a server on, the
    /// origin-less question for the rest -- because `origin` means
    /// "this profile dialled it" (PR #101 round 1). The refusal half
    /// beside it: a relayed inbound from an infrastructure-only peer is
    /// refused where a direct one under `AutonatProbe` is retained.
    #[test]
    fn an_inbound_is_recorded_origin_less_whatever_it_was_retained_under() {
        let peer = ident(RELAY);
        let mut m = ConnectionManager::new(ConnectionPolicy::new(8, 8), 8);
        m.set_trust(trust(&[RELAY], &[RELAY]), &[]);
        let class = m.classify(&peer);
        for (path, asked_under) in [
            (PeerPath::Relayed, Some(DialOrigin::RelayCircuit)),
            (PeerPath::Direct, Some(DialOrigin::AutonatProbe)),
            (PeerPath::Direct, None),
        ] {
            let connection =
                settle_established_inbound(&mut m, peer.clone(), class, path, asked_under, 0)
                    .expect("a data-plane peer is retained under every question");
            assert_eq!(
                connection.origin, None,
                "an inbound never records an origin: {path:?} asked under {asked_under:?}"
            );
            assert_eq!(connection.path, path);
        }
        // THE CONTROL, and the rule the path carries: an
        // infrastructure-only peer is retained on a direct inbound
        // under the closure's `AutonatProbe` and refused over a
        // circuit, where the question is `RelayCircuit`.
        let mut m = ConnectionManager::new(ConnectionPolicy::new(8, 8), 8);
        m.set_trust(trust(&[], &[RELAY]), &[]);
        let class = m.classify(&peer);
        assert!(
            settle_established_inbound(
                &mut m,
                peer.clone(),
                class,
                PeerPath::Direct,
                Some(DialOrigin::AutonatProbe),
                0
            )
            .is_some()
        );
        assert!(
            settle_established_inbound(
                &mut m,
                peer,
                class,
                PeerPath::Relayed,
                Some(DialOrigin::RelayCircuit),
                0
            )
            .is_none(),
            "an infrastructure-only source over a circuit is refused"
        );
        // And under the origin-less question -- no server on, a direct
        // inbound -- the same peer is refused as it always was.
        assert!(
            settle_established_inbound(&mut m, ident(RELAY), class, PeerPath::Direct, None, 0)
                .is_none(),
            "an infrastructure-only peer is refused by the origin-less question"
        );
    }

    /// The book's classification keeps the caller's own origin for a
    /// direct address and for a string that does not parse, and
    /// names `RelayCircuit` for a circuit whatever the caller's own.
    #[test]
    fn the_books_classification_keeps_the_callers_origin_off_a_circuit() {
        let direct = "/ip4/127.0.0.1/tcp/1";
        let circuit = format!("/ip4/127.0.0.1/tcp/1/p2p/{RELAY}/p2p-circuit");
        for otherwise in [DialOrigin::ConnectionManager, DialOrigin::Manual] {
            assert_eq!(book_origin(direct, otherwise), otherwise);
            assert_eq!(book_origin("not a multiaddr", otherwise), otherwise);
            assert_eq!(book_origin(&circuit, otherwise), DialOrigin::RelayCircuit);
        }
    }

    #[test]
    fn an_all_unsupported_batch_forgets_every_route() {
        let mut m = admitting_manager();
        let peer = ident(RELAY);
        let ticket = placeholder_ticket(&m);
        // DISTINCT addresses, deliberately: with both attempts on one
        // address, the ticket's permanent settlement erased the same
        // route a wrongly-transient second scoring had just learned,
        // and the mutation this test exists to kill passed.
        let error = DialError::Transport(vec![
            unsupported(),
            (
                "/ip4/192.0.2.7/tcp/7".parse().expect("valid"),
                TransportError::MultiaddrNotSupported(
                    "/ip4/192.0.2.7/tcp/7".parse().expect("valid"),
                ),
            ),
        ]);
        settle_failed_dial(&mut m, ticket, &error, 0);
        assert_eq!(
            m.known_addresses(&peer),
            0,
            "structural routes are forgotten, not learned as retryable"
        );
        assert_eq!(m.scheduled_retries(), 0);
    }

    #[test]
    fn a_mixed_batch_settles_each_attempt_by_its_own_class() {
        let mut m = admitting_manager();
        let peer = ident(RELAY);
        let ticket = placeholder_ticket(&m);
        // First attempt structural, second a network refusal: the OLD
        // aggregate said "not permanent" and learned both as retryable.
        let error = DialError::Transport(vec![
            unsupported(),
            (
                "/ip4/192.0.2.2/tcp/2".parse().expect("valid"),
                TransportError::Other(std::io::Error::from(std::io::ErrorKind::ConnectionRefused)),
            ),
        ]);
        settle_failed_dial(&mut m, ticket, &error, 0);
        assert_eq!(
            m.known_addresses(&peer),
            1,
            "only the transiently failed route is worth remembering"
        );
        assert!(
            m.dial_candidates(&peer, 1)
                .contains(&"/ip4/192.0.2.2/tcp/2".to_owned()),
            "and it is the network-refused one, not the structural one"
        );
    }
    /// A second trusted peer, so a foreign trailing claim has a name.
    const OTHER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTA";

    #[test]
    fn the_peers_own_trailing_suffix_is_stripped_from_the_dial_key() {
        // F10's failure mode on the command path. One socket reached as
        // the bare route and as the suffixed route earned a quarantine on
        // one spelling and went on being dialled under the other, and
        // both spent a slot in `max_addresses`.
        assert_eq!(
            canonical_dial_address(&ident(RELAY), &format!("/ip4/192.0.2.1/tcp/1/p2p/{RELAY}")),
            "/ip4/192.0.2.1/tcp/1",
            "the suffix names the peer the dial already names"
        );
        assert_eq!(
            canonical_dial_address(&ident(RELAY), "/ip4/192.0.2.1/tcp/1"),
            "/ip4/192.0.2.1/tcp/1",
            "and the bare form is already canonical"
        );
    }

    #[test]
    fn a_circuits_relay_is_part_of_the_route_and_is_never_stripped() {
        // THE ONE THAT MUST NOT REGRESS. `/p2p/<relay>` before a
        // `/p2p-circuit` says which relay carries the circuit. Collapsing
        // it would key every relay to one entry, so one bad relay would
        // quarantine the destination through all of them.
        let via_relay = format!("/ip4/192.0.2.1/tcp/4001/p2p/{RELAY}/p2p-circuit/p2p/{OTHER}");
        assert_eq!(
            canonical_dial_address(&ident(OTHER), &via_relay),
            format!("/ip4/192.0.2.1/tcp/4001/p2p/{RELAY}/p2p-circuit"),
            "the destination's own trailing claim goes; the relay stays"
        );

        // THE SAME DESTINATION THROUGH A DIFFERENT RELAY IS A DIFFERENT
        // KEY, and everything except the relay is held fixed so that the
        // inequality can only come from the relay surviving. An earlier
        // version of this varied the IP as well (`192.0.2.9` against
        // `192.0.2.1`) and so held under every mutation of the stripping,
        // including the one the paragraph above names -- a reviewer
        // measured it. Review finding on PR #86.
        let same_dest_other_relay =
            format!("/ip4/192.0.2.1/tcp/4001/p2p/{OTHER}/p2p-circuit/p2p/{RELAY}");
        let same_dest_this_relay =
            format!("/ip4/192.0.2.1/tcp/4001/p2p/{RELAY}/p2p-circuit/p2p/{RELAY}");
        assert_ne!(
            canonical_dial_address(&ident(RELAY), &same_dest_other_relay),
            canonical_dial_address(&ident(RELAY), &same_dest_this_relay),
            "one destination through two relays is two routes"
        );
    }

    #[test]
    fn a_trailing_claim_naming_someone_else_stays_in_the_key() {
        // Stripping it would launder the contradiction into the bare
        // route: the policy would then score and quarantine a string the
        // caller never asked for.
        let foreign = format!("/ip4/192.0.2.1/tcp/1/p2p/{OTHER}");
        assert_eq!(
            canonical_dial_address(&ident(RELAY), &foreign),
            foreign,
            "a foreign claim is the address contradicting the dial, not a suffix"
        );
    }

    #[test]
    fn an_unparseable_address_reaches_the_undialable_path_unchanged() {
        // Canonicalizing is not where a malformed value's classification
        // changes. `from_ticket` must still see the original so
        // `settle_undialable` reports what the caller actually supplied.
        //
        // ONLY THE FIRST ASSERTION IS LOAD-BEARING, and the second is kept
        // with that said rather than dressed up. `""` is NOT unparseable:
        // `multiaddr-0.18.2`'s `FromStr` splits on `/`, the leading
        // `Some("") != parts.next()` check passes for the empty string and
        // the component loop never runs, so it is `Ok(Multiaddr::empty())`.
        // The placeholder therefore reaches `canonical_for_peer` and is
        // preserved by the `stripped.is_empty()` arm, not by the parse
        // guard -- and since both arms answer `""`, no mutation of this
        // function can make the second assertion fail. It pins the
        // contract, not a branch; the branch itself is pinned by
        // `an_address_that_is_only_the_peers_own_suffix_is_left_alone`,
        // where the input is `/p2p/<peer>` and the answer is not `""`.
        // A reviewer measured the parse; review finding on PR #86.
        assert_eq!(
            canonical_dial_address(&ident(RELAY), "not-a-multiaddr"),
            "not-a-multiaddr"
        );
        assert_eq!(
            canonical_dial_address(&ident(RELAY), ""),
            "",
            "and the placeholder the outbound gate mints is untouched"
        );
    }

    #[test]
    fn an_address_that_is_only_the_peers_own_suffix_is_left_alone() {
        // Stripping yields the empty multiaddr, which does not parse --
        // so the address would arrive at `from_ticket` as a DIFFERENT
        // failure than the one it has. It is undialable either way; this
        // keeps which undialable it is.
        let bare = format!("/p2p/{RELAY}");
        assert_eq!(canonical_dial_address(&ident(RELAY), &bare), bare);
    }

    #[test]
    fn one_route_two_spellings_is_one_quarantine_entry() {
        // The end of the chain, through the real gate. A dial earns a
        // quarantine on the route, and the SAME route spelled the other
        // way is then refused -- which is what `attempt_dial` did not do
        // before, because `admit` keys on `(peer, request.address)`
        // verbatim and the command path handed it the caller's string.
        let mut m = admitting_manager();
        let peer = ident(RELAY);
        let bare = "/ip4/192.0.2.1/tcp/1";
        let suffixed = format!("{bare}/p2p/{RELAY}");

        // Earned the way the settlement path earns it: an address that
        // authenticated as somebody else goes into quarantine.
        let ticket = placeholder_ticket(&m);
        let used: Multiaddr = suffixed.parse().expect("valid");
        settle_failed_dial(
            &mut m,
            ticket,
            &DialError::WrongPeerId {
                obtained: libp2p::PeerId::random(),
                address: used,
            },
            0,
        );
        assert!(
            !m.handle().load().address_dialable(&peer, bare, 0),
            "precondition: the settlement bound the quarantine to the bare route"
        );

        // THE FIX. Before it, this admitted: the suffixed string was a
        // key the quarantine had never seen.
        let request = DialRequest {
            peer: Some(peer.clone()),
            address: canonical_dial_address(&peer, &suffixed),
            origin: DialOrigin::Manual,
        };
        assert!(
            m.handle().admit(&request, 0).is_err(),
            "the suffixed spelling is suppressed by the quarantine the bare one earned"
        );

        // The control: a genuinely different route is still dialable, so
        // the test is not passing because everything is refused.
        let elsewhere = DialRequest {
            peer: Some(peer.clone()),
            address: canonical_dial_address(&peer, &format!("/ip4/192.0.2.9/tcp/1/p2p/{RELAY}")),
            origin: DialOrigin::Manual,
        };
        assert!(
            m.handle().admit(&elsewhere, 0).is_ok(),
            "and a different route is unaffected"
        );
    }
    #[test]
    fn the_book_and_the_quarantine_key_one_route_the_same_way() {
        // THE P2 THE FIRST VERSION OF THIS FIX CAUSED. Canonicalizing at
        // admission alone left `learn_address` storing the caller's
        // spelling, so a quarantined route stopped filtering out of the
        // candidate list and the scheduler re-offered it every tick while
        // the gate refused it every tick. The book has to agree.
        let mut m = admitting_manager();
        let peer = ident(RELAY);
        let bare = "/ip4/192.0.2.1/tcp/1";
        let suffixed = format!("{bare}/p2p/{RELAY}");

        // Learned the way the `AddAddress` command and Identify learn it:
        // a bootstrap multiaddr carries its peer suffix by convention.
        assert!(
            learn_route(&mut m, &peer, &suffixed, 0),
            "the route is learned"
        );
        assert_eq!(
            m.known_addresses(&peer),
            1,
            "and the two spellings are ONE entry, not two"
        );

        // Quarantine it through the settlement path.
        let ticket = placeholder_ticket(&m);
        settle_failed_dial(
            &mut m,
            ticket,
            &DialError::WrongPeerId {
                obtained: libp2p::PeerId::random(),
                address: suffixed.parse().expect("valid"),
            },
            0,
        );

        // THE ASSERTION. The candidate list is built from book strings and
        // filtered against the quarantine map; if the two disagree the
        // quarantined route comes back as a candidate.
        assert!(
            m.dial_candidates(&peer, 0).is_empty(),
            "a quarantined route must not be offered as a candidate: got {:?}",
            m.dial_candidates(&peer, 0)
        );
    }

    #[test]
    fn learning_both_spellings_of_one_route_spends_one_slot() {
        // Consequence 3 of the same defect: `record_failure` and
        // `record_success` learn the canonical string, so a book holding
        // the suffixed one ended up with both and `max_addresses_per_peer`
        // bounded half as many real routes as it says.
        let mut m = admitting_manager();
        let peer = ident(RELAY);
        let bare = "/ip4/192.0.2.1/tcp/1";
        let suffixed = format!("{bare}/p2p/{RELAY}");

        assert!(learn_route(&mut m, &peer, bare, 0));
        assert!(learn_route(&mut m, &peer, &suffixed, 0));
        assert_eq!(
            m.known_addresses(&peer),
            1,
            "one physical route is one entry however it was spelled"
        );
    }

    #[test]
    fn an_equivalent_address_written_two_ways_is_one_key() {
        // The re-serialization, tested rather than left as an accident of
        // `strip_own_suffix`'s `collect::<Multiaddr>()`. The doc comment
        // used to claim the function changed "nothing else"; it does, and
        // collapsing these is what a key is for.
        let peer = ident(RELAY);
        assert_eq!(
            canonical_dial_address(&peer, "/ip6/2001:db8:0:0:0:0:0:1/tcp/1"),
            canonical_dial_address(&peer, "/ip6/2001:db8::1/tcp/1"),
            "one IPv6 address written long and short is one route"
        );
        assert_eq!(
            canonical_dial_address(&peer, "/ip6/2001:db8:0:0:0:0:0:1/tcp/1"),
            "/ip6/2001:db8::1/tcp/1",
            "and the canonical spelling is the compressed one"
        );
    }
    #[test]
    fn a_bare_peer_address_is_forgotten_rather_than_held_forever() {
        // THE ONE INPUT THE THREE KEY IMPLEMENTATIONS DISAGREED ON.
        // `strip_own_suffix` yields the empty string for an address that is
        // nothing but the dialled peer's own suffix; `canonical_for_peer`
        // yields the original. `record_permanent_failure` then removes
        // `ticket.address()` from the book -- so with the empty string it
        // removed nothing and the entry was held forever, which is the
        // QUIC-on-TCP defect this branch's own prose describes.
        //
        // AN EARLIER VERSION OF THIS TEST PASSED IN BOTH WORLDS, because it
        // settled against an empty book: `known.remove` found nothing to
        // miss. A reviewer traced that. The route has to be IN the book
        // first, which is what makes the removal observable.
        //
        // THE MUTATION IT DIES ON, stated because the obvious one is not it:
        // making `canonical_for_peer` return the empty strip keeps the book
        // and the ticket CONSISTENTLY empty, so the removal still succeeds
        // and this test passes. What it catches is the two keying
        // DIFFERENTLY -- reverting `settle_failed_dial` to the raw
        // `strip_own_suffix` while the book keeps the canonical form, which
        // is the divergence `canonical_for_peer` exists to prevent.
        let mut m = admitting_manager();
        let peer = ident(RELAY);
        let bare_text = format!("/p2p/{RELAY}");
        let bare: Multiaddr = bare_text.parse().expect("valid");

        // `canonical_dial_address` returns this shape unchanged, pinned by
        // `an_address_that_is_only_the_peers_own_suffix_is_left_alone`.
        assert!(
            learn_route(&mut m, &peer, &bare_text, 0),
            "the route is in the book"
        );
        assert_eq!(m.known_addresses(&peer), 1, "precondition: one entry");

        let ticket = placeholder_ticket(&m);
        settle_failed_dial(
            &mut m,
            ticket,
            &DialError::Transport(vec![(
                bare.clone(),
                TransportError::MultiaddrNotSupported(bare.clone()),
            )]),
            0,
        );

        // THE ASSERTION. A structurally undialable route is forgotten, so
        // it stops spending one of the eight per-peer slots. Under the old
        // empty-string key the removal missed and this stayed at 1.
        assert_eq!(
            m.known_addresses(&peer),
            0,
            "a structurally undialable address is forgotten, not held"
        );
        assert!(
            m.dial_candidates(&peer, 0).is_empty(),
            "and nothing offers it as a candidate"
        );
    }

    #[test]
    fn the_stripped_suffix_is_restored_before_the_transport_sees_it() {
        // TWO REVIEWERS DISAGREED ABOUT THIS, so it is measured here
        // instead of argued again. One held that stripping the destination
        // from a circuit address makes it undialable, because
        // `libp2p-relay`'s client transport refuses an address with no
        // destination component. The other held that the Swarm puts it
        // back. The second is right, and the mechanism is
        // `Multiaddr::with_p2p`: it appends `/p2p/<peer>` unless the
        // address ALREADY ends in a `/p2p/` component.
        //
        // That is the whole reason one string can serve as both the policy
        // key and the dialled address. This test is that property, so a
        // future change to either side cannot quietly break it.
        let peer = ident(RELAY);
        let as_peer: libp2p::PeerId = RELAY.parse().expect("a peer id");

        for original in [
            format!("/ip4/192.0.2.1/tcp/1/p2p/{RELAY}"),
            format!("/ip4/192.0.2.1/tcp/4001/p2p/{OTHER}/p2p-circuit/p2p/{RELAY}"),
        ] {
            let key = canonical_dial_address(&peer, &original);
            let dialled = key
                .parse::<Multiaddr>()
                .expect("the key is a multiaddr")
                .with_p2p(as_peer)
                .expect("a key derived from an address naming this peer is accepted");
            assert_eq!(
                dialled.to_string(),
                original,
                "what the transport receives must be what the caller asked for"
            );
        }

        // AND THE CIRCUIT CASE SPECIFICALLY: the key ends in the circuit
        // marker, which is what makes `with_p2p` take its appending branch
        // rather than its already-suffixed one. Without the marker last,
        // the destination could not be restored and `libp2p-relay` would
        // answer `MissingDstPeerId`.
        let circuit = format!("/ip4/192.0.2.1/tcp/4001/p2p/{OTHER}/p2p-circuit/p2p/{RELAY}");
        let key = canonical_dial_address(&peer, &circuit);
        assert!(
            key.ends_with("/p2p-circuit"),
            "the key stops at the marker: {key}"
        );

        // AND THE FOREIGN-CLAIM SHAPE, which an earlier version of this
        // test asserted away with "the key never ends in a foreign
        // `/p2p/`". It does: preserving such a claim is deliberate and
        // `a_trailing_claim_naming_someone_else_stays_in_the_key` pins it,
        // so the blanket `expect` above was false for the function under
        // test. What actually happens is worth stating rather than hiding:
        // `with_p2p` refuses, the Swarm turns that into
        // `MultiaddrNotSupported`, and the dial fails before any transport
        // sees it. Fail-closed, and unchanged by this commit.
        let foreign = format!("/ip4/192.0.2.1/tcp/1/p2p/{OTHER}");
        let foreign_key = canonical_dial_address(&peer, &foreign);
        assert_eq!(foreign_key, foreign, "the foreign claim is preserved");
        assert!(
            foreign_key
                .parse::<Multiaddr>()
                .expect("a multiaddr")
                .with_p2p(as_peer)
                .is_err(),
            "and the Swarm refuses it rather than dialling someone else"
        );
    }

    /// The file modules `source` declares, as filenames.
    ///
    /// EXTRACTED SO IT HAS A TEST. Three versions of this parser have now
    /// been wrong, each in a way that PASSED: the first matched three
    /// literal prefixes, the second truncated a `pub mod` line at the first
    /// `)` anywhere on it. Both were found by hand-mutating `mod.rs`,
    /// because the only thing exercising the parser was `mod.rs` itself --
    /// which contains none of the shapes that break it. Review finding on
    /// PR #86.
    fn declared_modules(source: &str) -> Vec<String> {
        source
            .lines()
            .filter_map(|line| {
                // TRIMMED, AND THE VISIBILITY STRIPPED GENERICALLY. A first
                // version matched three literal prefixes and required the
                // `;` to be last, which missed `pub(super) mod x;`, an
                // indented declaration, and `mod x; // comment` -- and a
                // missed declaration is a file that never gets scanned,
                // contributing zero matches to an expectation of zero. That
                // is the silent pass this guard exists to refuse, and
                // `pub(super) mod connectivity;` is a plausible next line in
                // this module. Review finding on PR #86.
                // THE COMMENT GOES FIRST, before anything looks for a
                // paren. `split_once(')')` below searches the whole rest of
                // the line, not the visibility's own parentheses -- so
                // `pub mod x; // driven by poll()` had its declaration
                // discarded at that `)` and vanished silently. Third
                // iteration of this same silent-pass shape in this one
                // parser, which is why the sample test below now exists
                // rather than the fix standing alone. Review finding on
                // PR #86.
                let line = line.split("//").next().unwrap_or(line).trim();
                // THE PAREN MUST BE THE VISIBILITY'S OWN. `split_once(')')`
                // searched the whole remainder of the line, so any later
                // paren ended the strip and the declaration after it was
                // discarded. Stripping `//` first removed one vector and
                // left the block-comment one -- `pub mod x; /* poll() */`
                // still vanished, and the comment below asserted it did
                // not. Requiring the match to START at `(` closes the
                // expression rather than a third symptom of it. Review
                // finding on PR #86, the third round to report this one
                // expression.
                let rest =
                    line.strip_prefix("pub")
                        .map_or(line, |after| match after.split_once(')') {
                            // `pub(crate) mod`, `pub(super) mod`,
                            // `pub(in crate::x) mod`.
                            Some((head, tail)) if head.starts_with('(') => tail.trim_start(),
                            // `pub mod`, or a paren that belongs to something
                            // else on the line.
                            _ => after.trim_start(),
                        });
                let rest = rest.trim_start().strip_prefix("mod ")?;
                // A FILE MODULE ENDS IN `;`. An inline `mod x { ... }` has no
                // file, so there is nothing for the table to cover and
                // skipping it is correct rather than a gap -- `mod.rs` has
                // five of them, all test modules. Checked BEFORE the name, so
                // an inline declaration is recognised rather than refused.
                // `next()` on a `split` is always `Some`, so this is an
                // unwrap with a name rather than a guard; the real filters
                // are the `;` test and the identifier test below.
                let head = rest.split('{').next().unwrap_or(rest);
                if !head.contains(';') {
                    return None;
                }
                // CUT AT THE FIRST `;`, not the last character, which is
                // what keeps `mod x ;` from hiding the declaration. A
                // trailing comment is handled above, by stripping `//` and
                // by requiring the visibility's paren to be its own -- not
                // here, which an earlier version of this comment claimed.
                let name = head.split(';').next().unwrap_or(head).trim();
                if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    return None;
                }
                Some(format!("{name}.rs"))
            })
            .collect()
    }

    #[test]
    fn every_module_spelling_this_file_could_use_is_recognised() {
        // A LITERAL SAMPLE, not `mod.rs`. The real file has `mod x;`, one
        // `pub mod x;` and five inline `mod x {`, so it exercises none of
        // the spellings that have actually broken this parser.
        //
        // EVERY LINE IS INDENTED, deliberately. A sample containing a `}`
        // at column zero ends the guard's own test-module cut early and
        // spills the rest of this module into the text it scans -- which is
        // not hypothetical: writing this test unindented is what produced
        // that failure, and it is the raw-string hazard a reviewer had
        // already predicted. The parser trims, so indentation changes
        // nothing it sees.
        let sample = "\
    mod a;
    pub mod b;
    pub(crate) mod c;
    pub(super) mod d;
    pub(in crate::runtime) mod e;
        mod f;
    mod g; // a trailing comment
    pub mod h; // driven by poll()
    mod i ;
    // mod commented_out;
    /// mod in_a_doc_comment;
    pub(crate) use dialing::canonical_for_peer;
";
        let want: Vec<String> = "a b c d e f g h i"
            .split(' ')
            .map(|n| format!("{n}.rs"))
            .collect();
        assert_eq!(
            declared_modules(sample),
            want,
            "every `mod x;` spelling in the sample must be seen, and nothing else"
        );
    }

    #[test]
    fn an_inline_module_declares_no_file_and_is_skipped() {
        // Correct rather than a gap: there is no file for the table to
        // cover. `mod.rs` has five of these and the first version of this
        // parser refused them, which was a red build for a valid shape.
        assert!(declared_modules("    mod inline {").is_empty());
        assert!(declared_modules("    pub(crate) mod inline {").is_empty());
    }

    #[test]
    fn a_nested_declaration_is_claimed_rather_than_ignored() {
        // THE ONE FALSE POSITIVE, recorded because it is the safe
        // direction. The parser is line-based, so a `mod x;` nested inside
        // an inline module reads as a file module -- and the `visited`
        // assertion then demands a table entry for a file that does not
        // exist, which fails LOUDLY. Over-claiming costs a build; missing a
        // declaration costs the coverage, which is the failure this whole
        // check exists to prevent.
        assert_eq!(
            declared_modules("        mod nested_inside_an_inline_module;"),
            vec![String::from("nested_inside_an_inline_module.rs")]
        );
    }

    #[test]
    fn the_spellings_this_parser_loses_are_recorded_rather_than_believed() {
        // NOT EVERY SHAPE RUST ALLOWS, and the test above is named for what
        // it covers rather than for that. These are the known losses, each
        // SILENT -- a lost declaration is a file the table need not list,
        // which contributes zero matches to an expectation of zero. Written
        // down so the list is asserted instead of assumed, and so adding a
        // shape here is the cheap way to extend the parser.
        //
        // `cargo fmt` normalises the first two onto their own lines, so CI
        // heals them; the rest are implausible in this module and none is
        // present today. If one ever is, the fix is this function.
        for lost in [
            "#[cfg(feature = \"relay\")] pub mod relaying;",
            "#[doc = \"m\"] mod x;",
            "mod r#fn;",
            "/* adapter */ mod x;",
            "mod /* adapter */ x;",
            // NEWLY lost by requiring the visibility's paren to start the
            // match, which is the fix one round up. Legal Rust; `rustfmt`
            // rewrites it to `pub(crate)` and CI runs `--check`, so it
            // cannot reach `main`. Recorded because this list is the
            // register of what the parser gives up, and the round that
            // introduced a loss is the round that should enter it.
            "pub (crate) mod x;",
            // Pre-existing, and on the list for the same reason: the
            // prefix test requires a literal space after `mod`.
            "mod\tx;",
        ] {
            assert!(
                declared_modules(lost).is_empty(),
                "this shape is a known LOSS; if the parser now sees it, move it \
                 to the recognised list: {lost}"
            );
        }

        // And two declarations on one line yields only the first.
        assert_eq!(
            declared_modules("mod a; mod b;"),
            vec![String::from("a.rs")],
            "the second declaration on a line is lost"
        );
    }

    #[test]
    fn a_commented_paren_does_not_swallow_a_pub_module() {
        // THE P3 THIS TEST EXISTS FOR. `split_once(')')` searched the whole
        // remainder of the line, so the `)` in a trailing comment ended the
        // visibility strip and the declaration after it was discarded --
        // silently, which for a file whose expected count is zero means the
        // guard goes on passing while the file is never scanned.
        assert_eq!(
            declared_modules("pub mod connectivity; // driven by poll()"),
            vec![String::from("connectivity.rs")]
        );
        assert_eq!(
            declared_modules("pub(crate) mod relaying; // reserves ) here"),
            vec![String::from("relaying.rs")]
        );
        // AND THE BLOCK-COMMENT SPELLING, which the `//` strip alone did
        // not cover and which a reviewer found still live after it.
        assert_eq!(
            declared_modules("pub mod connectivity; /* driven by poll() */"),
            vec![String::from("connectivity.rs")]
        );
        assert_eq!(
            declared_modules("pub(crate) mod relaying; /* reserves ) here */"),
            vec![String::from("relaying.rs")]
        );
    }

    #[test]
    fn no_production_path_learns_an_address_without_canonicalizing() {
        // THE WIRING, not the helper. Every test above could pass with the
        // production call sites reverted, because they all reach
        // `learn_route` themselves -- which is exactly the
        // passes-for-the-wrong-reason shape a reviewer named on PR #86.
        // The Identify arm cannot be unit-tested (`SwarmEvent` is
        // `#[non_exhaustive]`) and the command arm needs a live Swarm, so
        // the enforceable claim is structural: the book and the quarantine
        // are keyed through ONE wrapper for the DIRECT name, plus the
        // `ConnectionManager` methods that key them -- TWO taking an address
        // argument (`record_address_failure_unadmitted`,
        // `record_permanent_address_failure_unadmitted`) and FOUR taking a
        // TICKET (`record_failure`, `record_permanent_failure`,
        // `record_identity_mismatch`, `record_success`) -- plus the
        // canonicalization that makes a ticket's address safe in the first
        // place, and the POLICY's own `record_address_failure` at zero -- the
        // seventh method, added when a review found it in no table at all.
        // `record_failure` is on the ticket side: its signature is
        // `(&mut self, ticket: DialTicket, now_ms: u64)`, and an earlier
        // version of this sentence put it on the address side, contradicting
        // the sibling guard's own paragraph one commit away. This enumeration
        // has now gone stale three times, which is why the instruction below
        // it is to read the table. This fails if a further
        // CALL SITE of any of them appears in this module. READ THE TABLE,
        // not this sentence: it said "the three manager methods" and "one of
        // those four" after the table had grown to eight patterns, which is
        // the same enumeration defect this guard's own rounds kept finding.
        // Review findings on PR #86.
        //
        // IT CANNOT SEE A NEW MANAGER METHOD that reaches `learn_address`,
        // because the route table below is hand-maintained in this crate
        // against code in another. An earlier version of this sentence said
        // it "fails if a further path appears", which it does not. That half
        // is pinned where it can actually break:
        // `no_new_route_here_reaches_learn_address_unseen`, beside
        // `ConnectionManager` itself, fails if a new method there keys the
        // book or the quarantine -- reaching `learn_address` directly, or by
        // delegating to one of the two that do, or by writing the quarantine
        // through `policy.record_address_failure`, or by touching the `book`
        // field at all -- that guard counts `.book` and NOT `self.book`,
        // because rustfmt wraps a long chain between the receiver and the
        // field and one access is already written that way -- and names this
        // table as the thing to update.
        // Review finding on PR #86.
        //
        // FOUR VERSIONS OF THIS SENTENCE WERE WRONG ABOUT THE CODE. It
        // said "ONE wrapper" and stopped, which missed both; then it named
        // "the two settlement recorders", only one of which reaches
        // `learn_address`; then it dropped
        // `record_permanent_address_failure_unadmitted` on the ground that it
        // reaches `learn_address` through nothing -- true, and the wrong
        // question, because that method keys the BOOK directly. That is why
        // each route is now enumerated and asserted separately rather than
        // summarised.
        //
        // Reads the source rather than the binary, which is the weakness
        // worth stating: it cannot see a call built by a macro, it cannot
        // see an indented `#[cfg(test)]` on an item inside an `impl` (such
        // a block counts as production, which fails loudly if it calls
        // `learn_address` and is otherwise a silent pass -- the same
        // conditional as `#[cfg(all(test, ...))]` below), and it checks this
        // crate's runtime module only.
        // `ConnectionManager` is reachable from outside that module, so a
        // new `learn_address` caller elsewhere in the crate would pass.
        // EVERY FILE IN THE MODULE, not the three I had edited. The first
        // version scanned `dialing.rs`, `commands.rs` and `mod.rs`, which
        // left `kademlia_driver.rs` and `endpoints.rs` unscanned -- and the
        // driver is the most likely future home for an address-learning
        // call. A review named it.
        // THE TABLE BELOW IS CHECKED AGAINST THE MODULE, because a
        // hardcoded list is how this guard lost two files the first time.
        // Stage 11's next step adds a connectivity adapter to this module,
        // and an unscanned file contributes zero matches to an expectation
        // of zero -- the silent pass, one step later. Review finding on
        // PR #86.
        let declared: Vec<String> = declared_modules(include_str!("mod.rs"));
        assert!(
            !declared.is_empty(),
            "the module declarations could not be read out of mod.rs, so this \
             guard cannot tell what it is supposed to cover"
        );

        let mut visited: Vec<&str> = Vec::new();
        for (name, source) in [
            ("autonat_driver.rs", include_str!("autonat_driver.rs")),
            (
                "autonat_server_driver.rs",
                include_str!("autonat_server_driver.rs"),
            ),
            ("broadcast.rs", include_str!("broadcast.rs")),
            ("commands.rs", include_str!("commands.rs")),
            ("config.rs", include_str!("config.rs")),
            ("dcutr_driver.rs", include_str!("dcutr_driver.rs")),
            ("dialing.rs", include_str!("dialing.rs")),
            ("direct.rs", include_str!("direct.rs")),
            ("endpoints.rs", include_str!("endpoints.rs")),
            ("handle.rs", include_str!("handle.rs")),
            ("kademlia_driver.rs", include_str!("kademlia_driver.rs")),
            ("mdns_driver.rs", include_str!("mdns_driver.rs")),
            ("messages.rs", include_str!("messages.rs")),
            ("mod.rs", include_str!("mod.rs")),
            ("network_change.rs", include_str!("network_change.rs")),
            ("path_race.rs", include_str!("path_race.rs")),
            ("relay_driver.rs", include_str!("relay_driver.rs")),
            (
                "relay_server_driver.rs",
                include_str!("relay_server_driver.rs"),
            ),
        ] {
            // Tests are allowed to call the manager directly; the rule is
            // about production paths.
            //
            // EVERY test module is cut, not the first. `split_once` stopped
            // at the first `#[cfg(test)]` and `mod.rs` has five, so
            // everything after the first was unscanned -- and for a file
            // whose expected count is zero an under-count equals the
            // expectation, so the guard would have passed in silence.
            // CUT AT THE MODULE HEADER, and REQUIRE the shape rather than
            // dropping what does not match. The previous version split on
            // `#[cfg(test)]` and kept the text after the first `"\n}\n"`
            // in each tail, dropping the tail entirely when that was
            // absent. Dropping is the PERMISSIVE read here, not the
            // conservative one: nine of these ten files expect zero
            // matches, so discarding text can only move `calls` toward the
            // expectation. A review measured the `None` branch firing on
            // `direct.rs` today, and showed that a bare
            // `#[cfg(test)] mod tests;` -- ordinary Rust -- hides the whole
            // rest of a file behind it. Review finding on PR #86.
            //
            // So the only accepted shape is a file-level test module, and
            // anything else fails loudly instead of being swallowed.
            let mut production = String::new();
            let mut rest = source;
            while let Some((before, after)) = rest.split_once("\n#[cfg(test)]\nmod ") {
                production.push_str(before);
                // The module runs to the end of the file or to the next
                // column-zero item after its closing brace.
                // THE MODULE MUST BE INLINE. `#[cfg(test)] mod tests;`
                // declares a module in another FILE, so its `{` never
                // arrives and everything after it would be swallowed as
                // though it were test code -- which is the hole a review
                // found, since that is ordinary Rust and the files after
                // it expect zero matches. Refuse rather than guess.
                let head: &str = after.split_once('{').map_or(after, |(h, _)| h);
                assert!(
                    !head.contains(';'),
                    "{name}: `#[cfg(test)] mod <name>;` declares its tests in another file, \
                     and this guard cannot tell where they end -- so it refuses. Use an \
                     inline `mod tests {{ ... }}`, or extend this guard to follow the file."
                );
                // THE LEADING NEWLINE IS KEPT. `split_once` consumes the
                // separator, so `rest` used to begin at the character
                // AFTER `}\n` -- and the loop then looks for
                // `"\n#[cfg(test)]\nmod "` WITH a leading newline, so a
                // second test module sitting immediately after the first
                // with no blank line between them was never cut. `direct.rs`
                // is exactly that shape, so its `waiter_tests` module was
                // being counted as production: the comment claiming every
                // module is cut was false for one of the ten files, and a
                // test in there calling the manager directly -- which this
                // guard explicitly permits -- would have failed the build
                // with a message about production key domains. Measured by
                // a reviewer, not inferred. Review finding on PR #86.
                match after.split_once("\n}") {
                    // `tail` has lost the newline that `"\n}\n"` carried,
                    // so the next search is given one back. Done by
                    // splitting on `"\n}"` instead of `"\n}\n"` -- the
                    // surviving `\n` is the separator the next match needs.
                    Some((_, tail)) => rest = tail,
                    None => {
                        assert!(
                            after.trim_end().ends_with('}'),
                            "{name}: a `#[cfg(test)] mod` that neither closes at column zero \
                             nor ends the file -- this guard cannot tell its tests from its \
                             production code, so it refuses rather than guessing"
                        );
                        rest = "";
                    }
                }
            }
            production.push_str(rest);
            // EVERY occurrence, not the file as a whole. A first version
            // asked whether the file contained any `#[cfg(test)]\nmod `,
            // which a file with both a test module and a `#[cfg(test)]`
            // constant satisfies -- so the shape it was written to refuse
            // walked straight through. Measured, not assumed: planting a
            // `#[cfg(test)] const` in `endpoints.rs` left it green.
            for (i, _) in source.match_indices("\n#[cfg(test)]") {
                let after = &source[i + "\n#[cfg(test)]".len()..];
                assert!(
                    after.starts_with("\nmod "),
                    "{name}: a column-zero `#[cfg(test)]` that is not immediately \
                     followed by `mod ` -- the guard cannot tell where the test code \
                     ends, so it refuses rather than reading past it. If this is a \
                     test-only `use`, `const` or `fn`, move it inside the test module. \
                     If it is an attribute between `#[cfg(test)]` and `mod`, put the \
                     attribute first. If it is `pub mod` or `pub(crate) mod`, drop the \
                     visibility -- a test module needs none. (`#[cfg(all(test, ...))]` \
                     does NOT reach here: it is not matched at all, so its module is \
                     counted as production -- which fails the count assertion below \
                     only IF it calls `learn_address`, and is otherwise a silent \
                     pass. Measured both ways.) If the attribute and the `mod` are \
                     on ONE line, put the attribute on its own line: this check \
                     wants a newline between them."
                );
            }
            // EVERY ROUTE THAT KEYS THE BOOK OR THE QUARANTINE, PLUS THE
            // CANONICALIZATION THAT MAKES THE TICKET ONES SAFE, ASSERTED ONE
            // BY ONE.
            //
            // THE CANONICALIZATION ITSELF WAS PINNED BY NOTHING, which an
            // audit found after four rounds had rewritten this table. The
            // one change this branch exists for is
            // `address: canonical_dial_address(peer, address)` inside
            // `attempt_dial` -- and replacing it with `address.to_owned()`
            // left 202 tests and clippy green. No test anywhere drives
            // `attempt_dial` with a suffixed address: every new test calls
            // `canonical_dial_address` or `learn_route` directly, and
            // `grep -rn '/p2p/' tests/ --include=*.rs` returns nothing at
            // all. So the helper was covered, `admit` was covered, and the
            // line joining them was not. Counting
            // `canonical_dial_address(` is what closes it -- measured by
            // re-planting the same revert. Review findings on PR #86.
            //
            // THE SCOPE IS THE KEY, NOT `learn_address`. Writing it the other
            // way is what dropped `record_permanent_address_failure_unadmitted`
            // from this table: that method reaches `learn_address` through
            // nothing, which is true, and its body is `known.remove(address)`
            // against the book -- so a non-canonical argument there does not
            // mis-insert, it fails to REMOVE, and the undialable route holds
            // one of `max_addresses_per_peer` slots for the life of the
            // process. Two methods on `ConnectionManager` reach
            // `learn_address` (`record_failure` and
            // `record_address_failure_unadmitted`) and TWO more key the book
            // without it -- `record_permanent_address_failure_unadmitted`
            // and `record_permanent_failure`, the latter by
            // `ticket.address()`. An earlier version of this said "a third",
            // counting one of the two; the book is keyed in three places
            // over there, not two.
            // `no_new_route_here_reaches_learn_address_unseen` in
            // `interweave-transport-runtime` is what fails if another
            // appears, and it now counts the quarantine routes and the book
            // accesses as well.
            //
            // No line numbers here -- they drift silently, and the earlier
            // ones cited call sites rather than declarations.
            //
            // ONE ASSERTION PER PATTERN, not one on the sum. A single total
            // let a swap through: delete one `record_failure` site, add one
            // raw `learn_address`, and the sum is unchanged while a
            // production path learns a non-canonical address. Measured.
            //
            // It still cannot check the ARGUMENT, so a bad value handed to
            // one of the existing sites passes -- no count here, because
            // this one has now been restated four times and been wrong
            // twice. Said rather than assumed. Review findings on PR #86.
            let routes: [(&str, usize); 10] = [
                // `learn_route`, the only direct caller.
                ("learn_address(", 1),
                // `settle_failed_dial`'s non-structural arm for the extra
                // addresses of a multi-address failure.
                ("record_address_failure_unadmitted(", 1),
                // `attempt_dial`'s synchronous refusal, and
                // `settle_failed_dial`'s transient arm and its hole-punch
                // arm (step 8: a punch dial's ticket settles and scores
                // nothing, and the manager decides that by origin).
                ("record_failure(", 3),
                // `settle_failed_dial`'s STRUCTURAL arm for those same extra
                // addresses: removes the route from the book rather than
                // scoring it, which is why it is keyed and not scored.
                ("record_permanent_address_failure_unadmitted(", 1),
                // THE CANONICALIZATION, which is the thing this whole branch
                // is for: `attempt_dial`'s use of it, its own declaration,
                // and `learn_route`'s. Dropping any one of the three fails
                // here, which is what the audit found nothing else did.
                ("canonical_dial_address(", 3),
                // And the function it wraps, which is where the key actually
                // gets computed: its declaration, the wrapper's call, and
                // `settle_failed_dial`'s `strip` closure. The FOURTH site is
                // in `outbound_gate.rs` and out of this scan's reach -- the
                // established hook -- so it has a guard of its own beside it,
                // `the_established_hook_still_canonicalizes_the_rebound_address`.
                ("canonical_for_peer(", 3),
                // Ticket-carried and keyed by `ticket.address()`: the book,
                // the quarantine, the success score. Safe only because
                // `attempt_dial` canonicalizes above, which is why that
                // count and these belong in one table.
                ("record_permanent_failure(", 3),
                ("record_identity_mismatch(", 1),
                ("record_success(", 1),
                // THE QUARANTINE WRITE ITSELF, expected ZERO in every file
                // including this one. `ConnectionPolicy::record_address_failure`
                // is `pub`, `mod.rs` builds a `ConnectionPolicy` in production,
                // and the sibling guard in `interweave-transport-runtime` reads
                // only its own file -- so a production
                // `policy.record_address_failure(&peer, raw, ..)` anywhere in
                // this module would write the quarantine from an
                // uncanonicalized string and be counted by NEITHER guard. A
                // review found that hole while checking a sentence that claimed
                // the sibling covered it.
                //
                // THE MANAGER CANONICALIZES NOTHING, and an earlier version of
                // this comment said going through it was what made an address
                // safe. It does not: `record_address_failure_unadmitted` hands
                // the caller's `&str` straight to the policy, and
                // `learn_address` does `known.insert(address.to_owned())`. The
                // canonicalization is at the CALL SITES in this file --
                // `learn_route`, `attempt_dial`, `settle_failed_dial`'s `strip`
                // -- which is exactly why those are what the table counts. So
                // the reason zero is right is narrower than "use the manager":
                // the quarantine must be reached through a COUNTED call site
                // whose argument was canonicalized there, and a direct policy
                // call is counted by nothing. "Go through the manager and you
                // are safe" is how the next raw address gets written.
                //
                // STILL NOT CLOSED OUTSIDE THIS MODULE: a `ConnectionPolicy`
                // built and written to in, say, `outbound_gate.rs` is outside
                // this scan's ten files and outside the sibling's own file.
                // It is a substring of no other pattern here: the `(` is what
                // separates it from `record_address_failure_unadmitted(`.
                // Review finding on PR #86.
                ("record_address_failure(", 0),
            ];
            for (pattern, in_dialing) in routes {
                let calls = production.matches(pattern).count();
                let expected = if name == "dialing.rs" { in_dialing } else { 0 };
                assert_eq!(
                    calls, expected,
                    "{name} keys the book or the quarantine through `{pattern}` \
                     {calls} time(s), expected {expected}. IF THE COUNT ROSE: \
                     for an address-taking method call `learn_route` instead, \
                     which canonicalizes so the book, the quarantine and the \
                     ticket agree; for a TICKET-taking one (`record_failure`, \
                     `record_success`, `record_identity_mismatch`, \
                     `record_permanent_failure`) there is no `learn_route` \
                     form -- mint the ticket through \
                     `attempt_dial`, which canonicalizes, then raise the count \
                     here and name the new site. IF IT FELL, a site was removed \
                     or renamed: lower it here and in \
                     `no_new_route_here_reaches_learn_address_unseen`. \
                     `record_address_failure(` is NEITHER: it is the policy's \
                     own method, so there is no `learn_route` form and no \
                     ticket -- reach the quarantine through a counted \
                     `ConnectionManager` method, canonicalizing the argument \
                     at that call site. If this \
                     is a TEST call, the guard failed to cut its module -- see \
                     the shapes it accepts above. If it is a doc comment, write \
                     the name without the parenthesis."
                );
            }
            visited.push(name);
        }

        for file in &declared {
            assert!(
                visited.contains(&file.as_str()),
                "mod.rs declares {file} and this guard does not scan it -- add it \
                 to the table above, or a production `learn_address` call there \
                 passes unseen"
            );
        }
    }
}
