// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `contracts/LOCAL-CLIENT.md` §7's conformance items, as checks over the
//! neutral binding traits: a platform binding passes by running every one
//! of them against a sender and a receiver it composed. The in-process
//! binding runs them in `tests/in_process.rs` (Stage 12); the IPC adapter
//! runs the same functions at Stage 13.
//!
//! Each check names the item it proves. Item 7 (data-plane callbacks cannot
//! invoke administration) is structural in the traits -- a
//! [`DataSessionPort`] has no method yielding authority -- and its runtime
//! half (the admin facade's own capability, and the notice a revocation
//! owes the holder) is the binding's, checked beside it.
//!
//! Test-only code: panics are the reports.
#![allow(clippy::expect_used, clippy::panic, clippy::missing_panics_doc)]

use std::time::Duration;

use interweave_local_client_api::{
    DataCapability, DataSessionBinding, DataSessionPort, SessionEvent, SessionRequest,
};
use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, DirectDestination, EndpointId, MediaType, MessageId, Payload,
    TransportError, TransportIdentity,
};

/// How long a check waits for something that should arrive.
pub const PATIENCE: Duration = Duration::from_secs(20);

/// A request for `endpoint` with both data capabilities.
#[must_use]
pub fn full(endpoint: Option<&EndpointId>) -> SessionRequest {
    SessionRequest::new(
        "conformance",
        endpoint.cloned(),
        [DataCapability::Commands, DataCapability::Events],
    )
    .expect("in bounds")
}

/// A text payload.
#[must_use]
pub fn text(body: &str) -> Payload {
    Payload::at_ceiling(
        Some(MediaType::parse("text/plain").expect("valid media type")),
        body.as_bytes().to_vec(),
    )
    .expect("within the ceiling")
}

