// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Inbound broadcast admission, in ADR-0029's order.
//!
//! ADR-0029 fixes the order and this module is split along it:
//!
//! ```text
//! decode/size guards -> cryptographic/source validation
//!   -> local source trust decision -> Reject|Ignore|Accept report
//!   -> dedup / resource / local delivery
//! ```
//!
//! [`classify_broadcast`] is everything up to and including the verdict;
//! [`admit_broadcast`] is everything after it. The split is where it is
//! because the REPORT must happen between them, and the backend is the
//! only layer that can make it.
//!
//! Cryptographic and source validation is absent from both, deliberately:
//! under strict validation the backend has already refused an unsigned
//! message, one with no source, and one with no sequence number, before
//! anything reaches here. A second check would be a check of a different
//! thing wearing the same name.
//!
//! # Why the verdict is not the delivery decision
//!
//! A validation verdict answers what the MESH is owed: was this message
//! structurally valid, and is its original publisher authorized here.
//! Everything after — a duplicate, a conflicting body, a full queue, a
//! spent rate bucket, nobody joined — is local, and none of it makes the
//! message invalid or its publisher untrusted. So those outcomes never
//! change the report, and PUBSUB.md says so for the conflict case
//! explicitly: `Accept`, then refuse local delivery.
//!
//! Reporting them would be actively wrong rather than merely
//! conservative. `Reject` penalises the peer that forwarded the message,
//! and a node that never saw the first body cannot see a conflict — so
//! one node's cache state would punish an honest relay for a message
//! every other node accepts.

use interweave_transport_api::{BroadcastMessageV1, ChannelId, TransportIdentity};
use interweave_trust_api::{PeerTrustPolicy, TrustDecision};

use crate::dedup::{Admission, DedupCache, DedupKey, RecordedRoute};
use crate::direct_inbound::{Clocks, PrefixContext, Refusal, admit_prefix};
use crate::fingerprint::direct_content_fingerprint_v1;
use crate::ingress::SubscriptionRegistry;
use crate::session_queue::{BroadcastEvent, SessionDrop, SessionQueues};

/// What the mesh is told about one inbound message.
///
/// The three ADR-0029 results and nothing else. There is no fourth for
/// "valid, authorized, and locally undeliverable": that is
/// [`BroadcastAdmission`]'s business, and giving it a verdict would tell
/// the mesh something untrue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolVerdict {
    /// Valid and authorized. Deliver it and let it propagate.
    Accept(Box<BroadcastMessageV1>),
    /// Structurally valid, but this node does not authorize the original
    /// publisher.
    ///
    /// Not delivered, not forwarded, and **the peer that forwarded it is
    /// not penalised** — it relayed something objectively fine, and
    /// allowlists differing between nodes is normal.
    Ignore,
    /// Objectively invalid protocol data.
    Reject,
}

/// What happened to an accepted message locally.
///
/// Every variant except `Delivered` means nothing was handed to a session.
/// None of them is reported to the mesh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BroadcastAdmission {
    /// Queued for these sessions, and dropped for those.
    ///
    /// `dropped` is carried rather than swallowed because
    /// `resource-limits.md` permits broadcast local delivery to drop
    /// under overload — and a permitted drop that nothing can count is
    /// indistinguishable from a message that never arrived.
    Delivered {
        /// Sessions whose queue took it.
        sessions: Vec<String>,
        /// Sessions whose queue refused it, and why.
        dropped: Vec<(String, SessionDrop)>,
    },
    /// Valid, authorized, and nobody local had joined.
    ///
    /// The profile may still hold the topic warm through
    /// `channels.desired`; PUBSUB.md forbids buffering it for a future
    /// join, so this stores nothing.
    NobodyJoined,
    /// A prefix gate refused it: draining, or over the ingress rate.
    ///
    /// Untrusted cannot appear here — [`classify_broadcast`] answered
    /// that already — but it is not special-cased away, because a gate
    /// that stopped agreeing with the classifier should surface rather
    /// than be silently mapped onto something else.
    Refused(Refusal),
    /// Already delivered inside the dedup TTL.
    Duplicate,
    /// One publisher, one channel, one id, two different bodies.
    Conflict,
    /// The content could not be fingerprinted, so it cannot be deduped.
    ///
    /// Only an invalid media type reaches this: the envelope decoded, so
    /// the frame itself is well formed.
    Unfingerprintable,
}

