// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Inbound direct-v2 admission: the order the pieces run in.
//!
//! Every component this composes already existed and is tested on its
//! own. What did not exist is the ORDER, and the order is the security
//! property — each step is a gate the next one is entitled to assume has
//! passed, so a call site that assembled them in a different sequence
//! would be individually correct and collectively wrong.
//!
//! ```text
//! profile trust        who is this, and may they talk to us at all
//! draining             we are going away; take on no new work
//! ingress rate         a trusted peer may still be flooding
//! content fingerprint  the frame is intact enough to have an identity
//! dedup cache          have we already accepted this exact message
//! reservation          claim the key, or attach to whoever holds it
//! endpoint resolution  which local endpoint, incl. the default
//! endpoint policy      narrowing over profile trust, never widening
//! queue admission      THE acceptance point
//! record accepted      so a retry replays this route
//! ```
//!
//! # What each ordering choice buys
//!
//! **Trust before rate.** An untrusted peer must not consume a token; a
//! limiter that charged first would let an unauthorized peer spend the
//! allowance of authorized ones on the way to being refused.
//!
//! **Trust before draining, draining before everything else.** An
//! untrusted peer learns that it is untrusted, not what state this node
//! is in. Past that, a draining node refuses before it charges a token or
//! touches the cache: `AcceptedV2` promises a bounded queue took the
//! message, and a node about to drop that queue cannot honestly promise
//! it. Draining is not stopping — connections opened before the drain
//! stay up — so without this step the only thing stopping new work is
//! that no new *connections* are admitted.
//!
//! **Rate before dedup.** ADR-0019 is explicit that a rate-limited retry
//! receives coarse `overloaded` and **must not delete or mutate a prior
//! positive dedup entry**. Consulting the cache first and then refusing
//! would be a path where a flood erases an accepted route.
//!
//! **Dedup before reservation.** A message already accepted needs no
//! reservation at all — taking one would charge the budget for work that
//! is a cache read.
//!
//! **Reservation before resolution.** Only the owner resolves and
//! enqueues; matching concurrent duplicates attach as waiters and receive
//! the owner's outcome. This is what makes at-most-once presentation hold
//! under concurrent retransmission rather than only sequential retry —
//! the property SPIKE-002's A6 measured.
//!
//! **Queue admission last, and it is the acceptance.** `AcceptedV2` is
//! sent only after the endpoint's bounded queue took the event, so a full
//! queue is `overloaded` and never a false acceptance.
//!
//! # What this module does not do
//!
//! No I/O, no libp2p, no awaiting. It decides; the backend speaks. That
//! split is what lets the ordering above be unit-tested at all, and it is
//! why SPIKE-002 finding 2 — that producing a response is not evidence
//! the peer heard it — belongs to the caller rather than here.

use interweave_transport_api::{
    DirectMessageV2, DirectRejectReason, EndpointId, TransportIdentity,
};
use interweave_trust_api::{EndpointTrustPolicy, PeerTrustPolicy, TrustDecision};

use crate::dedup::{
    Admission, DedupCache, DedupKey, DestinationSelector, Reservation, ReservationFailure,
    ReservationMap,
};
use crate::endpoint_queue::{DirectEvent, EndpointQueues, QueueRefusal};
use crate::endpoint_registry::{EndpointRegistry, ResolveFailure};
use crate::fingerprint::direct_content_fingerprint_v1;
use crate::ingress::IngressLimiter;

/// The two clocks one admission needs, named so they cannot be swapped.
///
/// They measure different things and neither substitutes for the other.
/// `monotonic_ms` never goes backwards and is what rate buckets, dedup
/// TTLs and deadlines are computed from — a wall clock stepped by NTP or
/// an operator would hand a peer free allowance or expire an entry
/// early. `wall_ms` is Unix-epoch milliseconds and is what a RECEIPT
/// TIME has to be: it survives a restart, orders against events from
/// another process lifetime, and converts to the RFC3339 the contract
/// asks a client to show.
///
/// `received_at` was first taken from the monotonic clock, which made
/// every delivered event start near zero after a restart.
#[derive(Debug, Clone, Copy)]
pub struct Clocks {
    /// Milliseconds since this runtime started. Never goes backwards.
    pub monotonic_ms: u64,
    /// Milliseconds since the Unix epoch.
    pub wall_ms: u64,
}