/// Everything `session` receives within `patience` once something arrives,
/// polling rather than sleeping once so slow is not read as absent.
pub async fn receive<S: DataSessionPort>(session: &S, patience: Duration) -> Vec<SessionEvent> {
    let deadline = tokio::time::Instant::now() + patience;
    loop {
        let got = session.events().await.expect("events answer");
        if !got.is_empty() || tokio::time::Instant::now() >= deadline {
            return got;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Item 1: the source endpoint a receiver sees is the sender's LEASE, and
/// the send names only a destination -- the trait has no parameter
/// through which a caller could name a source.
pub async fn the_source_endpoint_is_the_senders_lease<B: DataSessionBinding>(
    sender: &B,
    receiver: &B,
    sender_peer: &TransportIdentity,
    receiver_peer: &TransportIdentity,
    source: &EndpointId,
    endpoint: &EndpointId,
) {
    // Two DIFFERENT endpoints, or a receive path that filled the source
    // from the destination would pass (#139 review F6).
    assert_ne!(source, endpoint, "the check needs distinct endpoints");
    let from = sender
        .open(full(Some(source)))
        .await
        .expect("the sender leases");
    let to = receiver
        .open(full(Some(endpoint)))
        .await
        .expect("the receiver leases");
    let accepted = from
        .send_direct(
            DirectDestination {
                peer: receiver_peer.clone(),
                endpoint: Some(endpoint.clone()),
            },
            MessageId::from_bytes([1; 16]),
            text("hello"),
        )
        .await
        .expect("accepted");
    assert_eq!(
        &accepted, endpoint,
        "AcceptedV2 names the endpoint that took it"
    );
    let got = receive(&to, PATIENCE).await;
    let [SessionEvent::Direct(message)] = got.as_slice() else {
        panic!("one direct message: {got:?}");
    };
    assert_eq!(&message.source_peer, sender_peer, "Noise proved the peer");
    assert_eq!(
        &message.source_endpoint, source,
        "the source endpoint is the sender's lease"
    );
    assert_eq!(from.session().source_endpoint(), Some(source));
    assert_eq!(&message.destination_endpoint, endpoint);
    from.close().await.expect("closes");
    to.close().await.expect("closes");
}

/// Items 2 and 5: one live owner per endpoint, and closing a session
/// releases its lease at once -- the next claim succeeds, with a fresh
/// epoch.
pub async fn a_lease_is_exclusive_and_released_on_close<B: DataSessionBinding>(
    binding: &B,
    endpoint: &EndpointId,
) {
    let first = binding.open(full(Some(endpoint))).await.expect("leases");
    let first_epoch = first
        .session()
        .endpoint_lease()
        .expect("direct-capable")
        .epoch
        .clone();
    assert!(
        matches!(
            binding.open(full(Some(endpoint))).await,
            Err(TransportError::EndpointInUse)
        ),
        "a second owner is refused"
    );
    first.close().await.expect("closes");
    let second = binding
        .open(full(Some(endpoint)))
        .await
        .expect("the lease was released on close");
    assert_ne!(
        second.session().endpoint_lease().expect("leased").epoch,
        first_epoch,
        "every grant has a fresh epoch"
    );
    second.close().await.expect("closes");
}

/// Item 5 when a session ends without `close`: dropping it is its
/// teardown for an in-process binding, and the lease comes back without
/// an administrator (#139 review F3).
pub async fn a_dropped_session_releases_its_lease<B: DataSessionBinding>(
    binding: &B,
    endpoint: &EndpointId,
) {
    let held = binding.open(full(Some(endpoint))).await.expect("leases");
    drop(held);
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        match binding.open(full(Some(endpoint))).await {
            Ok(next) => {
                next.close().await.expect("closes");
                return;
            }
            Err(TransportError::EndpointInUse) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "a dropped session kept its lease"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => panic!("unexpected refusal: {e:?}"),
        }
    }
}

/// Items 3 and 6: the receiver's queue is bounded, and `AcceptedV2` is
/// given only for a message its queue admitted -- past the bound the
/// sender is told `Overloaded`, and nothing waits in a hidden mailbox.
pub async fn the_queue_is_bounded_and_acceptance_follows_admission<B: DataSessionBinding>(
    sender: &B,
    receiver: &B,
    receiver_peer: &TransportIdentity,
    endpoint: &EndpointId,
) {
    let from = sender.open(full(Some(endpoint))).await.expect("leases");
    let to = receiver.open(full(Some(endpoint))).await.expect("leases");
    let bound = to.session().event_queue();
    let destination = DirectDestination {
        peer: receiver_peer.clone(),
        endpoint: Some(endpoint.clone()),
    };
    for i in 0..bound {
        let id = u8::try_from(i + 1).expect("a small bound");
        from.send_direct(
            destination.clone(),
            MessageId::from_bytes([id; 16]),
            text("fill"),
        )
        .await
        .expect("accepted while the queue has room");
    }
    assert_eq!(
        from.send_direct(
            destination.clone(),
            MessageId::from_bytes([0xee; 16]),
            text("over")
        )
        .await,
        Err(TransportError::Overloaded),
        "past the bound, acceptance is withheld"
    );
    let got = to.events().await.expect("events answer");
    assert_eq!(got.len(), bound, "exactly the admitted ones wait: {got:?}");
    from.close().await.expect("closes");
    to.close().await.expect("closes");
}

/// Item 4: the local refusals are the contract's exact errors.
pub async fn local_refusals_map_exactly<B: DataSessionBinding>(
    binding: &B,
    peer: &TransportIdentity,
    unknown: &EndpointId,
) {
    assert!(
        matches!(
            binding.open(full(Some(unknown))).await,
            Err(TransportError::EndpointUnknown)
        ),
        "an unconfigured endpoint"
    );
    let unleased = binding
        .open(full(None))
        .await
        .expect("opens without a lease");
    assert_eq!(
        unleased
            .send_direct(
                DirectDestination {
                    peer: peer.clone(),
                    endpoint: None,
                },
                MessageId::from_bytes([2; 16]),
                text("x"),
            )
            .await,
        Err(TransportError::EndpointNotRegistered),
        "a session with no lease cannot send as nobody"
    );
    unleased.close().await.expect("closes");
    let mute = binding
        .open(SessionRequest::new("conformance", None, []).expect("in bounds"))
        .await
        .expect("opens");
    assert_eq!(
        mute.join(ChannelId::parse("general").expect("valid")).await,
        Err(TransportError::CapabilityDenied),
        "no commands, no join"
    );
    assert_eq!(
        mute.events().await,
        Err(TransportError::CapabilityDenied),
        "no events, no events"
    );
    mute.close().await.expect("closes");
}

/// Item 8: no binding adds durable delivery. A message sent while no
/// session holds the endpoint is refused, not kept, and a later owner
/// receives nothing from before its lease.
pub async fn nothing_is_kept_for_an_unleased_endpoint<B: DataSessionBinding>(
    sender: &B,
    receiver: &B,
    receiver_peer: &TransportIdentity,
    endpoint: &EndpointId,
) {
    let from = sender.open(full(Some(endpoint))).await.expect("leases");
    let destination = DirectDestination {
        peer: receiver_peer.clone(),
        endpoint: Some(endpoint.clone()),
    };
    assert!(
        from.send_direct(
            destination.clone(),
            MessageId::from_bytes([3; 16]),
            text("nobody")
        )
        .await
        .is_err(),
        "no owner: refused, not held"
    );
    let later = receiver.open(full(Some(endpoint))).await.expect("leases");
    assert!(
        receive(&later, Duration::from_millis(500)).await.is_empty(),
        "nothing replayed to the next owner"
    );
    from.close().await.expect("closes");
    later.close().await.expect("closes");
}

/// The broadcast half of the session contract: delivery goes to the
/// sessions that JOINED, and a session's close leaves its joins.
pub async fn broadcast_reaches_joined_sessions_only<B: DataSessionBinding>(
    publisher: &B,
    receiver: &B,
    publisher_peer: &TransportIdentity,
    channel: &ChannelId,
) {
    let from = publisher.open(full(None)).await.expect("opens");
    from.join(channel.clone()).await.expect("joins");
    let joined = receiver.open(full(None)).await.expect("opens");
    joined.join(channel.clone()).await.expect("joins");
    let bystander = receiver.open(full(None)).await.expect("opens");

    // The mesh forms on its own schedule: publish until one arrives.
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut got = Vec::new();
    let mut id = 0_u8;
    while got.is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no broadcast arrived"
        );
        id = id.wrapping_add(1);
        from.broadcast(
            channel.clone(),
            BroadcastMessageV1 {
                message_id: MessageId::from_bytes([id; 16]),
                sent_at_ms: 1_786_600_000_000,
                payload: text("to the channel"),
            },
        )
        .await
        .expect("accepted locally");
        got = receive(&joined, Duration::from_millis(500)).await;
    }
    let SessionEvent::Broadcast(message) = &got[0] else {
        panic!("a broadcast: {got:?}");
    };
    assert_eq!(
        &message.source_peer, publisher_peer,
        "the signature proved it"
    );
    assert_eq!(&message.channel, channel);
    assert!(
        bystander.events().await.expect("answers").is_empty(),
        "a session that did not join receives nothing"
    );
    from.close().await.expect("closes");
    joined.close().await.expect("closes");
    bystander.close().await.expect("closes");
}