/// Decode and decide what the mesh is told.
///
/// `source` is the **authenticated original publisher**, not the peer
/// that forwarded the message. ADR-0029's trust question is about the
/// publisher, and answering it about the relay would let a trusted relay
/// launder an untrusted origin's traffic.
#[must_use]
pub fn classify_broadcast(
    raw: &[u8],
    limit: usize,
    source: &TransportIdentity,
    trust: &PeerTrustPolicy,
) -> ProtocolVerdict {
    // 1. DECODE AND SIZE, BEFORE TRUST. A malformed envelope is
    //    objectively invalid whoever sent it, and checking trust first
    //    would answer a trusted peer's broken frame with `Ignore` — which
    //    tells the mesh the message was fine and this node merely was not
    //    interested.
    let Ok(frame) = BroadcastMessageV1::decode(raw, limit) else {
        return ProtocolVerdict::Reject;
    };

    // 2. LOCAL TRUST, about the original publisher.
    match trust.decide(source) {
        TrustDecision::Denied(_) => ProtocolVerdict::Ignore,
        TrustDecision::Allowed => ProtocolVerdict::Accept(Box::new(frame)),
    }
}

/// Everything broadcast admission reads and writes, borrowed for one
/// decision.
///
/// A struct for the reason [`crate::direct_inbound::AdmissionContext`] is
/// one: the ORDER these are used in is this module's subject, and a
/// caller handed them individually could use them in some other order.
pub struct BroadcastContext<'a> {
    /// The gates that need no frame, shared with direct.
    pub prefix: PrefixContext<'a>,
    /// The duplicate cache, keyed by mode so the two cannot alias.
    pub dedup: &'a mut DedupCache,
    /// Local join references, which decide who receives.
    pub subs: &'a SubscriptionRegistry,
    /// Bounded per-session delivery queues.
    pub queues: &'a mut SessionQueues,
}