/// Everything admission reads and writes, borrowed for one decision.
///
/// A struct rather than eight parameters because the ORDER they are used
/// in is this module's subject, and a caller that could pass them
/// individually could also reach past this function and use them
/// directly in some other order.
pub struct AdmissionContext<'a> {
    /// Who this profile trusts, at profile scope.
    pub trust: &'a PeerTrustPolicy,
    /// Direct-ingress token buckets.
    pub ingress: &'a mut IngressLimiter,
    /// The duplicate cache.
    pub dedup: &'a mut DedupCache,
    /// In-flight reservations.
    pub reservations: &'a mut ReservationMap,
    /// Configured endpoints, their policies and their leases.
    pub registry: &'a EndpointRegistry,
    /// Open delivery queues.
    pub queues: &'a mut EndpointQueues,
    /// Whether the node has begun draining.
    ///
    /// Draining is not stopping: existing connections stay up, so a peer
    /// that connected before the drain can still send. What it must not
    /// get is `AcceptedV2` for work this node is about to discard.
    pub draining: bool,
}

/// What admission decided, in local terms.
///
/// Local, deliberately. The wire code comes from [`Self::to_wire`], and
/// keeping the two apart is what lets an operator see why a message was
/// refused while a peer sees only that it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The endpoint's queue took it. Answer `AcceptedV2` with this route.
    Accepted {
        /// The endpoint that accepted it.
        resolved_endpoint: EndpointId,
    },
    /// Already accepted earlier with matching content.
    ///
    /// Answer `AcceptedV2` with the STORED route and do not deliver
    /// again. The stored route wins even if the profile default has
    /// changed since — that is what makes a retry idempotent rather than
    /// merely tolerated.
    DuplicateAccepted {
        /// The endpoint the first attempt resolved to.
        resolved_endpoint: EndpointId,
    },
    /// Another request holds this key; share its outcome.
    ///
    /// The caller holds this response channel until the owner resolves,
    /// and then answers every waiter identically. Never a second enqueue.
    AttachedAsWaiter,
    /// Refused, with the local reason.
    Refused(Refusal),
}

/// Why admission refused, before collapsing to a wire code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Profile trust does not admit this peer.
    UntrustedPeer,
    /// The node is draining and will not take on new work.
    Draining,
    /// The peer's own or the shared ingress allowance is exhausted.
    RateLimited,
    /// The frame's content could not be fingerprinted.
    ///
    /// Only an invalid media type reaches here; the codec has already
    /// refused a frame that did not parse.
    Unfingerprintable,
    /// Same key, different content.
    DuplicateConflict,
    /// A reservation budget refused it.
    Overloaded,
    /// Routing found no usable endpoint.
    NoRoute(ResolveFailure),
    /// The endpoint's queue refused it.
    Queue(QueueRefusal),
}

impl Refusal {
    /// The coarse code a peer may receive.
    ///
    /// Total by construction. Every arm is written out rather than
    /// defaulted, because a missing arm would otherwise be filled in at a
    /// call site under deadline — and the five-way collapse into
    /// `no_route` is the anti-oracle property, not a formatting choice.
    #[must_use]
    pub const fn to_wire(&self) -> DirectRejectReason {
        match self {
            Self::UntrustedPeer => DirectRejectReason::UnauthorizedPeer,
            Self::Draining => DirectRejectReason::ShuttingDown,
            Self::RateLimited | Self::Overloaded => DirectRejectReason::Overloaded,
            Self::Unfingerprintable | Self::DuplicateConflict => DirectRejectReason::Malformed,
            // Both already collapse everything they carry.
            Self::NoRoute(inner) => inner.to_wire(),
            Self::Queue(inner) => inner.to_wire(),
        }
    }
}

impl Outcome {
    /// The wire answer, when this is a refusal.
    #[must_use]
    pub const fn refusal_wire(&self) -> Option<DirectRejectReason> {
        match self {
            Self::Refused(r) => Some(r.to_wire()),
            _ => None,
        }
    }
}

