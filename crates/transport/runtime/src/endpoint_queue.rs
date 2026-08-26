// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Bounded per-endpoint delivery queues.
//!
//! # What `AcceptedV2` means, expressed as a data structure
//!
//! ADR-0018 and `DIRECT.md` both say it: `AcceptedV2` is sent **only
//! after** the resolved endpoint's bounded local queue accepted the
//! normalized event. Not after the frame parsed, not after routing chose
//! an endpoint, and never as a promise that a human or Claude processed
//! anything. So acceptance has to have somewhere to happen, and this is
//! it — a caller that cannot [`push`](EndpointQueues::push) has not
//! accepted, and must not answer as though it had.
//!
//! # Why a queue is opened, never conjured
//!
//! [`EndpointQueues::push`] refuses an endpoint that has no open queue
//! rather than creating one on demand. That is a resource bound, not
//! tidiness: the destination endpoint is chosen by a REMOTE peer, so a
//! map that grew an entry per name asked for would be an unbounded
//! structure keyed by whatever arrives — the exact shape §6 forbids.
//! Queues are opened when a local session claims a lease and closed when
//! it goes, which bounds the key set by local configuration.
//!
//! # An offline endpoint has no backlog
//!
//! Closing drops the queue and everything in it. `testing.md` requires
//! that an offline human endpoint holds no daemon-side backlog, and
//! ADR-0044 puts durable retention in the human application rather than
//! in transport. A queue that survived its lease would be exactly the
//! transport-side store neither document permits.

use std::collections::{BTreeMap, VecDeque};

use interweave_transport_api::{
    DirectRejectReason, EndpointId, MessageId, Payload, TransportIdentity,
};

/// One directed message, normalized for local delivery.
///
/// Everything a local client needs and nothing it must not trust. The
/// remote's `sent_at_ms` is deliberately absent: it is diagnostic on the
/// wire and would read as authoritative here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectEvent {
    /// The authenticated remote peer. Noise proved this.
    pub source_peer: TransportIdentity,
    /// The endpoint the remote says produced this.
    ///
    /// **Peer-asserted.** The sender derives it from its own lease, but a
    /// receiver cannot verify that, so it is metadata and never
    /// authorization (ADR-0030).
    pub source_endpoint: EndpointId,
    /// The local endpoint this resolved to.
    ///
    /// Resolved here, not requested: an omitted destination has already
    /// become the configured default by the time an event exists.
    pub destination_endpoint: EndpointId,
    /// The sender's idempotency key.
    pub message_id: MessageId,
    /// The application bytes and their advisory media type.
    pub payload: Payload,
    /// LOCAL receipt time in milliseconds, taken at admission.
    ///
    /// Required by `message-received.schema.json` and named by
    /// `contracts/ENDPOINTS.md`, and it has to be captured HERE rather
    /// than when a client drains: the queue is bounded and an event may
    /// wait in it, so a drain-time stamp would drift by however long the
    /// consumer was behind — reporting congestion as lateness in the
    /// message.
    ///
    /// Local, and distinct from the remote's `sent_at_ms`, which stays
    /// absent because it is diagnostic on the wire and would read as
    /// authoritative here. Excluded from the content fingerprint for the
    /// same reason `sent_at_ms` is: a retry of the same message arrives
    /// at a different moment and is still the same message.
    pub received_at: u64,
}

/// Why a local delivery was not admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueRefusal {
    /// The endpoint's queue is at its bound.
    ///
    /// A resource answer, and the honest one: the endpoint exists, holds
    /// a lease, and is simply not draining fast enough.
    Full {
        /// The bound in force.
        bound: usize,
    },
    /// No queue is open for that endpoint.
    ///
    /// Nothing holds its lease, or it was never opened. Indistinguishable
    /// from every other routing failure on the wire, for the reason
    /// ADR-0030 gives: telling the two apart would let a probing peer
    /// learn which endpoints exist and which are currently attended.
    NotOpen,
}

impl QueueRefusal {
    /// The coarse code a peer may receive.
    ///
    /// The two differ, and the difference is deliberate. `Full` is
    /// `overloaded` — a true resource answer that an honest sender may
    /// retry. `NotOpen` collapses into `no_route` with endpoint unknown,
    /// disabled, unleased and policy-denied, because distinguishing it
    /// would make this protocol an endpoint-presence oracle.
    #[must_use]
    pub const fn to_wire(self) -> DirectRejectReason {
        match self {
            Self::Full { .. } => DirectRejectReason::Overloaded,
            Self::NotOpen => DirectRejectReason::NoRoute,
        }
    }
}

impl core::fmt::Display for QueueRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Full { bound } => write!(f, "the endpoint queue is at its bound of {bound}"),
            Self::NotOpen => write!(f, "no queue is open for that endpoint"),
        }
    }
}