/// Everything after the verdict: rate, dedup, fan-out.
///
/// Called only for [`ProtocolVerdict::Accept`], and only after the report
/// has been made. Nothing here can change what the mesh was told.
///
/// `channel` comes from the topic the message arrived on, not from the
/// envelope — the envelope carries none, so a publisher cannot claim one
/// channel while publishing on another.
pub fn admit_broadcast(
    frame: &BroadcastMessageV1,
    channel: &ChannelId,
    source: &TransportIdentity,
    clocks: Clocks,
    ctx: &mut BroadcastContext<'_>,
) -> BroadcastAdmission {
    // 3. THE SHARED GATES, in the one order they are written in. Trust is
    //    re-answered here and passes trivially, which is the point: there
    //    is one implementation of these three and no broadcast-shaped
    //    copy that could drift from it.
    //
    //    WHAT THE RATE BUCKET BOUNDS, and what it cannot. Everything
    //    below it: the fingerprint hash over the payload, the dedup
    //    insertion, and the fan-out that copies the payload once per
    //    joined session. Not signature verification, which the backend
    //    performs before this function is reachable. And not the envelope
    //    decode in `classify_broadcast`, which is structural rather than
    //    an oversight — the mesh is owed a verdict, the verdict depends
    //    on whether the envelope decodes, so refusing to decode would
    //    mean answering without knowing whether the bytes were valid.
    //    Both of those are bounded per message by the transmit ceiling
    //    rather than per unit time. ADR-0026's amendment says the same in
    //    the same words, because a bound is worth exactly what its stated
    //    scope is.
    if let Err(refusal) = admit_prefix(source, clocks.monotonic_ms, &mut ctx.prefix) {
        return BroadcastAdmission::Refused(refusal);
    }

    // 4. CONTENT IDENTITY. The direct fingerprint by name and by
    //    definition: it is computed over media type and payload alone,
    //    never crosses the wire, and the dedup keys already differ by
    //    mode, so the two cannot alias.
    let Ok(fingerprint) = direct_content_fingerprint_v1(
        frame.payload.media_type().map(|m| m.as_str()),
        frame.payload.bytes(),
    ) else {
        return BroadcastAdmission::Unfingerprintable;
    };

    let key = DedupKey::Broadcast {
        source_peer: source.clone(),
        channel: channel.clone(),
        message_id: frame.message_id,
    };

    // 5. DEDUP. A duplicate is not re-delivered and a conflict is not
    //    delivered at all — and neither is reported to the mesh.
    match ctx.dedup.admit(&key, fingerprint, clocks.monotonic_ms) {
        Admission::DuplicateAccepted { .. } => return BroadcastAdmission::Duplicate,
        Admission::Conflict => return BroadcastAdmission::Conflict,
        Admission::Fresh => {}
    }

    // 6. FAN OUT. Only sessions that JOINED, never merely because the
    //    profile desires the channel: a warm mesh with no local consumer
    //    delivers to nobody and stores nothing.
    let subscribers = ctx.subs.subscribers(channel);
    if subscribers.is_empty() {
        // RECORDED ANYWAY. The message was admitted; it simply had no
        // local consumer. Not recording would let a second copy arrive
        // inside the TTL and be treated as fresh — and if a session
        // joined in between, that copy WOULD be delivered, which is the
        // replay PUBSUB.md forbids by another route.
        ctx.dedup.record_accepted(
            key,
            RecordedRoute::Broadcast,
            fingerprint,
            clocks.monotonic_ms,
        );
        return BroadcastAdmission::NobodyJoined;
    }

    let mut sessions = Vec::with_capacity(subscribers.len());
    let mut dropped = Vec::new();
    for session in subscribers {
        let event = BroadcastEvent {
            source_peer: source.clone(),
            channel: channel.clone(),
            message_id: frame.message_id,
            payload: frame.payload.clone(),
            // THE WALL CLOCK, and stamped once here so every session's
            // copy of one message reports the same receipt time.
            received_at: clocks.wall_ms,
        };
        match ctx.queues.push(&session, event) {
            Ok(()) => sessions.push(session),
            // ONE SESSION'S BOUND IS NOT ANOTHER'S. The loop continues,
            // because a slow consumer must not cost a fast one its copy.
            Err(drop) => dropped.push((session, drop)),
        }
    }

    // Recorded even when every session dropped it: the message was
    // admitted and a retry of the same id must not be treated as fresh
    // just because this node was busy when the first copy arrived.
    ctx.dedup.record_accepted(
        key,
        RecordedRoute::Broadcast,
        fingerprint,
        clocks.monotonic_ms,
    );
    BroadcastAdmission::Delivered { sessions, dropped }
}

#[cfg(test)]
mod tests {
    use super::*;

    use interweave_transport_api::{MediaType, MessageId, Payload};

    use crate::dedup::DEFAULT_TTL_MS;
    use crate::ingress::IngressLimiter;

    const P1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    /// A plausible Unix-epoch millisecond, so a monotonic value cannot
    /// pass for it in a test.
    const WALL_AT_RECEIPT: u64 = 1_786_600_000_000;
    const P2: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";

    fn peer(s: &str) -> TransportIdentity {
        TransportIdentity::parse(s).expect("valid peer id")
    }

    fn channel() -> ChannelId {
        ChannelId::parse("general").expect("valid channel")
    }

