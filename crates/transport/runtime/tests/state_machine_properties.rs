// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! Properties of the pure state machines, over generated operations.
//!
//! # Why this suite exists
//!
//! Stage 6 took thirteen review rounds. About a third of the findings
//! were caused by the PREVIOUS round's fix, and they share a shape: a
//! bounded thing is changed under review pressure, the change satisfies
//! the reported case, and it breaks a regime nobody enumerated. Outbox
//! capacity was found wrong five separate times.
//!
//! Every one of the 526 unit tests in this workspace picks its inputs by
//! hand. That is exactly the wrong instrument for the question "does
//! this hold for every sequence", because the author picks the sequences
//! they were already thinking about — which is the same set that
//! produced the bug.
//!
//! So these tests do not assert what happens for chosen inputs. They
//! assert what must be true after ANY sequence of operations, and let
//! the generator find the regime. Each property below is a sentence the
//! implementation already claims somewhere in a comment; the difference
//! is that breaking it now fails a test.
//!
//! # Reading a failure
//!
//! proptest shrinks a failing case to a minimal one and writes it to
//! `proptest-regressions/` beside this file. That file is committed on
//! purpose: a shrunk counterexample is the cheapest possible regression
//! test, and it re-runs first on every subsequent invocation.

#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use interweave_transport_api::{EndpointId, MessageId, Payload, TransportIdentity};
use interweave_transport_runtime::{
    ContentFingerprint, DedupKey, DestinationSelector, DirectEvent, EndpointQueues, QueueRefusal,
    Reservation, ReservationFailure, ReservationMap, direct_content_fingerprint_v1,
};
use proptest::prelude::*;

// ---------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------

const PEERS: [&str; 3] = [
    "12D3KooWA9hSAWSVFhLuxgQqCrJmHBTNiSHfDaFEjJnkVLA8LCLd",
    "12D3KooWEyoppNCUx8Yx66oV9fJnriXwCcXwDDUA2kj6vnc6iDEp",
    "12D3KooWJRdGUnhLBcJRhcnVYYCCLQnBTUBJPFdBRLHtvNzWzYDb",
];

fn peer(i: usize) -> TransportIdentity {
    TransportIdentity::parse(PEERS[i % PEERS.len()]).expect("a valid peer id")
}

fn endpoint(name: &str) -> EndpointId {
    EndpointId::parse(name).expect("a valid endpoint id")
}

fn event(destination: &EndpointId, id: u8, body: &[u8]) -> DirectEvent {
    DirectEvent {
        source_peer: peer(0),
        source_endpoint: endpoint("human"),
        destination_endpoint: destination.clone(),
        message_id: MessageId::from_bytes([id; 16]),
        payload: Payload::at_ceiling(None, body.to_vec()).expect("within the ceiling"),
        received_at: 0,
    }
}

fn fingerprint(body: &[u8]) -> ContentFingerprint {
    direct_content_fingerprint_v1(Some("text/plain"), body).expect("a valid fingerprint")
}

fn dedup_key(source: usize, id: u8) -> DedupKey {
    DedupKey::Direct {
        source_peer: peer(source),
        source_endpoint: endpoint("claude"),
        destination_selector: DestinationSelector::Default,
        message_id: MessageId::from_bytes([id; 16]),
    }
}

/// A small alphabet on purpose. Collisions are the interesting case —
/// two operations landing on the same key or endpoint — and a wide
/// generator would make them vanishingly rare.
fn endpoints() -> impl Strategy<Value = EndpointId> {
    prop_oneof![
        Just(endpoint("human")),
        Just(endpoint("claude")),
        Just(endpoint("agent-1")),
    ]
}

// ---------------------------------------------------------------------
// EndpointQueues — the bound that was wrong five times
// ---------------------------------------------------------------------

