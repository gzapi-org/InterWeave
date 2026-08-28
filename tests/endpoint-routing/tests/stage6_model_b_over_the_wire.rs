// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Model B over the wire: one PeerId, several local endpoints.
//!
//! ADR-0030's central claim, and the reason this suite is separate from
//! `tests/direct-v2`: that one asks whether the wire is correct, this
//! asks whether a message addressed to `human` reaches ONLY `human` on a
//! profile where `claude` holds a lease on the same PeerId. A failure
//! here is a registry or routing defect, and a reader should not have to
//! tell the two apart by reading the assertion.
//!
//! Run against real peers because the property is about what crosses a
//! socket. A mock delivers wherever the test told it to.
#![allow(clippy::expect_used, clippy::panic)]

mod support;

use support::{connected_pair, endpoint, frame};

/// Scenarios 2 and 3: a send to `human` reaches only human, a send to
/// `claude` reaches only Claude — on ONE PeerId.
#[tokio::test]
async fn each_endpoint_receives_only_what_was_addressed_to_it() {
    let (sender, receiver, peer) = connected_pair().await;

    sender
        .send_direct(
            "human",
            peer.clone(),
            frame("human", Some("human"), b"for the human", 1),
        )
        .await
        .expect("command")
        .expect("accepted");
    sender
        .send_direct(
            "human",
            peer.clone(),
            frame("human", Some("claude"), b"for claude", 2),
        )
        .await
        .expect("command")
        .expect("accepted");
    sender
        .send_direct(
            "human",
            peer,
            frame("human", Some("gpt-5"), b"for gpt-5", 3),
        )
        .await
        .expect("command")
        .expect("accepted");

    for (name, expected) in [
        ("human", b"for the human".as_slice()),
        ("claude", b"for claude"),
        ("gpt-5", b"for gpt-5"),
    ] {
        let delivered = receiver
            .drain_endpoint(endpoint(name))
            .await
            .expect("answers");
        assert_eq!(delivered.len(), 1, "`{name}` received exactly one message");
        assert_eq!(
            delivered[0].payload.bytes(),
            expected,
            "`{name}` received the one addressed to it"
        );
        assert_eq!(delivered[0].destination_endpoint, endpoint(name));
    }
}

/// `gpt-5` is not a special case. Model B's endpoints are configured
/// labels, so a model nobody had heard of when this stage was written
/// routes exactly as `human` and `claude` do — no code knows the names.
#[tokio::test]
async fn an_endpoint_this_stage_never_heard_of_routes_like_any_other() {
    let (sender, receiver, peer) = connected_pair().await;

    let resolved = sender
        .send_direct("human", peer, frame("human", Some("gpt-5"), b"hello", 4))
        .await
        .expect("command")
        .expect("a configured endpoint is a configured endpoint");
    assert_eq!(resolved, endpoint("gpt-5"));

    assert_eq!(
        receiver
            .drain_endpoint(endpoint("gpt-5"))
            .await
            .expect("answers")
            .len(),
        1
    );
    for other in ["human", "claude"] {
        assert!(
            receiver
                .drain_endpoint(endpoint(other))
                .await
                .expect("answers")
                .is_empty(),
            "`{other}` received nothing"
        );
    }
}

/// Scenario 13: one message id from one peer, but two SOURCE endpoints,
/// produces independent deliveries.
///
/// The source endpoint is a dedup dimension, so two clients on one
/// profile may each use the same idempotency key without one silencing
/// the other. Collapsing the key to `(peer, message_id)` would make the
/// second arrival a duplicate of the first and drop it.
#[tokio::test]
async fn one_id_from_two_source_endpoints_delivers_twice() {
    let (sender, receiver, peer) = connected_pair().await;

    sender
        .send_direct(
            "human",
            peer.clone(),
            frame("human", Some("claude"), b"from human", 5),
        )
        .await
        .expect("command")
        .expect("accepted");
    sender
        .send_direct(
            "gpt-5",
            peer,
            frame("gpt-5", Some("claude"), b"from gpt-5", 5),
        )
        .await
        .expect("command")
        .expect("the same id from a different source is not a duplicate");

    let delivered = receiver
        .drain_endpoint(endpoint("claude"))
        .await
        .expect("answers");
    assert_eq!(delivered.len(), 2, "both were delivered");

    let sources: Vec<&str> = delivered
        .iter()
        .map(|e| e.source_endpoint.as_str())
        .collect();
    assert!(sources.contains(&"human"), "the human's copy arrived");
    assert!(sources.contains(&"gpt-5"), "and the other one did too");
    assert_eq!(
        delivered[0].message_id, delivered[1].message_id,
        "under one shared id"
    );
}

/// The same id from the SAME source is a duplicate, which is the other
/// half of the claim above — without it "two deliveries" could simply
/// mean dedup is not working at all.
#[tokio::test]
async fn one_id_from_one_source_delivers_once() {
    let (sender, receiver, peer) = connected_pair().await;
    let repeated = frame("human", Some("claude"), b"same body", 6);

    sender
        .send_direct("human", peer.clone(), repeated.clone())
        .await
        .expect("command")
        .expect("accepted");
    sender
        .send_direct("human", peer, repeated)
        .await
        .expect("command")
        .expect("the retry is accepted from cache");

    assert_eq!(
        receiver
            .drain_endpoint(endpoint("claude"))
            .await
            .expect("answers")
            .len(),
        1,
        "one enqueue, however many times it is sent"
    );
}