/// One endpoint's bounded FIFO.
#[derive(Debug)]
struct Queue {
    events: VecDeque<DirectEvent>,
    bound: usize,
}

/// Every open endpoint queue on this profile.
///
/// Keyed by [`EndpointId`], and the key set is chosen by LOCAL
/// configuration — see the module docs for why that matters.
#[derive(Debug, Default)]
pub struct EndpointQueues {
    queues: BTreeMap<EndpointId, Queue>,
}

impl EndpointQueues {
    /// An empty set. Nothing is deliverable until a lease opens a queue.
    #[must_use]
    pub fn new() -> Self {
        Self {
            queues: BTreeMap::new(),
        }
    }

    /// Open a queue for `endpoint`, bounded at `bound`.
    ///
    /// Called when a local session claims the endpoint's lease. `bound`
    /// is the session's own `event_queue`, which `LocalDataSession`
    /// already refuses to construct as zero — a zero bound reads like
    /// "unbounded" and behaves like "closed". It is clamped to 1 here
    /// anyway rather than trusted, because this type must not depend on
    /// a caller having validated it.
    ///
    /// Re-opening an endpoint that already has a queue REPLACES it and
    /// discards what it held. That is the lease changing hands, and the
    /// new holder must not inherit the previous session's undelivered
    /// messages.
    pub fn open(&mut self, endpoint: EndpointId, bound: usize) {
        self.queues.insert(
            endpoint,
            Queue {
                events: VecDeque::new(),
                bound: bound.max(1),
            },
        );
    }

    /// Close `endpoint`'s queue, discarding anything still in it.
    ///
    /// Returns how many events were dropped, so a caller can log a real
    /// number rather than assert a silent one.
    pub fn close(&mut self, endpoint: &EndpointId) -> usize {
        self.queues
            .remove(endpoint)
            .map_or(0, |queue| queue.events.len())
    }

    /// Whether a queue is open for `endpoint`.
    #[must_use]
    pub fn is_open(&self, endpoint: &EndpointId) -> bool {
        self.queues.contains_key(endpoint)
    }

    /// How many events are waiting for `endpoint`.
    #[must_use]
    pub fn len(&self, endpoint: &EndpointId) -> usize {
        self.queues.get(endpoint).map_or(0, |q| q.events.len())
    }

