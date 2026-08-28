// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The lease boundary: a caller sends AS the endpoint its LEASE names, and
//! nothing else — not a frame field, not a guessable session string —
//! decides the source.
//!
//! Stage 6 enforced that a frame's `source_endpoint` named a lease the
//! NODE held, and `configure_direct` leased every enabled endpoint, so a
//! handle holder could name any of them. That was carried to Stage 8 by
//! explicit decision (PR #38). The close is the LEASE as capability:
//! `send_direct` takes the `EndpointLease` `claim_endpoint` returned, and
//! its 128-bit epoch is verified against the live lease — so a caller
//! cannot send as an endpoint it did not claim, because it does not hold
//! the matching epoch. `contracts/ENDPOINTS.md` ("callers cannot spoof
//! another local endpoint") and CLAUDE.md §5 are the governing text.
//!
//! Over real sockets, because the claim is about what the REMOTE sees as
//! the sender: the remote's delivered event is the assertion.
#![allow(clippy::expect_used, clippy::panic)]

mod support;

use interweave_local_client_api::{EndpointLease, Generation};
use interweave_transport_api::TransportError;
use support::{claim_all, connected_pair_claiming, endpoint, frame};

/// The inherited P1, closed. The sender holds `human`'s lease and builds a
/// frame that names `claude` as its source — an endpoint this same node
/// holds a lease on, through another session — and the remote delivers it
/// as `human`. The frame field is never consulted.
///
/// Mutation: honour the frame's own field in the `SendDirect` arm, and
/// the remote sees `claude`.
#[tokio::test]
async fn a_send_is_as_the_leases_endpoint_never_the_frames() {
    let (sender, receiver, peer, leases) =
        connected_pair_claiming(&["human", "claude"], &["human", "claude"]).await;

    sender
        .send_direct(
            &leases["human"],
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
        "the source is the LEASE's endpoint, not the frame's field"
    );
}

/// A lease carrying the wrong epoch cannot send — even for an endpoint
/// that exists, is enabled, and is really leased by another session on
/// this node. The epoch is the unforgeable proof of ownership: a caller
/// that did not claim the endpoint does not hold it.
///
/// This is the spoofing boundary itself: a `String` session id would let
/// any handle holder name another session, but a lease requires the
/// 128-bit secret that only the claim returned.
///
/// Mutation: verify only `lease.endpoint` and ignore the epoch in
/// `holds_lease`, and the forged lease sends.
#[tokio::test]
async fn a_lease_with_the_wrong_epoch_cannot_send() {
    let (sender, receiver, peer, leases) =
        connected_pair_claiming(&["human"], &["human", "claude"]).await;

    // A forged lease: the right endpoint name, a fabricated epoch. The
    // sender genuinely holds `human`, but not under THIS epoch.
    let forged = EndpointLease {
        endpoint: endpoint("human"),
        epoch: Generation::parse("forged__________").expect("a valid generation string"),
    };
    let error = sender
        .send_direct(
            &forged,
            peer.clone(),
            frame("human", Some("claude"), b"spoof", 2),
        )
        .await
        .expect("command")
        .expect_err("a fabricated epoch matches no live lease");
    assert_eq!(error, TransportError::EndpointNotRegistered);
    assert!(
        receiver
            .drain_endpoint(endpoint("claude"))
            .await
            .expect("answers")
            .is_empty(),
        "nothing crossed the wire"
    );

    // Positive control: the REAL lease, with the epoch the claim returned,
    // sends.
    sender
        .send_direct(
            &leases["human"],
            peer,
            frame("human", Some("claude"), b"real", 3),
        )
        .await
        .expect("command")
        .expect("the genuine lease holder sends");
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
    let (sender, receiver, peer, leases) = connected_pair_claiming(&["human"], &["human"]).await;

    let error = sender
        .send_direct(
            &leases["human"],
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
        .send_direct(
            &leases["human"],
            peer,
            frame("human", Some("claude"), b"now", 5),
        )
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

/// A release frees the endpoint for the next session, takes the queue
/// with it, and — the capability point — invalidates the released lease:
/// the same `EndpointLease` value can no longer send, because the epoch it
/// carries no longer matches any live lease.
#[tokio::test]
async fn release_frees_the_endpoint_and_invalidates_its_lease() {
    let (sender, receiver, peer, leases) = connected_pair_claiming(&["human"], &["human"]).await;
    let human = leases["human"].clone();

    // s1 holds `claude` and one message lands for it.
    receiver
        .claim_endpoint("s1", endpoint("claude"), "in-process")
        .await
        .expect("command")
        .expect("free");
    sender
        .send_direct(
            &human,
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

    // s2 can now take it, and the sender's released lease sends nothing.
    receiver
        .claim_endpoint("s2", endpoint("claude"), "in-process")
        .await
        .expect("command")
        .expect("released, so free");
    let released = sender.release_session("human").await.expect("command");
    assert_eq!(released, vec![endpoint("human")]);
    let error = sender
        .send_direct(&human, peer, frame("human", Some("claude"), b"after", 7))
        .await
        .expect("command")
        .expect_err("the lease was released, so its epoch matches nothing");
    assert_eq!(error, TransportError::EndpointNotRegistered);
}

/// Exclusivity through the handle: a held endpoint refuses a second
/// session, and a session refuses a second endpoint. The registry tests
/// these in isolation; here the caller is the one that will exist.
#[tokio::test]
async fn a_lease_is_exclusive_in_both_directions() {
    let (sender, _receiver, _peer, _leases) = connected_pair_claiming(&["human"], &[]).await;

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