    fn frame(id: u8, body: &[u8]) -> BroadcastMessageV1 {
        BroadcastMessageV1 {
            message_id: MessageId::from_bytes([id; 16]),
            sent_at_ms: 1_786_600_000_000,
            payload: Payload::at_ceiling(
                Some(MediaType::parse("text/plain").expect("valid media type")),
                body.to_vec(),
            )
            .expect("within the ceiling"),
        }
    }

    /// Everything admission borrows, owned for one test.
    struct World {
        trust: PeerTrustPolicy,
        ingress: IngressLimiter,
        dedup: DedupCache,
        subs: SubscriptionRegistry,
        queues: SessionQueues,
        draining: bool,
    }

    impl World {
        fn new(joined: &[&str]) -> Self {
            let mut subs =
                SubscriptionRegistry::new(std::collections::BTreeSet::new()).expect("empty");
            let mut queues = SessionQueues::new();
            for s in joined {
                subs.join(channel(), (*s).to_owned())
                    .expect("within the bounds");
                queues.open(*s, 8);
            }
            Self {
                trust: PeerTrustPolicy::new([peer(P1)]).expect("a small allowlist"),
                ingress: IngressLimiter::with_defaults(0),
                dedup: DedupCache::new(64, DEFAULT_TTL_MS),
                subs,
                queues,
                draining: false,
            }
        }

        fn admit(&mut self, f: &BroadcastMessageV1, from: &str, now: u64) -> BroadcastAdmission {
            let clocks = Clocks {
                monotonic_ms: now,
                wall_ms: WALL_AT_RECEIPT + now,
            };
            let mut ctx = BroadcastContext {
                prefix: PrefixContext {
                    trust: &self.trust,
                    ingress: &mut self.ingress,
                    draining: self.draining,
                },
                dedup: &mut self.dedup,
                subs: &self.subs,
                queues: &mut self.queues,
            };
            admit_broadcast(f, &channel(), &peer(from), clocks, &mut ctx)
        }
    }

    #[test]
    fn a_malformed_envelope_is_reject_whoever_sent_it() {
        // ADR-0029's ORDER, and the UNTRUSTED case is the one that pins
        // it. A trusted publisher's broken frame answers `Reject` under
        // either ordering, so it distinguishes nothing; only an
        // unallowlisted publisher separates them — decode-first says
        // `Reject` (the bytes are objectively broken), trust-first says
        // `Ignore` (telling the mesh the bytes were fine and this node
        // merely was not interested).
        //
        // An earlier version of this test used the trusted publisher and
        // asserted it caught the short-circuit. It did not: the mutation
        // passed.
        let trust = PeerTrustPolicy::new([peer(P1)]).expect("allowlist");
        let broken = b"\x01not a frame";

        assert_eq!(
            classify_broadcast(broken, 49_152, &peer(P2), &trust),
            ProtocolVerdict::Reject,
            "objective invalidity outranks a trust question"
        );
        assert_eq!(
            classify_broadcast(broken, 49_152, &peer(P1), &trust),
            ProtocolVerdict::Reject,
            "and a trusted publisher's broken frame is no different"
        );
    }

    #[test]
    fn an_unauthorized_publisher_is_ignore_and_a_valid_one_is_accept() {
        let trust = PeerTrustPolicy::new([peer(P1)]).expect("allowlist");
        let bytes = frame(1, b"hello").encode();

        assert_eq!(
            classify_broadcast(&bytes, 49_152, &peer(P2), &trust),
            ProtocolVerdict::Ignore,
            "a valid message from an unallowlisted publisher is not invalid"
        );
        assert!(matches!(
            classify_broadcast(&bytes, 49_152, &peer(P1), &trust),
            ProtocolVerdict::Accept(_)
        ));
    }