/// Run one inbound direct-v2 request through admission.
///
/// `source_peer` is the **authenticated** remote identity — Noise proved
/// it. The frame's `source_endpoint` is peer-asserted and is used as a
/// dedup dimension, never as authorization.
///
/// The reservation is released here on every path this function decides.
/// A caller holding waiters' response channels answers them when the
/// owner's outcome arrives — `ReservationMap::release` already returns
/// the owner's and every waiter's budget together, so a waiter needs no
/// settling of its own.
/// The gates EVERY inbound request passes, parsable or not.
///
/// Extracted because a frame that failed to decode still arrived from
/// some peer, over some connection, at some rate — and answering it is
/// work. Answering before these ran let a peer spend no allowance to
/// make this node encode and send a rejection, and let a peer with no
/// data-plane trust draw a data-plane response. Both paths now run the
/// same three gates in the same order, which is the property this
/// module exists to hold.
///
/// # Errors
/// Returns the local [`Refusal`]; the caller collapses it to a wire code.
pub fn admit_prefix(
    source_peer: &TransportIdentity,
    now_ms: u64,
    ctx: &mut AdmissionContext<'_>,
) -> Result<(), Refusal> {
    // 1. PROFILE TRUST. Before a token is charged, so an unauthorized
    //    peer cannot spend an authorized peer's allowance being refused.
    if matches!(ctx.trust.decide(source_peer), TrustDecision::Denied(_)) {
        return Err(Refusal::UntrustedPeer);
    }

    // 2. DRAINING. `AcceptedV2` means this node took the message into a
    //    bounded queue, and a draining node is about to drop that queue —
    //    so accepting here would be a lie the sender acts on. Refusing
    //    before the token is charged, because a peer gains nothing by
    //    spending allowance to be told the node is going away.
    if ctx.draining {
        return Err(Refusal::Draining);
    }

    // 3. INGRESS RATE. A trusted peer may still be flooding.
    if ctx.ingress.admit(source_peer, now_ms).is_err() {
        // AND NOTHING ELSE HAPPENS. ADR-0019: a rate-limited retry gets
        // coarse `overloaded` and must not delete or mutate the prior
        // positive dedup entry. Returning here is what guarantees that,
        // rather than a comment asking a later branch not to.
        return Err(Refusal::RateLimited);
    }
    Ok(())
}

