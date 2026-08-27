// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Bounded per-session broadcast delivery queues.
//!
//! The broadcast counterpart of [`crate::endpoint_queue`], and it differs
//! in the one way the two modes differ: a direct message resolves to
//! exactly one endpoint, while a broadcast fans out to every session that
//! joined the channel.
//!
//! # A full queue drops for that session and nobody else
//!
//! `resource-limits.md` is explicit that broadcast local delivery **may
//! drop** under overload while direct must refuse before accepting. That
//! asymmetry is not an inconsistency: `AcceptedV2` is a promise to one
//! sender that one queue took the message, so direct cannot accept what
//! it cannot hold. Broadcast promises nobody anything — PUBSUB.md makes
//! publish success mean local acceptance at the PUBLISHER and no more —
//! so a slow session drops its own copy rather than blocking the mesh or
//! its peers.
//!
//! What must not happen is that the drop is silent, so [`SessionQueues::
//! push`] answers which bound refused it.
//!
//! # Queues are opened by a join, never conjured
//!
//! For the same reason endpoint queues are: the key set must be chosen by
//! LOCAL state. A session id reaching this map comes from a local join,
//! never from a remote frame — a broadcast carries no session and no
//! endpoint at all (ADR-0030).

use std::collections::{BTreeMap, VecDeque};

use interweave_transport_api::{ChannelId, MessageId, Payload, TransportIdentity};

/// One broadcast message, normalized for local delivery.
///
/// `sent_at_ms` is deliberately absent, exactly as it is from
/// [`crate::endpoint_queue::DirectEvent`]: it is diagnostic on the wire
/// and would read as authoritative here. PUBSUB.md forbids it as an input
/// to authorization, ordering, freshness, replay or dedup, and the
/// simplest way to enforce that is for it never to leave the decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastEvent {
    /// The authenticated original publisher. The signature proved this.
    ///
    /// Not the peer that forwarded it: a mesh relay is a transport
    /// detail, and ADR-0029 makes trust a question about the ORIGINAL
    /// publisher.
    pub source_peer: TransportIdentity,
    /// The channel it arrived on.
    ///
    /// Derived from the topic, not read from the envelope — the envelope
    /// carries no channel, so a publisher cannot assert one that
    /// disagrees with where it published.
    pub channel: ChannelId,
    /// The publisher's application identity for this message.
    ///
    /// Never the mesh duplicate key. Two publishers may choose the same
    /// 128 bits (ADR-0004).
    pub message_id: MessageId,
    /// The application bytes and their advisory media type.
    pub payload: Payload,
    /// Unix-epoch milliseconds at which THIS node admitted the message.
    ///
    /// Required unconditionally by `MessageReceived` in
    /// `contracts/TRANSPORT.md` — the `?` markers there are on the
    /// endpoint and channel fields, not on this one — and stamped here
    /// rather than at drain, which may be arbitrarily later than receipt.
    ///
    /// Local, and distinct from the remote's `sent_at_ms`, which stays
    /// absent because it is diagnostic on the wire and would read as
    /// authoritative here. The two are different clocks owned by
    /// different parties, and omitting the peer's says nothing about
    /// omitting our own.
    ///
    /// Wall, never monotonic: a receipt time has to survive a restart and
    /// order against another process lifetime. Taking it from the
    /// monotonic clock made every direct event start near zero after a
    /// restart, which is why [`crate::direct_inbound::Clocks`] names the
    /// two separately.
    pub received_at: u64,
}

/// Why a session did not receive its copy.
///
/// Local only, and there is deliberately no wire mapping: a GossipSub
/// publisher receives no per-message answer, so a refusal here is a fact
/// about this node and never something a peer is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionDrop {
    /// The session's queue is at its bound.
    Full {
        /// The bound in force.
        bound: usize,
    },
    /// No queue is open for that session.
    NotOpen,
}

/// One session's bounded queue.
#[derive(Debug)]
struct Queue {
    events: VecDeque<BroadcastEvent>,
    bound: usize,
}

/// Every open broadcast delivery queue on this profile.
#[derive(Debug, Default)]
pub struct SessionQueues {
    queues: BTreeMap<String, Queue>,
}

impl SessionQueues {
    /// An empty set. Nothing is deliverable until a join opens a queue.
    #[must_use]
    pub fn new() -> Self {
        Self {
            queues: BTreeMap::new(),
        }
    }

    /// Open a queue for `session`, bounded at `bound`.
    ///
    /// Clamped to 1 rather than trusted: a zero bound reads like
    /// "unbounded" and behaves like "closed".
    ///
    /// Re-opening a session that already has one REPLACES it. A session
    /// re-establishing is a new consumer and must not inherit a previous
    /// one's undelivered messages.
    pub fn open(&mut self, session: impl Into<String>, bound: usize) {
        self.queues.insert(
            session.into(),
            Queue {
                events: VecDeque::new(),
                bound: bound.max(1),
            },
        );
    }