proptest! {
    /// A queue never holds more than its bound.
    ///
    /// `EndpointQueues::open` clamps the bound to at least 1, so the
    /// property is stated against the clamped value rather than the
    /// requested one — asserting against the request would be asserting
    /// against a reading of the API instead of its behaviour.
    #[test]
    fn a_queue_never_exceeds_its_bound(
        bound in 0usize..6,
        pushes in prop::collection::vec(0u8..4, 0..24),
    ) {
        let claude = endpoint("claude");
        let mut queues = EndpointQueues::new();
        queues.open(claude.clone(), bound);
        let effective = bound.max(1);

        for id in pushes {
            let _ = queues.push(event(&claude, id, b"x"));
            prop_assert!(
                queues.len(&claude) <= effective,
                "held {} with a bound of {effective}",
                queues.len(&claude)
            );
        }
    }

    /// A refusal is never destructive.
    ///
    /// The failure this guards is the one that keeps recurring: a change
    /// to the capacity check that makes room by dropping something
    /// already accepted. `Accepted` means admitted to the queue, so an
    /// admitted event may not disappear because a later one arrived.
    #[test]
    fn a_refused_push_never_disturbs_what_was_accepted(
        bound in 1usize..5,
        pushes in prop::collection::vec(0u8..4, 0..24),
    ) {
        let claude = endpoint("claude");
        let mut queues = EndpointQueues::new();
        queues.open(claude.clone(), bound);

        for id in pushes {
            let before = queues.len(&claude);
            let outcome = queues.push(event(&claude, id, b"x"));
            let after = queues.len(&claude);
            match outcome {
                Ok(()) => prop_assert_eq!(after, before + 1, "an accepted push adds exactly one"),
                Err(QueueRefusal::Full { .. }) => {
                    prop_assert_eq!(after, before, "a refusal changed the queue");
                }
                Err(QueueRefusal::NotOpen) => prop_assert!(false, "the queue is open"),
            }
        }
    }

    /// Everything accepted comes back out, in arrival order, exactly once.
    #[test]
    fn drain_returns_every_accepted_event_in_order(
        bound in 1usize..6,
        pushes in prop::collection::vec(0u8..8, 0..24),
    ) {
        let claude = endpoint("claude");
        let mut queues = EndpointQueues::new();
        queues.open(claude.clone(), bound);

        let mut accepted = Vec::new();
        for id in pushes {
            if queues.push(event(&claude, id, b"x")).is_ok() {
                accepted.push(id);
            }
        }

        let drained: Vec<u8> = queues
            .drain(&claude)
            .iter()
            .map(|e| e.message_id.as_bytes()[0])
            .collect();
        prop_assert_eq!(drained, accepted, "drain did not return arrivals in order");
        prop_assert_eq!(queues.len(&claude), 0, "drain left something behind");
    }

    /// One endpoint's traffic never disturbs another's.
    ///
    /// This is ADR-0030 Model B stated as an invariant rather than as a
    /// scenario: a queue is per endpoint, so filling one must not consume
    /// or evict another's capacity.
    #[test]
    fn an_endpoints_queue_is_untouched_by_traffic_to_another(
        bound in 1usize..4,
        ops in prop::collection::vec((endpoints(), 0u8..4), 0..30),
    ) {
        let all = [endpoint("human"), endpoint("claude"), endpoint("agent-1")];
        let mut queues = EndpointQueues::new();
        for e in &all {
            queues.open(e.clone(), bound);
        }

        // CONTENTS, not depths. Comparing only lengths let traffic to
        // one endpoint replace or reorder an event already queued for
        // another without either length changing, and the drain property
        // exercises a single endpoint so it could not see it either.
        let mut expected: std::collections::BTreeMap<EndpointId, Vec<u8>> =
            all.iter().map(|e| (e.clone(), Vec::new())).collect();

        for (destination, id) in ops {
            if queues.push(event(&destination, id, b"x")).is_ok() {
                expected
                    .get_mut(&destination)
                    .expect("a known endpoint")
                    .push(id);
            }
            for e in &all {
                prop_assert_eq!(
                    queues.len(e),
                    expected[e].len(),
                    "traffic to another endpoint changed {}'s depth",
                    e.as_str()
                );
            }
        }

        // Drain every endpoint and compare the sequences. A push to one
        // queue must leave every other queue's contents identical, in
        // order, not merely the same length.
        for e in &all {
            let drained: Vec<u8> = queues
                .drain(e)
                .iter()
                .map(|ev| ev.message_id.as_bytes()[0])
                .collect();
            prop_assert_eq!(
                &drained,
                &expected[e],
                "{}'s queue contents were disturbed by traffic to another endpoint",
                e.as_str()
            );
        }
    }
}