/// Run one inbound direct-v2 request through admission.
///
/// `source_peer` is the **authenticated** remote identity — Noise proved
/// it. The frame's `source_endpoint` is peer-asserted and is used as a
/// dedup dimension, never as authorization.
///
/// Begins with [`admit_prefix`], the gates every inbound request passes
/// whether or not it decoded, and then runs the structured half that
/// needs a frame: fingerprint, dedup, reservation, resolution, endpoint
/// policy, queue admission.
///
/// The reservation is released here on every path this function decides.
/// A caller holding waiters' response channels answers them when the
/// owner's outcome arrives — `ReservationMap::release` already returns
/// the owner's and every waiter's budget together, so a waiter needs no
/// settling of its own.
pub fn admit_inbound(
    frame: &DirectMessageV2,
    source_peer: &TransportIdentity,
    clocks: Clocks,
    ctx: &mut AdmissionContext<'_>,
) -> Outcome {
    let Clocks {
        monotonic_ms: now_ms,
        wall_ms,
    } = clocks;
    if let Err(refusal) = admit_prefix(source_peer, now_ms, ctx) {
        return Outcome::Refused(refusal);
    }

    // 4. CONTENT IDENTITY. Needed by both the cache and the reservation,
    //    and it excludes `sent_at_ms` — a retry may carry a different one.
    let Ok(fingerprint) = direct_content_fingerprint_v1(
        frame.payload.media_type().map(|m| m.as_str()),
        frame.payload.bytes(),
    ) else {
        return Outcome::Refused(Refusal::Unfingerprintable);
    };

    let key = dedup_key(frame, source_peer);

    // 5. DEDUP. An already-accepted message replays its STORED route and
    //    is not delivered again, even if the default has since changed.
    match ctx.dedup.admit(&key, fingerprint, now_ms) {
        Admission::DuplicateAccepted { resolved_endpoint } => {
            return Outcome::DuplicateAccepted { resolved_endpoint };
        }
        Admission::Conflict => return Outcome::Refused(Refusal::DuplicateConflict),
        Admission::Fresh => {}
    }

    // 6. RESERVATION. Only the owner proceeds; a matching concurrent copy
    //    attaches and will receive the owner's outcome.
    match ctx.reservations.acquire(&key, fingerprint) {
        Ok(Reservation::Waiter) => return Outcome::AttachedAsWaiter,
        Err(ReservationFailure::Overloaded) => {
            return Outcome::Refused(Refusal::Overloaded);
        }
        Err(ReservationFailure::Conflict) => {
            return Outcome::Refused(Refusal::DuplicateConflict);
        }
        Ok(Reservation::Owner) => {}
    }

    // From here the key is HELD. Every exit must settle it, or the budget
    // leaks and this peer's allowance decays under its own retries.
    let resolved = match ctx.registry.resolve_inbound(
        frame.destination_endpoint.as_ref(),
        // 6-7. Endpoint policy NARROWS profile trust. The intersection
        //      lives in trust-api so no call site can invert the order
        //      and widen it.
        |policy: &EndpointTrustPolicy| {
            matches!(
                ctx.trust.decide_for_endpoint(source_peer, policy),
                TrustDecision::Allowed
            )
        },
    ) {
        Ok((endpoint, _lease)) => endpoint,
        Err(failure) => {
            ctx.reservations.release(&key);
            return Outcome::Refused(Refusal::NoRoute(failure));
        }
    };

    // 9. QUEUE ADMISSION. THE acceptance point: `AcceptedV2` is sent only
    //    after this returned Ok.
    let event = DirectEvent {
        source_peer: source_peer.clone(),
        source_endpoint: frame.source_endpoint.clone(),
        destination_endpoint: resolved.clone(),
        message_id: frame.message_id,
        payload: frame.payload.clone(),
        // AT ADMISSION, not at drain: the queue is bounded and an event
        // may wait in it, so a drain-time stamp would drift by however
        // long the consumer was behind.
        //
        // And from the WALL clock, not the monotonic one that governs
        // rate limits and deadlines. A receipt time must survive a
        // restart and order against events from another process
        // lifetime; milliseconds-since-startup does neither.
        received_at: wall_ms,
    };
    if let Err(refusal) = ctx.queues.push(event) {
        ctx.reservations.release(&key);
        return Outcome::Refused(Refusal::Queue(refusal));
    }

    // 10. RECORD, so a retry replays this route rather than re-resolving.
    ctx.dedup
        .record_accepted(key.clone(), resolved.clone(), fingerprint, now_ms);
    ctx.reservations.release(&key);

    Outcome::Accepted {
        resolved_endpoint: resolved,
    }
}