    /// Close `session`'s queue, discarding anything still in it.
    ///
    /// Returns how many events were dropped, so a caller can log a real
    /// number rather than assert a silent one.
    pub fn close(&mut self, session: &str) -> usize {
        self.queues
            .remove(session)
            .map_or(0, |queue| queue.events.len())
    }

    /// Whether a queue is open for `session`.
    #[must_use]
    pub fn is_open(&self, session: &str) -> bool {
        self.queues.contains_key(session)
    }

    /// How many events are waiting for `session`.
    #[must_use]
    pub fn len(&self, session: &str) -> usize {
        self.queues.get(session).map_or(0, |q| q.events.len())
    }

    /// Whether any queue is open.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queues.is_empty()
    }

    /// Admit one event for one session, or say why not.
    ///
    /// # Errors
    /// [`SessionDrop`] naming the bound that refused it. A caller fans
    /// out to several sessions and must treat each answer separately: one
    /// full queue is not a reason to withhold anyone else's copy.
    pub fn push(&mut self, session: &str, event: BroadcastEvent) -> Result<(), SessionDrop> {
        // NOT `entry().or_insert_with(..)`, for the reason the endpoint
        // queues give: a map that grew an entry per key asked for would
        // be unbounded by whatever arrives.
        let Some(queue) = self.queues.get_mut(session) else {
            return Err(SessionDrop::NotOpen);
        };
        if queue.events.len() >= queue.bound {
            return Err(SessionDrop::Full { bound: queue.bound });
        }
        queue.events.push_back(event);
        Ok(())
    }

    /// Take everything waiting for `session`, oldest first.
    ///
    /// Empty for a session with no open queue, which is the same answer
    /// as an open-but-idle one.
    pub fn drain(&mut self, session: &str) -> Vec<BroadcastEvent> {
        self.queues
            .get_mut(session)
            .map(|q| q.events.drain(..).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

    fn event(body: &[u8]) -> BroadcastEvent {
        BroadcastEvent {
            source_peer: TransportIdentity::parse(P1).expect("valid peer"),
            channel: ChannelId::parse("general").expect("valid channel"),
            message_id: MessageId::from_bytes([7; 16]),
            payload: Payload::at_ceiling(None, body.to_vec()).expect("within the ceiling"),
            received_at: 1_786_600_000_000,
        }
    }

    #[test]
    fn a_full_queue_drops_for_that_session_and_nobody_else() {
        // The fan-out property. A slow consumer must not cost a fast one
        // its copy, which is the whole reason these are per session.
        let mut q = SessionQueues::new();
        q.open("slow", 1);
        q.open("fast", 8);

        // Both sessions are offered both messages, which is what makes
        // the contrast meaningful: same fan-out, different outcomes.
        assert_eq!(q.push("slow", event(b"one")), Ok(()));
        assert_eq!(q.push("fast", event(b"one")), Ok(()));
        assert_eq!(
            q.push("slow", event(b"two")),
            Err(SessionDrop::Full { bound: 1 }),
            "the slow session is at its bound"
        );
        assert_eq!(
            q.push("fast", event(b"two")),
            Ok(()),
            "and the fast one is unaffected"
        );
        assert_eq!(q.len("slow"), 1);
        assert_eq!(q.len("fast"), 2);
    }

    #[test]
    fn a_session_with_no_queue_is_refused_rather_than_given_one() {
        let mut q = SessionQueues::new();
        assert_eq!(q.push("nobody", event(b"x")), Err(SessionDrop::NotOpen));
        assert!(
            q.is_empty(),
            "a refused push must not have created the queue it refused"
        );
    }

    #[test]
    fn closing_discards_the_backlog_and_reports_its_size() {
        let mut q = SessionQueues::new();
        q.open("s", 4);
        q.push("s", event(b"a")).expect("room");
        q.push("s", event(b"b")).expect("room");
        assert_eq!(q.close("s"), 2, "the caller learns what was lost");
        assert!(!q.is_open("s"));
        assert!(q.drain("s").is_empty());
    }

    #[test]
    fn reopening_a_session_does_not_inherit_the_previous_backlog() {
        // A reconnecting consumer is a new consumer. Inheriting would
        // deliver a message to something that never joined when it
        // arrived.
        let mut q = SessionQueues::new();
        q.open("s", 4);
        q.push("s", event(b"old")).expect("room");
        q.open("s", 4);
        assert_eq!(q.len("s"), 0);
    }

    #[test]
    fn a_zero_bound_is_clamped_rather_than_read_as_unbounded() {
        let mut q = SessionQueues::new();
        q.open("s", 0);
        assert_eq!(q.push("s", event(b"a")), Ok(()));
        assert_eq!(
            q.push("s", event(b"b")),
            Err(SessionDrop::Full { bound: 1 })
        );
    }

    #[test]
    fn draining_returns_oldest_first_and_empties_the_queue() {
        let mut q = SessionQueues::new();
        q.open("s", 4);
        q.push("s", event(b"first")).expect("room");
        q.push("s", event(b"second")).expect("room");
        let drained = q.drain("s");
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].payload.bytes(), b"first");
        assert_eq!(q.len("s"), 0, "draining empties it");
        assert!(q.is_open("s"), "but does not close it");
    }
}