    #[test]
    fn two_publishers_reusing_one_envelope_id_both_reach_their_sessions() {
        // The required Stage 7 property, provable with no network: the
        // dedup key carries the publisher, so one publisher cannot
        // suppress another by choosing its id.
        let mut w = World::new(&["human"]);
        w.trust = PeerTrustPolicy::new([peer(P1), peer(P2)]).expect("both");

        let f = frame(9, b"from one");
        let g = frame(9, b"from two");
        assert!(matches!(
            w.admit(&f, P1, 0),
            BroadcastAdmission::Delivered { .. }
        ));
        assert!(
            matches!(w.admit(&g, P2, 1), BroadcastAdmission::Delivered { .. }),
            "a second publisher's message is not a duplicate of the first"
        );
        assert_eq!(w.queues.drain("human").len(), 2, "both were delivered");
    }

    #[test]
    fn a_matching_duplicate_is_not_delivered_twice() {
        let mut w = World::new(&["human"]);
        let f = frame(3, b"once");
        assert!(matches!(
            w.admit(&f, P1, 0),
            BroadcastAdmission::Delivered { .. }
        ));
        assert_eq!(w.admit(&f, P1, 10), BroadcastAdmission::Duplicate);
        assert_eq!(w.queues.drain("human").len(), 1);
    }