/// Build the dedup key for a frame, for a caller that must settle waiters.
///
/// Exposed because the reservation is released by whoever owns the
/// response channels, and that caller needs the same key this function
/// computed rather than a second construction of it that could disagree.
#[must_use]
pub fn dedup_key(frame: &DirectMessageV2, source_peer: &TransportIdentity) -> DedupKey {
    DedupKey::Direct {
        source_peer: source_peer.clone(),
        source_endpoint: frame.source_endpoint.clone(),
        destination_selector: frame
            .destination_endpoint
            .clone()
            .map_or(DestinationSelector::Default, DestinationSelector::Explicit),
        message_id: frame.message_id,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use interweave_local_client_api::Generation;
    use interweave_transport_api::{MediaType, MessageId, Payload};

    use super::*;
    use crate::dedup::{DEFAULT_TTL_MS, ReservationMap};
    use crate::endpoint_registry::{LocalSessionId, RegisteredEndpoint};

    const P1: &str = "12D3KooWA9hFCGwGCpCbWWfLmYSpqPzXgLmPvbBrgWGNvNGSDVpS";
    const P2: &str = "12D3KooWLRPJAEfHFKtqAJEs4qWm4YrhTHkYCFjM8CutJmwyGMXP";

    fn peer(text: &str) -> TransportIdentity {
        TransportIdentity::parse(text).expect("valid peer id")
    }

    fn endpoint(name: &str) -> EndpointId {
        EndpointId::parse(name).expect("valid endpoint id")
    }

    /// A registry with `human` and `claude`, `human` the default, both
    /// leased so routing can succeed.
    fn registry() -> EndpointRegistry {
        let mut endpoints = BTreeMap::new();
        endpoints.insert(endpoint("human"), RegisteredEndpoint::default());
        endpoints.insert(endpoint("claude"), RegisteredEndpoint::default());
        let mut registry = EndpointRegistry::new(endpoints, Some(endpoint("human")));
        for name in ["human", "claude"] {
            registry
                .claim(
                    &endpoint(name),
                    LocalSessionId(format!("session-{name}")),
                    "test-client",
                    Generation::parse(format!("{name:_<16}")).expect("valid generation"),
                )
                .expect("the endpoint is claimable");
        }
        registry
    }

    fn queues() -> EndpointQueues {
        let mut queues = EndpointQueues::new();
        queues.open(endpoint("human"), 8);
        queues.open(endpoint("claude"), 8);
        queues
    }

    fn frame(destination: Option<&str>, body: &[u8], id: u8) -> DirectMessageV2 {
        DirectMessageV2 {
            message_id: MessageId::from_bytes([id; 16]),
            sent_at_ms: 1_000,
            source_endpoint: endpoint("human"),
            destination_endpoint: destination.map(endpoint),
            payload: Payload::at_ceiling(
                Some(MediaType::parse("text/plain").expect("valid media type")),
                body.to_vec(),
            )
            .expect("within the ceiling"),
        }
    }

    /// Everything admission touches, owned so a test can inspect it after.
    struct World {
        trust: PeerTrustPolicy,
        ingress: IngressLimiter,
        dedup: DedupCache,
        reservations: ReservationMap,
        registry: EndpointRegistry,
        queues: EndpointQueues,
        draining: bool,
    }

    impl World {
        fn new() -> Self {
            Self {
                trust: PeerTrustPolicy::new([peer(P1)]).expect("a one-peer allowlist"),
                ingress: IngressLimiter::with_defaults(0),
                dedup: DedupCache::new(64, DEFAULT_TTL_MS),
                reservations: ReservationMap::new(128, 8),
                registry: registry(),
                queues: queues(),
                draining: false,
            }
        }

        fn admit(
            &mut self,
            frame: &DirectMessageV2,
            from: &TransportIdentity,
            now: u64,
        ) -> Outcome {
            let mut ctx = AdmissionContext {
                trust: &self.trust,
                ingress: &mut self.ingress,
                dedup: &mut self.dedup,
                reservations: &mut self.reservations,
                registry: &self.registry,
                queues: &mut self.queues,
                draining: self.draining,
            };
            admit_inbound(
                frame,
                from,
                Clocks {
                    monotonic_ms: now,
                    wall_ms: now,
                },
                &mut ctx,
            )
        }
    }

    #[test]
    fn an_explicit_destination_is_accepted_onto_its_own_endpoint() {
        let mut w = World::new();
        let outcome = w.admit(&frame(Some("claude"), b"hi", 1), &peer(P1), 0);
        assert_eq!(
            outcome,
            Outcome::Accepted {
                resolved_endpoint: endpoint("claude")
            }
        );
        assert_eq!(w.queues.len(&endpoint("claude")), 1);
        assert_eq!(w.queues.len(&endpoint("human")), 0, "and nowhere else");
    }

    #[test]
    fn an_omitted_destination_resolves_to_the_configured_default() {
        let mut w = World::new();
        let outcome = w.admit(&frame(None, b"hi", 2), &peer(P1), 0);
        assert_eq!(
            outcome,
            Outcome::Accepted {
                resolved_endpoint: endpoint("human")
            },
            "omitted means the default, never fan-out"
        );
        assert_eq!(w.queues.len(&endpoint("human")), 1);
        assert_eq!(w.queues.len(&endpoint("claude")), 0);
    }

    /// TRUST BEFORE RATE. An untrusted peer must not spend a token, so it
    /// cannot exhaust the allowance of peers that are authorized.
    #[test]
    fn a_draining_node_refuses_without_spending_ingress_or_queueing() {
        let mut w = World::new();
        w.draining = true;
        let before = w.ingress.tracked_peers();
        let outcome = w.admit(&frame(Some("claude"), b"hi", 40), &peer(P1), 0);
        assert_eq!(outcome, Outcome::Refused(Refusal::Draining));
        assert_eq!(
            w.ingress.tracked_peers(),
            before,
            "a peer spends no allowance to be told the node is going away"
        );
        assert_eq!(
            w.queues.len(&endpoint("claude")),
            0,
            "and nothing was enqueued on the way to being refused"
        );
    }

    /// Trust outranks draining: an untrusted peer learns that it is
    /// untrusted, never what state this node is in.
    #[test]
    fn a_draining_node_still_tells_an_untrusted_peer_it_is_untrusted() {
        let mut w = World::new();
        w.draining = true;
        let outcome = w.admit(&frame(Some("claude"), b"hi", 41), &peer(P2), 0);
        assert_eq!(outcome, Outcome::Refused(Refusal::UntrustedPeer));
    }

    #[test]
    fn draining_is_shutting_down_on_the_wire() {
        assert_eq!(
            Refusal::Draining.to_wire(),
            DirectRejectReason::ShuttingDown,
            "not overloaded, and not a routing answer"
        );
    }

    #[test]
    fn an_untrusted_peer_is_refused_without_spending_ingress() {
        let mut w = World::new();
        let before = w.ingress.tracked_peers();
        let outcome = w.admit(&frame(Some("claude"), b"hi", 3), &peer(P2), 0);
        assert_eq!(outcome, Outcome::Refused(Refusal::UntrustedPeer));
        assert_eq!(
            w.ingress.tracked_peers(),
            before,
            "no bucket was created for a peer that was never admitted"
        );
        assert_eq!(
            w.queues.len(&endpoint("claude")),
            0,
            "and nothing delivered"
        );
    }

    /// RATE BEFORE DEDUP: ADR-0019 requires a rate-limited retry to leave
    /// a prior positive entry untouched.
    #[test]
    fn a_rate_limited_retry_does_not_erase_the_accepted_route() {
        let mut w = World::new();
        let f = frame(Some("claude"), b"hi", 4);
        assert!(matches!(
            w.admit(&f, &peer(P1), 0),
            Outcome::Accepted { .. }
        ));

        // Exhaust this peer's bucket.
        let mut refused = false;
        for _ in 0..1_000 {
            if matches!(
                w.admit(&f, &peer(P1), 0),
                Outcome::Refused(Refusal::RateLimited)
            ) {
                refused = true;
                break;
            }
        }
        assert!(refused, "the bucket does run out");

        // The entry survives, so a later retry still replays the route.
        let key = dedup_key(&f, &peer(P1));
        assert_eq!(
            w.dedup.get(&key).map(|r| r.resolved_endpoint.clone()),
            Some(endpoint("claude")),
            "a flood must not delete an accepted route"
        );
    }

    /// A retry with matching content replays the STORED route and does
    /// not deliver again — even after the default has changed.
    #[test]
    fn a_matching_retry_replays_the_stored_route_without_a_second_delivery() {
        let mut w = World::new();
        let f = frame(None, b"hi", 5);
        assert_eq!(
            w.admit(&f, &peer(P1), 0),
            Outcome::Accepted {
                resolved_endpoint: endpoint("human")
            }
        );
        assert_eq!(w.queues.len(&endpoint("human")), 1);

        // The default moves to `claude` after the first acceptance.
        w.registry.set_default(Some(endpoint("claude")));

        assert_eq!(
            w.admit(&f, &peer(P1), 1),
            Outcome::DuplicateAccepted {
                resolved_endpoint: endpoint("human")
            },
            "the stored route wins over the new default"
        );
        assert_eq!(
            w.queues.len(&endpoint("human")),
            1,
            "and it was not delivered twice"
        );
        assert_eq!(w.queues.len(&endpoint("claude")), 0);
    }

    /// Same key, different body: one identity cannot mean two messages.
    #[test]
    fn the_same_id_with_a_different_body_is_a_conflict() {
        let mut w = World::new();
        assert!(matches!(
            w.admit(&frame(Some("claude"), b"first", 6), &peer(P1), 0),
            Outcome::Accepted { .. }
        ));
        let outcome = w.admit(&frame(Some("claude"), b"second", 6), &peer(P1), 0);
        assert_eq!(outcome, Outcome::Refused(Refusal::DuplicateConflict));
        assert_eq!(
            w.queues.len(&endpoint("claude")),
            1,
            "the conflicting body was not delivered"
        );
        assert_eq!(
            outcome.refusal_wire(),
            Some(DirectRejectReason::Malformed),
            "and it is not distinguishable as a routing failure"
        );
    }

    /// A concurrent matching copy attaches rather than enqueuing again.
    /// The owner's reservation is still held, which is what a second
    /// arrival sees.
    #[test]
    fn a_concurrent_matching_copy_attaches_instead_of_enqueuing() {
        let mut w = World::new();
        let f = frame(Some("claude"), b"hi", 7);

        // Take the reservation as an owner would, and leave it held.
        let key = dedup_key(&f, &peer(P1));
        let fingerprint = direct_content_fingerprint_v1(Some("text/plain"), b"hi")
            .expect("a fingerprintable body");
        assert!(matches!(
            w.reservations.acquire(&key, fingerprint),
            Ok(Reservation::Owner)
        ));

        assert_eq!(
            w.admit(&f, &peer(P1), 0),
            Outcome::AttachedAsWaiter,
            "a matching copy shares the owner's outcome"
        );
        assert_eq!(
            w.queues.len(&endpoint("claude")),
            0,
            "and never creates a parallel enqueue path"
        );
    }

    /// THE ACCEPTANCE POINT. A full queue is `overloaded`, and nothing was
    /// accepted — the exit gate's scenario 9.
    #[test]
    fn the_receipt_time_is_taken_at_admission() {
        // `message-received.schema.json` requires `received_at` and
        // `contracts/ENDPOINTS.md` names it. It has to be captured when
        // the event is admitted rather than when a client drains: the
        // queue is bounded and an event may wait in it, so a drain-time
        // stamp would drift by however long the consumer was behind and
        // report congestion as lateness in the message.
        let mut w = World::new();
        let admitted_at = 1_700_000_000_000;
        assert!(matches!(
            w.admit(&frame(Some("claude"), b"hi", 7), &peer(P1), admitted_at),
            Outcome::Accepted { .. }
        ));

        let drained = w.queues.drain(&endpoint("claude"));
        assert_eq!(drained.len(), 1);
        assert_eq!(
            drained[0].received_at, admitted_at,
            "the moment it arrived, not the moment it was collected"
        );
    }

    /// And from the WALL clock, not the monotonic one.
    ///
    /// The two are separate arguments precisely so this is checkable:
    /// milliseconds-since-startup restarts near zero with the process,
    /// so a receipt time taken from it cannot be converted to a real
    /// instant and misorders against events from another lifetime.
    #[test]
    fn the_receipt_time_is_wall_clock_not_monotonic() {
        let mut w = World::new();
        // A node running for three seconds, on a machine whose clock
        // says 2023 — the shape of every real process.
        let clocks = Clocks {
            monotonic_ms: 3_000,
            wall_ms: 1_700_000_000_000,
        };
        let f = frame(Some("claude"), b"hi", 8);
        let mut ctx = AdmissionContext {
            trust: &w.trust,
            ingress: &mut w.ingress,
            dedup: &mut w.dedup,
            reservations: &mut w.reservations,
            registry: &w.registry,
            queues: &mut w.queues,
            draining: w.draining,
        };
        assert!(matches!(
            admit_inbound(&f, &peer(P1), clocks, &mut ctx),
            Outcome::Accepted { .. }
        ));

        let drained = w.queues.drain(&endpoint("claude"));
        assert_eq!(
            drained[0].received_at, 1_700_000_000_000,
            "the epoch instant, not the three seconds since startup"
        );
    }

    #[test]
    fn a_full_queue_refuses_rather_than_accepting_falsely() {
        let mut w = World::new();
        w.queues.close(&endpoint("claude"));
        w.queues.open(endpoint("claude"), 1);

        assert!(matches!(
            w.admit(&frame(Some("claude"), b"one", 8), &peer(P1), 0),
            Outcome::Accepted { .. }
        ));
        let outcome = w.admit(&frame(Some("claude"), b"two", 9), &peer(P1), 0);
        assert_eq!(
            outcome,
            Outcome::Refused(Refusal::Queue(QueueRefusal::Full { bound: 1 }))
        );
        assert_eq!(outcome.refusal_wire(), Some(DirectRejectReason::Overloaded));
        assert_eq!(w.queues.len(&endpoint("claude")), 1, "still exactly one");
    }

    /// A refused message leaves NO positive entry, so a retry can succeed
    /// once the route recovers rather than being refused from cache.
    #[test]
    fn a_refused_message_leaves_no_cache_entry_to_poison_its_retry() {
        let mut w = World::new();
        w.queues.close(&endpoint("claude"));
        w.queues.open(endpoint("claude"), 1);
        let f = frame(Some("claude"), b"hi", 10);

        w.queues
            .push(DirectEvent {
                source_peer: peer(P1),
                source_endpoint: endpoint("human"),
                destination_endpoint: endpoint("claude"),
                message_id: MessageId::from_bytes([99; 16]),
                payload: Payload::at_ceiling(None, b"filler".to_vec()).expect("fits"),
                received_at: 0,
            })
            .expect("the queue takes one");

        assert!(matches!(w.admit(&f, &peer(P1), 0), Outcome::Refused(_)));
        assert!(
            w.dedup.get(&dedup_key(&f, &peer(P1))).is_none(),
            "a refusal must not cache a route"
        );

        // The queue drains; the same message now succeeds.
        w.queues.drain(&endpoint("claude"));
        assert_eq!(
            w.admit(&f, &peer(P1), 1),
            Outcome::Accepted {
                resolved_endpoint: endpoint("claude")
            }
        );
    }

    /// A ROUTING refusal leaves no entry either. Separate from the
    /// queue-refusal test above because they exit through different
    /// branches, and a mutation that cached a route on only one of them
    /// survived the other's test — which is how this gap was found.
    #[test]
    fn a_routing_refusal_leaves_no_cache_entry_either() {
        let mut w = World::new();
        let f = frame(Some("nonexistent"), b"hi", 15);

        assert!(matches!(
            w.admit(&f, &peer(P1), 0),
            Outcome::Refused(Refusal::NoRoute(_))
        ));
        assert!(
            w.dedup.get(&dedup_key(&f, &peer(P1))).is_none(),
            "an unroutable message must not cache a route"
        );

        // The same message to an endpoint that DOES exist still works,
        // so the refusal poisoned nothing.
        let good = frame(Some("claude"), b"hi", 15);
        assert_eq!(
            w.admit(&good, &peer(P1), 1),
            Outcome::Accepted {
                resolved_endpoint: endpoint("claude")
            }
        );
    }

    /// Every refusal that is a ROUTING failure is one wire code, so a
    /// probing peer cannot enumerate endpoints or infer policy.
    #[test]
    fn every_routing_refusal_is_indistinguishable_on_the_wire() {
        let mut w = World::new();

        // Unknown endpoint.
        let unknown = w.admit(&frame(Some("nonexistent"), b"hi", 11), &peer(P1), 0);
        // Known, configured, but nothing holds its lease.
        w.registry.revoke(&endpoint("claude"));
        let offline = w.admit(&frame(Some("claude"), b"hi", 12), &peer(P1), 0);

        for outcome in [&unknown, &offline] {
            assert!(matches!(outcome, Outcome::Refused(Refusal::NoRoute(_))));
            assert_eq!(
                outcome.refusal_wire(),
                Some(DirectRejectReason::NoRoute),
                "endpoint presence must not be observable"
            );
        }
        assert_ne!(unknown, offline, "though they differ LOCALLY");
    }

    /// The reservation budget is returned on every path admission
    /// decides, or a peer's own retries would decay its allowance.
    #[test]
    fn every_decided_path_returns_its_reservation() {
        let mut w = World::new();
        assert!(matches!(
            w.admit(&frame(Some("claude"), b"hi", 13), &peer(P1), 0),
            Outcome::Accepted { .. }
        ));
        assert!(w.reservations.is_empty(), "accepted path released");

        assert!(matches!(
            w.admit(&frame(Some("nonexistent"), b"hi", 14), &peer(P1), 0),
            Outcome::Refused(Refusal::NoRoute(_))
        ));
        assert!(w.reservations.is_empty(), "refused path released too");
    }
}
