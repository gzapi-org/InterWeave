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
use libp2p::{Multiaddr, identify};
use tokio::sync::oneshot;

use interweave_transport_api::TransportIdentity;
use interweave_transport_runtime::{
    ConnectionClass, ConnectionManager, ConnectionSlot, DialOrigin, DialRequest, DialTicket,
    Revoked,
};

use crate::behaviour::SubstrateBehaviourEvent;
use crate::gated_swarm::{AdmittedDial, GatedSwarm, UndialableAdmission};
use crate::outbound_gate::{InFlightTickets, strip_own_suffix, strip_peer_suffix};

use super::messages::DialRefusal;
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
        address: address.to_owned(),
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

/// Settle an admission that could not be turned into a dial, and say why.
///
/// PERMANENT, not transient. Every way `AdmittedDial::from_ticket`
/// fails is a deterministic property of the ticket itself -- it names
/// no peer, its peer is not a libp2p `PeerId`, or its address is not a
/// multiaddr -- so the same ticket converts the same way every time,
/// whatever the network does. `record_failure` reschedules, so a
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
    now_ms: u64,
) -> Option<(ConnectionSlot, DialOrigin)> {
    let class = manager.classify(peer);
    if !manager.authorizes_for(class, ticket.origin()) {
        manager.record_authorization_withdrawn(ticket, now_ms);
        return None;
    }
    // THE ADDRESS THAT WORKED. Learned from the ticket rather than from
    // anything the peer said, so a route this profile has actually
    // authenticated is in the book even if the peer never advertises it.
    let address = ticket.address().to_owned();
    let origin = ticket.origin();
    let slot = manager.record_success(ticket, now_ms);
    let _ = manager.learn_address(peer, &address, now_ms);
    Some((slot, origin))
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
    let expected = ticket
        .peer()
        .and_then(|p| p.as_str().parse::<libp2p::PeerId>().ok());
    let strip = |address: &Multiaddr| match &expected {
        // The connection's peer is authenticated knowledge: only ITS
        // claim strips, so a foreign claim stays in the settlement key
        // and the policy records the literal that lied.
        Some(peer) => strip_own_suffix(address, peer),
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
                        if attempt_is_structural(attempt_error) {
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
            attempt_is_structural(&attempts[0].1)
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
fn attempt_is_structural(error: &TransportError<std::io::Error>) -> bool {
    matches!(error, TransportError::MultiaddrNotSupported(_))
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
                    .all(|(_, e)| matches!(e, TransportError::MultiaddrNotSupported(_)))
        }
        _ => false,
    }
}

/// Release the admission a connection outcome belongs to.
///
/// The two events that end an outbound attempt are the established
/// connection and the outgoing error. Both carry the `ConnectionId` the
/// dial was built with, which is why the ticket is filed under it: no
/// matching by address, no guessing from a peer that may appear twice.
///
/// An event for a connection this runtime did not dial -- anything
/// inbound -- finds no ticket and does nothing, which is correct rather
/// than merely harmless: inbound connections were never admitted
/// through the dial gate and have no slot to return.
pub(super) fn settle_outcome(
    event: &Libp2pSwarmEvent<SubstrateBehaviourEvent>,
    manager: &mut ConnectionManager,
    in_flight: &InFlightTickets,
    open: &mut HashMap<libp2p::swarm::ConnectionId, OpenConnection>,
    refuse: &mut Vec<libp2p::swarm::ConnectionId>,
    now_ms: u64,
) -> Announce {
    match event {
        Libp2pSwarmEvent::ConnectionEstablished {
            connection_id,
            peer_id,
            ..
        } => {
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
                Some(ticket) => match settle_established_outbound(manager, &peer, ticket, now_ms) {
                    Some((slot, origin)) => {
                        open.insert(
                            *connection_id,
                            OpenConnection {
                                peer,
                                slot,
                                origin: Some(origin),
                            },
                        );
                    }
                    None => {
                        refuse.push(*connection_id);
                        return Announce::Suppress;
                    }
                },
                // INBOUND HAS NO ADMISSION. ADR-0011: the same current
                // authorization that governs outbound applies before an
                // inbound data-plane connection is retained -- arriving
                // is not an authorization. The ceiling is the second
                // question, because a connection this profile will not
                // keep should not spend a slot to find that out.
                None => {
                    let class = manager.classify(&peer);
                    if !manager.authorizes(class) {
                        refuse.push(*connection_id);
                        return Announce::Suppress;
                    }
                    match manager.admit_inbound() {
                        Some(slot) => {
                            open.insert(
                                *connection_id,
                                OpenConnection {
                                    peer,
                                    slot,
                                    origin: None,
                                },
                            );
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
        Libp2pSwarmEvent::Behaviour(SubstrateBehaviourEvent::Identify(
            identify::Event::Received { peer_id, info, .. },
        )) => {
            if let Ok(peer) = to_transport_identity(peer_id) {
                for address in &info.listen_addrs {
                    let _ = manager.learn_address(&peer, &address.to_string(), now_ms);
                }
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
/// admitted by the origin-less `authorizes` and is re-asked the same
/// question, so a revocation that reaches the data plane still closes
/// it.
pub(super) fn connections_to_close<'a>(
    manager: &ConnectionManager,
    revoked: &[Revoked],
    open: impl Iterator<
        Item = (
            libp2p::swarm::ConnectionId,
            &'a TransportIdentity,
            Option<DialOrigin>,
        ),
    >,
) -> BTreeSet<libp2p::swarm::ConnectionId> {
    let revoked_class: BTreeMap<&TransportIdentity, ConnectionClass> = revoked
        .iter()
        .map(|entry| (&entry.peer, entry.now))
        .collect();
    let mut closing = BTreeSet::new();
    for (id, peer, origin) in open {
        let Some(now) = revoked_class.get(peer) else {
            continue;
        };
        let still_authorized = match origin {
            Some(origin) => manager.authorizes_for(*now, origin),
            None => manager.authorizes(*now),
        };
        if !still_authorized {
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
    /// Why this connection was opened, or `None` for one that arrived.
    ///
    /// ADR-0036's separation is an origin/class PAIR, so a trust change
    /// cannot be re-evaluated from the class alone. Without this a peer
    /// that lost only its data-plane trust -- still infrastructure --
    /// had every connection to it closed, including relay reservations
    /// and AutoNAT probes that `authorizes_for` would still permit.
    ///
    /// Inbound is `None` because arriving is not a dial: it was admitted
    /// with the origin-less `authorizes`, and it is re-evaluated the
    /// same way.
    pub(super) origin: Option<DialOrigin>,
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
        connections_to_close, is_permanent_dial_error, settle_established_outbound,
        settle_failed_dial, settle_undialable,
    };
    use crate::gated_swarm::AdmittedDial;
    use interweave_transport_api::TransportIdentity;
    use interweave_transport_runtime::{
        ConnectionManager, ConnectionPolicy, DialOrigin, TrustSources,
    };
    use interweave_transport_runtime::{DialRequest, DialTicket};
    use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
    use libp2p::Multiaddr;
    use libp2p::core::transport::TransportError;
    use libp2p::swarm::{ConnectionId, DialError};

    const RELAY: &str = "12D3KooWCLxLXFHqvfsHVLDcNsSpZBQq1M1KMRgQRLLLnHTv7oQD";

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

    /// A peer trusted BOTH ways loses only its data-plane trust.
    ///
    /// ADR-0036 keeps the two authorizations separate, so this peer is
    /// still infrastructure and its relay reservation is still
    /// authorized. Deciding from the class alone -- which is what
    /// closing every connection to a reported peer does -- drops the
    /// reachability the peer is still trusted for.
    #[test]
    fn partial_revocation_keeps_the_reachability_it_still_authorizes() {
        let mut m = manager(&[RELAY], &[RELAY]);
        let peer = ident(RELAY);
        let revoked = m.set_trust(trust(&[], &[RELAY]), std::slice::from_ref(&peer));
        assert_eq!(revoked.len(), 1, "the data-plane loss IS a revocation");

        let reservation = ConnectionId::new_unchecked(1);
        let closing = connections_to_close(
            &m,
            &revoked,
            [(reservation, &peer, Some(DialOrigin::RelayReservation))].into_iter(),
        );
        assert!(
            closing.is_empty(),
            "an infrastructure peer keeps the connection it is still authorized for"
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
            [(data, &peer, Some(DialOrigin::ConnectionManager))].into_iter(),
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
        let closing = connections_to_close(&m, &revoked, [(inbound, &peer, None)].into_iter());
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
            [(ConnectionId::new_unchecked(4), &peer, None)].into_iter(),
        );
        assert!(closing.is_empty());
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
            "BOTH exhausted addresses were scored and learned, not only              the one the ticket settled"
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
            "the quarantine binds to the REAL address, stripped of its              suffix — settled on the placeholder it would bind to nothing"
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
            settle_established_outbound(&mut m, &peer, ticket, 5).is_none(),
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
        let (slot, origin) =
            settle_established_outbound(&mut m, &peer, ticket, 5).expect("trusted and kept");
        assert_eq!(origin, DialOrigin::KademliaQuery);
        assert_eq!(
            m.known_addresses(&peer),
            1,
            "the address that worked is in the book (F12's whole point)"
        );
        drop(slot);
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
}