    /// Whether any queue is open.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queues.is_empty()
    }

    /// Admit one event, or say why not.
    ///
    /// THE ACCEPTANCE POINT. A caller answers `AcceptedV2` if and only if
    /// this returned `Ok`.
    ///
    /// # Errors
    /// [`QueueRefusal::NotOpen`] when nothing holds the endpoint's lease,
    /// [`QueueRefusal::Full`] when it is at its bound. Both have coarse
    /// wire codes via [`QueueRefusal::to_wire`], and they are not the
    /// same code.
    pub fn push(&mut self, event: DirectEvent) -> Result<(), QueueRefusal> {
        // NOT `entry().or_insert_with(..)`. The endpoint here came from a
        // remote frame, and creating a queue for it would let a peer grow
        // this map by naming endpoints that do not exist.
        let Some(queue) = self.queues.get_mut(&event.destination_endpoint) else {
            return Err(QueueRefusal::NotOpen);
        };
        if queue.events.len() >= queue.bound {
            return Err(QueueRefusal::Full { bound: queue.bound });
        }
        queue.events.push_back(event);
        Ok(())
    }

    /// Take everything waiting for `endpoint`, oldest first.
    ///
    /// Empty for an endpoint with no open queue, which is the same answer
    /// as an open-but-idle one. A drainer learns nothing about presence
    /// it did not already have — it holds the lease.
    pub fn drain(&mut self, endpoint: &EndpointId) -> Vec<DirectEvent> {
        self.queues
            .get_mut(endpoint)
            .map(|q| q.events.drain(..).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P1: &str = "12D3KooWA9hFCGwGCpCbWWfLmYSpqPzXgLmPvbBrgWGNvNGSDVpS";

    fn peer() -> TransportIdentity {
        TransportIdentity::parse(P1).expect("valid peer id")
    }

    fn endpoint(name: &str) -> EndpointId {
        EndpointId::parse(name).expect("valid endpoint id")
    }

    fn event(destination: &str, body: &[u8]) -> DirectEvent {
        DirectEvent {
            source_peer: peer(),
            source_endpoint: endpoint("human"),
            destination_endpoint: endpoint(destination),
            message_id: MessageId::from_bytes([7; 16]),
            payload: Payload::at_ceiling(None, body.to_vec()).expect("within the ceiling"),
            received_at: 0,
        }
    }

    #[test]
    fn an_open_queue_admits_up_to_its_bound_and_then_refuses() {
        let mut queues = EndpointQueues::new();
        queues.open(endpoint("claude"), 2);

        assert!(queues.push(event("claude", b"one")).is_ok());
        assert!(queues.push(event("claude", b"two")).is_ok());
        assert_eq!(
            queues.push(event("claude", b"three")),
            Err(QueueRefusal::Full { bound: 2 }),
            "the third is refused, not dropped silently"
        );
        assert_eq!(
            queues.len(&endpoint("claude")),
            2,
            "and nothing was evicted"
        );
    }

    /// The exit-gate distinction: a full queue is `overloaded`, and an
    /// absent one is `no_route` with every other routing failure.
    #[test]
    fn a_full_queue_and_an_absent_one_answer_differently_on_the_wire() {
        assert_eq!(
            QueueRefusal::Full { bound: 1 }.to_wire(),
            DirectRejectReason::Overloaded
        );
        assert_eq!(
            QueueRefusal::NotOpen.to_wire(),
            DirectRejectReason::NoRoute,
            "presence must not be observable"
        );
    }

    /// A REMOTE peer chooses this key. Creating a queue on demand would
    /// make the map unbounded in whatever names arrive.
    #[test]
    fn pushing_to_an_endpoint_with_no_queue_creates_nothing() {
        let mut queues = EndpointQueues::new();
        assert_eq!(
            queues.push(event("not-leased", b"hi")),
            Err(QueueRefusal::NotOpen)
        );
        assert!(
            queues.is_empty(),
            "a refused push must not have grown the map"
        );
        assert!(!queues.is_open(&endpoint("not-leased")));
    }

    /// An offline endpoint has no daemon-side backlog: closing discards.
    #[test]
    fn closing_an_endpoint_discards_its_backlog() {
        let mut queues = EndpointQueues::new();
        queues.open(endpoint("human"), 8);
        queues.push(event("human", b"one")).expect("admitted");
        queues.push(event("human", b"two")).expect("admitted");

        assert_eq!(
            queues.close(&endpoint("human")),
            2,
            "it says what it dropped"
        );
        assert!(!queues.is_open(&endpoint("human")));

        // ...and re-opening starts empty rather than resurrecting them.
        queues.open(endpoint("human"), 8);
        assert_eq!(queues.len(&endpoint("human")), 0);
        assert!(queues.drain(&endpoint("human")).is_empty());
    }

    /// The lease changing hands must not hand over undelivered messages.
    #[test]
    fn reopening_an_endpoint_does_not_inherit_the_previous_holders_events() {
        let mut queues = EndpointQueues::new();
        queues.open(endpoint("human"), 8);
        queues
            .push(event("human", b"for the old session"))
            .expect("admitted");

        queues.open(endpoint("human"), 8);
        assert_eq!(
            queues.len(&endpoint("human")),
            0,
            "a new lease starts with an empty queue"
        );
    }

    #[test]
    fn events_drain_oldest_first() {
        let mut queues = EndpointQueues::new();
        queues.open(endpoint("claude"), 4);
        for body in [b"one".as_slice(), b"two", b"three"] {
            queues.push(event("claude", body)).expect("admitted");
        }
        let drained = queues.drain(&endpoint("claude"));
        let bodies: Vec<&[u8]> = drained.iter().map(|e| e.payload.bytes()).collect();
        assert_eq!(bodies, vec![b"one".as_slice(), b"two", b"three"]);
        assert_eq!(queues.len(&endpoint("claude")), 0, "draining empties it");
    }

    /// Each endpoint has its own bound. One noisy destination must not
    /// consume another's capacity.
    #[test]
    fn one_endpoints_bound_does_not_spend_anothers() {
        let mut queues = EndpointQueues::new();
        queues.open(endpoint("human"), 1);
        queues.open(endpoint("claude"), 1);

        queues.push(event("human", b"hi")).expect("admitted");
        assert!(
            queues.push(event("human", b"again")).is_err(),
            "human is full"
        );
        assert!(
            queues.push(event("claude", b"hi")).is_ok(),
            "claude's own bound is untouched"
        );
    }

    /// A zero bound reads like "unbounded" and behaves like "closed".
    /// `LocalDataSession` refuses to construct one, and this type does not
    /// depend on that having happened.
    #[test]
    fn a_zero_bound_is_clamped_rather_than_trusted() {
        let mut queues = EndpointQueues::new();
        queues.open(endpoint("human"), 0);
        assert!(
            queues.push(event("human", b"hi")).is_ok(),
            "a clamped queue admits one, rather than refusing everything"
        );
    }

    /// Draining an endpoint that has no queue is empty, not a panic, and
    /// says nothing a lease holder did not already know.
    #[test]
    fn draining_an_unopened_endpoint_is_empty() {
        let mut queues = EndpointQueues::new();
        assert!(queues.drain(&endpoint("nobody")).is_empty());
        assert_eq!(queues.len(&endpoint("nobody")), 0);
    }
}