// ---------------------------------------------------------------------
// ReservationMap — bounded in-flight, and budget that must come back
// ---------------------------------------------------------------------

proptest! {
    /// Neither bound is ever exceeded, whatever the sequence.
    ///
    /// Both are stated together because they failed together: a change
    /// that enlarged the per-peer allowance to satisfy one case let the
    /// global total drift past its own bound.
    #[test]
    fn neither_reservation_bound_is_ever_exceeded(
        max_global in 1usize..8,
        max_per_peer in 1usize..4,
        ops in prop::collection::vec((0usize..3, 0u8..4, any::<bool>()), 0..40),
    ) {
        let mut map = ReservationMap::new(max_global, max_per_peer);

        // Per source as well as in total. Comparing only the total
        // against `max_global` let an implementation admit more than
        // `max_per_peer` from one source and stay under the global cap —
        // so the property named both bounds and enforced one.
        // Charge per (source, key), because `release` frees the owner
        // AND every waiter attached to that key at once.
        let mut held: BTreeMap<(usize, u8), usize> = BTreeMap::new();

        for (source, id, acquire) in ops {
            let key = dedup_key(source, id);
            if acquire {
                if map.acquire(&key, fingerprint(b"same")).is_ok() {
                    *held.entry((source, id)).or_default() += 1;
                }
            } else {
                map.release(&key);
                held.remove(&(source, id));
            }
            prop_assert!(
                map.outstanding() <= max_global,
                "{} outstanding against a global bound of {max_global}",
                map.outstanding()
            );
            for peer in 0..3usize {
                let charged: usize = held
                    .iter()
                    .filter(|((s, _), _)| *s == peer)
                    .map(|(_, c)| *c)
                    .sum();
                prop_assert!(
                    charged <= max_per_peer,
                    "peer {peer} holds {charged} against a per-peer bound of {max_per_peer}"
                );
            }
        }
    }

    /// Releasing a key returns the owner's budget AND every waiter's.
    ///
    /// The implementation states this in prose — "release already returns
    /// the owner's and every waiter's budget together, so a waiter needs
    /// no settling of its own". A waiter holds a response channel until
    /// the owner resolves, so it is charged exactly as the owner is, and
    /// a release returning only the owner's share would leak the
    /// difference until restart.
    ///
    /// The leak is observed by SPENDING the allowance again, not by
    /// reading `outstanding()`. The first version of this property did
    /// the latter and was vacuous: `release` removes the entry from
    /// `in_flight` and `outstanding()` sums that map, so it reads zero
    /// however much the per-peer ledger still holds. Charging only the
    /// owner passed it. What the leak actually costs is future
    /// admissions, so that is what the property spends.
    #[test]
    fn releasing_a_key_returns_the_whole_charge(
        waiters in 0usize..6,
    ) {
        const PER_PEER: usize = 8;
        let mut map = ReservationMap::new(64, PER_PEER);
        let key = dedup_key(0, 1);
        let fp = fingerprint(b"same");

        prop_assert!(matches!(map.acquire(&key, fp), Ok(Reservation::Owner)));
        let mut attached = 0;
        for _ in 0..waiters {
            if matches!(map.acquire(&key, fp), Ok(Reservation::Waiter)) {
                attached += 1;
            }
        }
        prop_assert_eq!(map.outstanding(), attached + 1, "owner plus waiters are charged");

        map.release(&key);
        prop_assert_eq!(map.outstanding(), 0, "release left in-flight entries behind");
        prop_assert!(map.is_empty(), "release left the map non-empty");

        // The whole per-peer allowance must be spendable again. If
        // release returned only the owner's share, the first `attached`
        // of these are refused.
        for i in 0..PER_PEER {
            let fresh = dedup_key(0, u8::try_from(100 + i).expect("in range"));
            prop_assert!(
                matches!(map.acquire(&fresh, fingerprint(b"fresh")), Ok(Reservation::Owner)),
                "reservation {i} of {PER_PEER} was refused after release — \
                 {attached} waiters' budget did not come back"
            );
        }
    }

    /// A conflicting fingerprint spends no budget.
    ///
    /// A conflict is a refusal, and a refusal that charged for itself
    /// would let a peer exhaust an endpoint's in-flight allowance by
    /// replaying one message id with different bodies.
    #[test]
    fn a_fingerprint_conflict_never_consumes_budget(
        conflicts in 1usize..12,
    ) {
        const PER_PEER: usize = 8;
        let mut map = ReservationMap::new(64, PER_PEER);
        let key = dedup_key(0, 1);

        prop_assert!(map.acquire(&key, fingerprint(b"first")).is_ok());
        let charged = map.outstanding();

        for i in 0..conflicts {
            let body = format!("different-{i}");
            prop_assert!(matches!(
                map.acquire(&key, fingerprint(body.as_bytes())),
                Err(ReservationFailure::Conflict)
            ));
            prop_assert_eq!(
                map.outstanding(),
                charged,
                "a conflict changed the outstanding charge"
            );
        }

        // SPEND the rest of the allowance. `outstanding()` above reads
        // only `in_flight`, so a conflict that wrongly charged the
        // separate per-peer ledger would satisfy every assertion so far
        // and only surface as refusals later. This is the same vacuity
        // the release property had; fixing it there and leaving it here
        // is what a review caught.
        for i in 0..PER_PEER - 1 {
            let fresh = dedup_key(0, u8::try_from(100 + i).expect("in range"));
            prop_assert!(
                matches!(map.acquire(&fresh, fingerprint(b"fresh")), Ok(Reservation::Owner)),
                "reservation {i} was refused after {conflicts} conflicts — \
                 a conflict spent per-peer budget"
            );
        }
    }

    /// One peer can never spend another's allowance.
    ///
    /// The per-peer bound exists so a flooding peer cannot starve a quiet
    /// one. Stated as an invariant it also covers the regime a scenario
    /// test misses: a quiet peer arriving AFTER the loud one saturated
    /// the map.
    #[test]
    fn a_saturated_peer_never_exhausts_another_peers_allowance(
        max_per_peer in 1usize..4,
        flood in 4usize..20,
    ) {
        let max_global = max_per_peer * 3;
        let mut map = ReservationMap::new(max_global, max_per_peer);

        for id in 0..flood {
            let _ = map.acquire(&dedup_key(0, u8::try_from(id % 250).expect("in range")),
                                fingerprint(b"loud"));
        }

        // The quiet peer arrives afterwards and must get its WHOLE
        // allowance, not merely its first reservation. Checking only the
        // first would pass an implementation that admits it and then
        // counts the loud peer's outstanding requests when deciding the
        // quiet peer's second — and the bounds property cannot catch
        // that, because a premature refusal violates no upper bound.
        for i in 0..max_per_peer {
            let quiet = dedup_key(1, u8::try_from(200 + i).expect("in range"));
            prop_assert!(
                matches!(map.acquire(&quiet, fingerprint(b"quiet")), Ok(Reservation::Owner)),
                "reservation {i} of {max_per_peer} was refused — a flooding \
                 peer consumed a quiet peer's allowance"
            );
        }
    }
}