    #[test]
    fn one_id_with_two_bodies_delivers_neither_the_second_nor_a_verdict() {
        // PUBSUB.md: the mesh was already told `Accept`, and this is the
        // local half. The first body stays delivered; the second is not.
        let mut w = World::new(&["human"]);
        assert!(matches!(
            w.admit(&frame(4, b"first"), P1, 0),
            BroadcastAdmission::Delivered { .. }
        ));
        assert_eq!(
            w.admit(&frame(4, b"second"), P1, 1),
            BroadcastAdmission::Conflict
        );

        let held = w.queues.drain("human");
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].payload.bytes(), b"first");
    }

    #[test]
    fn a_channel_nobody_joined_delivers_nothing_and_a_later_join_replays_nothing() {
        // `channels.desired` may hold the mesh warm, and PUBSUB.md
        // forbids buffering for a future consumer. So the message is
        // dropped at dispatch, and joining afterwards does not surface it.
        let mut w = World::new(&[]);
        assert_eq!(
            w.admit(&frame(5, b"unheard"), P1, 0),
            BroadcastAdmission::NobodyJoined
        );

        w.subs.join(channel(), "late".to_owned()).expect("joins");
        w.queues.open("late", 8);
        assert!(
            w.queues.drain("late").is_empty(),
            "a join must not replay what arrived before it"
        );

        // And the same message arriving again is a duplicate, not a
        // second chance — the entry was recorded even with no consumer.
        assert_eq!(
            w.admit(&frame(5, b"unheard"), P1, 1),
            BroadcastAdmission::Duplicate
        );
        assert!(w.queues.drain("late").is_empty());
    }

    #[test]
    fn delivery_reaches_only_the_sessions_that_joined() {
        let mut w = World::new(&["human"]);
        w.queues.open("claude", 8); // has a queue, but never joined
        assert!(matches!(
            w.admit(&frame(6, b"hi"), P1, 0),
            BroadcastAdmission::Delivered { .. }
        ));
        assert_eq!(w.queues.drain("human").len(), 1);
        assert!(
            w.queues.drain("claude").is_empty(),
            "a queue is not a subscription"
        );
    }

    #[test]
    fn a_full_session_queue_is_reported_rather_than_silently_dropped() {
        let mut w = World::new(&["fast"]);
        w.subs.join(channel(), "slow".to_owned()).expect("joins");
        w.queues.open("slow", 1);

        assert!(matches!(
            w.admit(&frame(7, b"one"), P1, 0),
            BroadcastAdmission::Delivered { .. }
        ));
        match w.admit(&frame(8, b"two"), P1, 1) {
            BroadcastAdmission::Delivered { sessions, dropped } => {
                assert_eq!(sessions, vec!["fast".to_owned()]);
                assert_eq!(
                    dropped,
                    vec![("slow".to_owned(), SessionDrop::Full { bound: 1 })]
                );
            }
            other => panic!("expected a partial delivery, got {other:?}"),
        }
    }

    #[test]
    fn a_draining_node_admits_no_broadcast() {
        let mut w = World::new(&["human"]);
        w.draining = true;
        assert_eq!(
            w.admit(&frame(2, b"late"), P1, 0),
            BroadcastAdmission::Refused(Refusal::Draining)
        );
        assert!(w.queues.drain("human").is_empty());
    }

    #[test]
    fn sent_at_ms_changes_nothing_about_admission() {
        // PUBSUB.md: diagnostic only, and never an input to
        // authorization, ordering, freshness, replay or dedup. Two frames
        // identical but for the timestamp must admit identically — and
        // the second must be a DUPLICATE, which it cannot be if the
        // timestamp reached the fingerprint or the key.
        let mut w = World::new(&["human"]);
        let mut early = frame(11, b"body");
        early.sent_at_ms = 0;
        let mut late = frame(11, b"body");
        late.sent_at_ms = u64::MAX;

        assert!(matches!(
            w.admit(&early, P1, 0),
            BroadcastAdmission::Delivered { .. }
        ));
        assert_eq!(
            w.admit(&late, P1, 1),
            BroadcastAdmission::Duplicate,
            "a different timestamp does not make it a different message"
        );
        assert_eq!(w.queues.drain("human").len(), 1);
    }

    #[test]
    fn every_delivered_copy_carries_the_wall_clock_receipt_time() {
        // `MessageReceived` in contracts/TRANSPORT.md requires
        // `received_at` unconditionally — the `?` markers there are on the
        // endpoint and channel fields, not on this one. Stamping it at
        // DRAIN instead would report a time arbitrarily later than
        // receipt, and omitting it would leave an adapter unable to
        // populate a required field at all.
        let mut w = World::new(&["human", "claude"]);
        w.subs.join(channel(), "claude".to_owned()).expect("joins");

        assert!(matches!(
            w.admit(&frame(13, b"body"), P1, 5),
            BroadcastAdmission::Delivered { .. }
        ));

        let expected = WALL_AT_RECEIPT + 5;
        for session in ["human", "claude"] {
            let held = w.queues.drain(session);
            assert_eq!(held.len(), 1, "{session} received it");
            assert_eq!(
                held[0].received_at, expected,
                "{session} must see the wall clock, not the monotonic one"
            );
        }
    }

    #[test]
    fn the_receipt_time_is_the_wall_clock_and_not_the_monotonic_one() {
        // The two are separated by `Clocks` precisely because this has
        // been got wrong once already: taking `received_at` from the
        // monotonic clock made every direct event start near zero after a
        // restart. A monotonic millisecond is a small number; a receipt
        // time is a Unix epoch.
        let mut w = World::new(&["human"]);
        assert!(matches!(
            w.admit(&frame(14, b"body"), P1, 7),
            BroadcastAdmission::Delivered { .. }
        ));
        let held = w.queues.drain("human");
        assert!(
            held[0].received_at > 1_600_000_000_000,
            "a monotonic value would be far too small to be a Unix epoch: {}",
            held[0].received_at
        );
        assert_ne!(held[0].received_at, 7, "and it is not the monotonic input");
    }

    #[test]
    fn the_delivered_event_carries_the_publisher_and_the_topics_channel() {
        let mut w = World::new(&["human"]);
        assert!(matches!(
            w.admit(&frame(12, b"body"), P1, 0),
            BroadcastAdmission::Delivered { .. }
        ));
        let held = w.queues.drain("human");
        assert_eq!(held[0].source_peer, peer(P1));
        assert_eq!(held[0].channel, channel());
        assert_eq!(held[0].message_id, MessageId::from_bytes([12; 16]));
    }
}
