// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The lease boundary: a session sends AS the endpoint it holds, and
//! nothing else decides the source.
//!
//! Stage 6 enforced that a frame's `source_endpoint` named a lease the
//! NODE held, and `configure_direct` leased every enabled endpoint, so a
//! handle holder could name any of them. That was carried to Stage 8 by
//! explicit decision (PR #38) because it needed a second party to be
//! observable — and sessions are that party. `contracts/ENDPOINTS.md`
//! outbound step 1 and CLAUDE.md §5 are the governing text.
//!
//! Over real sockets, because the claim is about what the REMOTE sees as
//! the sender: the remote's delivered event is the assertion.
#![allow(clippy::expect_used, clippy::panic)]

mod support;

use interweave_transport_api::TransportError;
use support::{claim_all, connected_pair_claiming, endpoint, frame};

/// The inherited P1, closed. Session `human` builds a frame that names
/// `claude` as its source — an endpoint this same node holds a lease on,
/// through another session — and the remote delivers it as `human`.
///
/// Mutation: honour the frame's own field in the `SendDirect` arm, and
/// the remote sees `claude`.
#[tokio::test]
async fn a_session_sends_only_as_its_own_endpoint() {
    let (sender, receiver, peer) =
        connected_pair_claiming(&["human", "claude"], &["human", "claude"]).await;

    sender
        .send_direct(
            "human",
            peer,
            frame("claude", Some("claude"), b"who am i", 1),
        )
        .await
        .expect("command")
        .expect("accepted");

    let delivered = receiver
        .drain_endpoint(endpoint("claude"))
        .await
        .expect("answers");
    assert_eq!(delivered.len(), 1);
    assert_eq!(
        delivered[0].source_endpoint,
        endpoint("human"),
        "the source is the SESSION's lease, not the frame's field"
    );
}

/// A session holding no lease cannot send at all — even though the
/// endpoint it names is enabled, configured, and leased by someone else
/// on this very node.
#[tokio::test]
async fn a_session_without_a_lease_cannot_send() {
    let (sender, receiver, peer) = connected_pair_claiming(&["human"], &["human", "claude"]).await;

    let error = sender
        .send_direct(
            "ghost",
            peer.clone(),
            frame("human", Some("claude"), b"spoof", 2),
        )
        .await
        .expect("command")
        .expect_err("a session with no lease has no source");
    assert_eq!(error, TransportError::EndpointNotRegistered);
    assert!(
        receiver
            .drain_endpoint(endpoint("claude"))
            .await
            .expect("answers")
            .is_empty(),
        "nothing crossed the wire"
    );

    // Positive control: the session that DOES hold `human` sends.
    sender
        .send_direct("human", peer, frame("human", Some("claude"), b"real", 3))
        .await
        .expect("command")
        .expect("the lease holder sends");
    assert_eq!(
        receiver
            .drain_endpoint(endpoint("claude"))
            .await
            .expect("answers")
            .len(),
        1
    );
}

/// Configuration is not a lease. An enabled endpoint nobody has claimed
/// answers `no_route` — coarsely, like every other routing failure —
/// until a session claims it, and then it routes.
///
/// Mutation: make `configure` claim every enabled endpoint again, and
/// the first half fails.
#[tokio::test]
async fn an_enabled_unleased_endpoint_is_no_route_until_claimed() {
    let (sender, receiver, peer) = connected_pair_claiming(&["human"], &["human"]).await;

    let error = sender
        .send_direct(
            "human",
            peer.clone(),
            frame("human", Some("claude"), b"early", 4),
        )
        .await
        .expect("command")
        .expect_err("configured but unleased is offline");
    assert_eq!(error, TransportError::RemoteEndpointUnavailable);

    receiver
        .claim_endpoint("claude", endpoint("claude"), "in-process")
        .await
        .expect("command")
        .expect("the endpoint is free");

    sender
        .send_direct("human", peer, frame("human", Some("claude"), b"now", 5))
        .await
        .expect("command")
        .expect("leased, so it routes");
    let delivered = receiver
        .drain_endpoint(endpoint("claude"))
        .await
        .expect("answers");
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].payload.bytes(), b"now");
}

/// A release frees the endpoint for the next session and takes the
/// queue with it; the released session can no longer send.
#[tokio::test]
async fn release_frees_the_endpoint_for_the_next_session() {
    let (sender, receiver, peer) = connected_pair_claiming(&["human"], &["human"]).await;

    // s1 holds `claude` and one message lands for it.
    receiver
        .claim_endpoint("s1", endpoint("claude"), "in-process")
        .await
        .expect("command")
        .expect("free");
    sender
        .send_direct(
            "human",
            peer.clone(),
            frame("human", Some("claude"), b"for s1", 6),
        )
        .await
        .expect("command")
        .expect("accepted");

    let released = receiver.release_session("s1").await.expect("command");
    assert_eq!(released, vec![endpoint("claude")]);
    assert!(
        receiver
            .drain_endpoint(endpoint("claude"))
            .await
            .expect("answers")
            .is_empty(),
        "the queue went with the lease; no daemon-side backlog for an offline endpoint"
    );

    // s2 can now take it, and s1 cannot send as anything.
    receiver
        .claim_endpoint("s2", endpoint("claude"), "in-process")
        .await
        .expect("command")
        .expect("released, so free");
    let released = sender.release_session("human").await.expect("command");
    assert_eq!(released, vec![endpoint("human")]);
    let error = sender
        .send_direct("human", peer, frame("human", Some("claude"), b"after", 7))
        .await
        .expect("command")
        .expect_err("the lease is gone");
    assert_eq!(error, TransportError::EndpointNotRegistered);
}

/// Exclusivity through the handle: a held endpoint refuses a second
/// session, and a session refuses a second endpoint. The registry tests
/// these in isolation; here the caller is the one that will exist.
#[tokio::test]
async fn a_lease_is_exclusive_in_both_directions() {
    let (sender, _receiver, _peer) = connected_pair_claiming(&["human"], &[]).await;

    let error = sender
        .claim_endpoint("intruder", endpoint("human"), "in-process")
        .await
        .expect("command")
        .expect_err("held by `human`");
    assert_eq!(error, TransportError::EndpointInUse);

    let error = sender
        .claim_endpoint("human", endpoint("claude"), "in-process")
        .await
        .expect("command")
        .expect_err("`human` already holds one");
    assert_eq!(
        error,
        TransportError::InvalidArgument,
        "a second lease for one session is the caller's mistake, not an endpoint answer"
    );

    // And the refusals changed nothing: `claude` is still free.
    claim_all(&sender, &["claude"]).await;
}
