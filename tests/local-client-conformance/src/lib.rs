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
//! half (the admin port's own capabilities, the notice a revocation owes
//! the holder, the overlay's rules) is checked here too, generic over
//! [`AdminBinding`] (plan §16 (2)), so every binding runs it.
//!
//! Test-only code: panics are the reports.
#![allow(clippy::expect_used, clippy::panic, clippy::missing_panics_doc)]

use std::time::Duration;

use interweave_local_client_api::{
    AdminBinding, AdminCapability, AdminPort, DataCapability, DataSessionBinding, DataSessionPort,
    LocalSessionEvent, SessionEvent, SessionRequest, TrustAdminView, TrustSource, TrustedPeer,
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

/// One `events` call, with the runtime's state set aside. The state is
/// owed at open and on every change, so any read may carry one; the
/// helpers below are about what was sent and admitted, and item 10
/// (`the_runtimes_state_is_owed_once_at_open`) reads the state
/// on its own.
async fn take_all<S: DataSessionPort>(session: &S) -> Vec<SessionEvent> {
    session
        .events(usize::MAX)
        .await
        .expect("events answer")
        .into_iter()
        .filter(|e| {
            !matches!(
                e,
                SessionEvent::Local(LocalSessionEvent::ServerState { .. })
            )
        })
        .collect()
}

/// Everything `session` receives within `patience` once something arrives,
/// polling rather than sleeping once so slow is not read as absent.
pub async fn receive<S: DataSessionPort>(session: &S, patience: Duration) -> Vec<SessionEvent> {
    let deadline = tokio::time::Instant::now() + patience;
    loop {
        let got = take_all(session).await;
        if !got.is_empty() || tokio::time::Instant::now() >= deadline {
            return got;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// How long an absence is watched for. A binding may deliver an event
/// after the call that admitted it returns -- over IPC it is pushed and
/// read asynchronously (architect-cto, relay seq 9766, G3) -- so "nothing
/// arrived" is only said after this long.
pub const SETTLE: Duration = Duration::from_millis(500);

/// Everything `session` receives over `window`, polled throughout: an
/// absence check reads `is_empty()` of this, never of one `events` call.
pub async fn arriving_within<S: DataSessionPort>(
    session: &S,
    window: Duration,
) -> Vec<SessionEvent> {
    let deadline = tokio::time::Instant::now() + window;
    let mut got = Vec::new();
    loop {
        got.extend(take_all(session).await);
        if tokio::time::Instant::now() >= deadline {
            return got;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// At least `count` events for `session`, polled for up to `patience`;
/// fewer when the patience ran out, for the caller's assertion to name.
pub async fn receive_at_least<S: DataSessionPort>(
    session: &S,
    count: usize,
    patience: Duration,
) -> Vec<SessionEvent> {
    let deadline = tokio::time::Instant::now() + patience;
    let mut got = Vec::new();
    loop {
        got.extend(take_all(session).await);
        if got.len() >= count || tokio::time::Instant::now() >= deadline {
            return got;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Whether `event` is the runtime's state.
fn is_state(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::Local(LocalSessionEvent::ServerState { .. })
    )
}

/// `LOCAL-CLIENT.md` §2 (A 2026-10-06): a session's creation context
/// carries the profile's `PeerId`, the identity a data-plane client
/// reports as its own. `local` is the profile `binding` serves; `other`,
/// the far node's, is the control that the answer is not just any peer.
pub async fn a_session_reports_its_profile_peer<B: DataSessionBinding>(
    binding: &B,
    local: &TransportIdentity,
    other: &TransportIdentity,
) {
    assert_ne!(local, other, "the control needs two peers");
    let session = binding.open(full(None)).await.expect("opens");
    assert_eq!(session.session().local_peer(), local);
}

/// Item 10, the state's half: a session is owed exactly one `ServerState`
/// at open, and no second while nothing changed -- the runtime's state is
/// a notice owed on change, not a stream (`LOCAL-CLIENT.md`, A
/// 2026-10-03). The coalescing of several changes into one pending is
/// each binding's own test, since only the binding can drive a change.
pub async fn the_runtimes_state_is_owed_once_at_open<B: DataSessionBinding>(receiver: &B) {
    let session = receiver.open(full(None)).await.expect("opens");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut states = 0;
    while states == 0 {
        assert!(tokio::time::Instant::now() < deadline, "no state at open");
        states += session
            .events(usize::MAX)
            .await
            .expect("events answer")
            .iter()
            .filter(|e| is_state(e))
            .count();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(states, 1, "one at open, never two pending");
    let mut later = Vec::new();
    let settle = tokio::time::Instant::now() + SETTLE;
    while tokio::time::Instant::now() < settle {
        later.extend(session.events(usize::MAX).await.expect("events answer"));
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        !later.iter().any(is_state),
        "no second state without a change: {later:?}"
    );
}

/// Item 9: `ready()` resolves when at least one event is queued and takes
/// nothing -- `events` after it takes what was there -- and waits while
/// nothing is. The wait is checked beside its positive control on the
/// same session: the direct message that ends it.
pub async fn ready_resolves_on_what_waits_and_takes_nothing<B: DataSessionBinding>(
    sender: &B,
    receiver: &B,
    receiver_peer: &TransportIdentity,
    source: &EndpointId,
    endpoint: &EndpointId,
) {
    let from = sender.open(full(Some(source))).await.expect("leases");
    let to = receiver.open(full(Some(endpoint))).await.expect("leases");
    // The state owed at open is what waits first.
    tokio::time::timeout(PATIENCE, to.ready())
        .await
        .expect("the open-time state ends the wait")
        .expect("ready");
    let opened = to.events(usize::MAX).await.expect("events answer");
    assert!(
        opened.iter().any(is_state),
        "the state was there: {opened:?}"
    );
    assert!(
        tokio::time::timeout(SETTLE, to.ready()).await.is_err(),
        "nothing owed: ready waits"
    );
    {
        let waiting = to.ready();
        tokio::pin!(waiting);
        from.send_direct(
            DirectDestination {
                peer: receiver_peer.clone(),
                endpoint: Some(endpoint.clone()),
            },
            MessageId::from_bytes([9; 16]),
            text("wake"),
        )
        .await
        .expect("accepted");
        tokio::time::timeout(PATIENCE, &mut waiting)
            .await
            .expect("the message ends the wait")
            .expect("ready");
    }
    // Taken nothing: the message is still there, for `ready` again and
    // then for `events`, once.
    tokio::time::timeout(PATIENCE, to.ready())
        .await
        .expect("still owed")
        .expect("ready");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut got = Vec::new();
    while got.is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the message never came"
        );
        got.extend(take_all(&to).await);
    }
    assert!(
        matches!(got.as_slice(), [SessionEvent::Direct(m)] if m.message_id == MessageId::from_bytes([9; 16])),
        "{got:?}"
    );
    from.close().await.expect("closes");
    to.close().await.expect("closes");
    let mute = receiver
        .open(
            SessionRequest::new("conformance", None, [DataCapability::Commands])
                .expect("in bounds"),
        )
        .await
        .expect("opens");
    assert_eq!(
        mute.ready().await,
        Err(TransportError::CapabilityDenied),
        "no events, nothing to wait for"
    );
    mute.close().await.expect("closes");
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
    // A session on the receiving side that claimed no endpoint: it holds
    // `commands` and `events`, and no direct message is ever routed to it
    // (LOCAL-IPC.md, A 2026-09-30).
    let unleased = receiver
        .open(full(None))
        .await
        .expect("opens without a lease");
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
    let strays = arriving_within(&unleased, SETTLE).await;
    assert!(
        !strays.iter().any(|e| matches!(e, SessionEvent::Direct(_))),
        "a session with no lease is sent no direct message: {strays:?}"
    );
    from.close().await.expect("closes");
    to.close().await.expect("closes");
    unleased.close().await.expect("closes");
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

/// The socket's share of a pushed binding's pipeline, in frames: the
/// kernel's send buffer is bounded in bytes, not events, so its worth is
/// the buffer over the frame size. Measured 2026-09-30 on a Linux host
/// with `net.core.wmem_default` 212992: an `AF_UNIX` socket pair holds 278
/// writes of 60-120 bytes and 167 of 300-600 (the server writes one frame
/// per write). A host with a send buffer several times larger may need a
/// larger allowance, and says so by failing this item at the cap -- loud,
/// never a false pass. An allowance, not a contract figure (`LOCAL-IPC.md`
/// §Push events and overload, A 2026-09-30).
pub const SOCKET_FRAME_ALLOWANCE: usize = 1024;

/// Items 3 and 6: acceptance follows admission, and what the receiver
/// has not drained is bounded -- past it the sender is told `Overloaded`,
/// and every message accepted before that is delivered, in order, with
/// nothing waiting in a hidden mailbox.
///
/// How many are accepted before the first `Overloaded` is the binding's
/// pipeline: the session queue in process; over IPC the queue,
/// the server's event lane, the socket and the client's buffer, since the
/// server pumps the queue onward (`LOCAL-IPC.md` §Push events and
/// overload, A 2026-09-30). So the check fills until refused, capped at
/// four times the bound plus [`SOCKET_FRAME_ALLOWANCE`] -- a cap reached is
/// a failure, not a pass -- and holds the receiver to exactly what was
/// accepted. Over IPC a fill runs to a few hundred sends, each a real
/// round trip: that is what this item costs, and why the fixture's bound
/// is the smallest it allows.
pub async fn the_queue_is_bounded_and_acceptance_follows_admission<B: DataSessionBinding>(
    sender: &B,
    receiver: &B,
    receiver_peer: &TransportIdentity,
    endpoint: &EndpointId,
) {
    let from = sender.open(full(Some(endpoint))).await.expect("leases");
    let to = receiver.open(full(Some(endpoint))).await.expect("leases");
    let bound = to.session().event_queue();
    let cap = bound * 4 + SOCKET_FRAME_ALLOWANCE;
    let destination = DirectDestination {
        peer: receiver_peer.clone(),
        endpoint: Some(endpoint.clone()),
    };
    let mut accepted = Vec::new();
    loop {
        assert!(
            accepted.len() < cap,
            "{cap} accepted with nothing drained: acceptance is not bounded"
        );
        let n = u16::try_from(accepted.len()).expect("under the cap");
        let mut id = [0_u8; 16];
        id[..2].copy_from_slice(&n.to_be_bytes());
        match from
            .send_direct(destination.clone(), MessageId::from_bytes(id), text("fill"))
            .await
        {
            Ok(_) => accepted.push(MessageId::from_bytes(id)),
            Err(TransportError::Overloaded) => break,
            Err(other) => panic!("accepted or Overloaded, got {other:?}"),
        }
    }
    assert!(
        accepted.len() >= bound,
        "the queue took at least its bound before refusing: {} of {bound}",
        accepted.len()
    );
    let mut got = receive_at_least(&to, accepted.len(), PATIENCE).await;
    got.extend(arriving_within(&to, SETTLE).await);
    let delivered: Vec<MessageId> = got
        .iter()
        .map(|event| match event {
            SessionEvent::Direct(message) => message.message_id,
            other => panic!("a direct message: {other:?}"),
        })
        .collect();
    assert_eq!(
        delivered, accepted,
        "exactly the accepted ones are delivered, in order"
    );
    from.close().await.expect("closes");
    to.close().await.expect("closes");
}

/// A bounded take: `events(max)` takes at most `max`, in the port's
/// order -- the direct messages before the broadcasts -- and what it
/// leaves stays queued for the next call, in order, so a caller that asks
/// for what it has room for loses nothing (relay seq 9709). The queue of
/// three spans two groups, so the take is bounded ACROSS them.
pub async fn a_bounded_take_leaves_the_rest_queued_in_order<B: DataSessionBinding>(
    sender: &B,
    receiver: &B,
    receiver_peer: &TransportIdentity,
    endpoint: &EndpointId,
    channel: &ChannelId,
) {
    let from = sender.open(full(Some(endpoint))).await.expect("leases");
    from.join(channel.clone()).await.expect("joins");
    // A second joined session on the receiving node shows when a
    // broadcast has reached it, without taking from the session under
    // test.
    let witness = receiver.open(full(None)).await.expect("opens");
    witness.join(channel.clone()).await.expect("joins");
    let publish = |id: u8| {
        from.broadcast(
            channel.clone(),
            BroadcastMessageV1 {
                message_id: MessageId::from_bytes([id; 16]),
                sent_at_ms: 1_786_600_000_000,
                payload: text("to the channel"),
            },
        )
    };
    // The mesh forms on its own schedule: publish until one arrives.
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut id = 0x80_u8;
    while receive(&witness, Duration::from_millis(500))
        .await
        .is_empty()
    {
        assert!(tokio::time::Instant::now() < deadline, "no mesh formed");
        id += 1;
        publish(id).await.expect("accepted locally");
    }

    let to = receiver.open(full(Some(endpoint))).await.expect("leases");
    to.join(channel.clone()).await.expect("joins");
    let destination = DirectDestination {
        peer: receiver_peer.clone(),
        endpoint: Some(endpoint.clone()),
    };
    let direct = [
        MessageId::from_bytes([1; 16]),
        MessageId::from_bytes([2; 16]),
    ];
    for message in &direct {
        // Accepted means admitted to the receiver's queue.
        from.send_direct(destination.clone(), *message, text("queued"))
            .await
            .expect("accepted");
    }
    let marked = MessageId::from_bytes([0x7f; 16]);
    publish(0x7f).await.expect("accepted locally");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let seen = receive(&witness, Duration::from_millis(500)).await;
        if seen.iter().any(|event| {
            matches!(event, SessionEvent::Broadcast(message) if message.message_id == marked)
        }) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the marked broadcast never arrived"
        );
    }

    // The runtime's state, owed at open and on any change, is a notice
    // the take may hold too: taken and counted under `max` like any
    // event, then set aside, since this item is about the messages.
    let id_of = |event: &SessionEvent| match event {
        SessionEvent::Direct(message) => Some(message.message_id),
        SessionEvent::Broadcast(message) => Some(message.message_id),
        SessionEvent::Local(LocalSessionEvent::ServerState { .. }) => None,
        other @ SessionEvent::Local(_) => panic!("a message: {other:?}"),
    };
    assert!(
        to.events(0).await.expect("answers").is_empty(),
        "max 0 takes nothing"
    );
    // One taken: a binding that delivers asynchronously may have nothing
    // yet, so the take is repeated until it yields -- and it may never
    // yield more than one.
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let first: Vec<MessageId> = loop {
        let got = to.events(1).await.expect("answers");
        assert!(got.len() <= 1, "events(1) took {}", got.len());
        let got: Vec<MessageId> = got.iter().filter_map(id_of).collect();
        if !got.is_empty() {
            break got;
        }
        assert!(tokio::time::Instant::now() < deadline, "nothing arrived");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(first, direct[..1], "one taken: the oldest direct message");
    // The rest, until the marked broadcast is among it.
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut rest: Vec<MessageId> = Vec::new();
    while !rest.contains(&marked) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the rest never came: {rest:?}"
        );
        rest.extend(
            to.events(usize::MAX)
                .await
                .expect("answers")
                .iter()
                .filter_map(id_of),
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        rest.first(),
        Some(&direct[1]),
        "the other direct message was left queued, ahead of the broadcasts: {rest:?}"
    );
    assert!(
        rest.contains(&marked),
        "the broadcast was left queued: {rest:?}"
    );
    from.close().await.expect("closes");
    witness.close().await.expect("closes");
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
        mute.events(usize::MAX).await,
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
        arriving_within(&bystander, SETTLE).await.is_empty(),
        "a session that did not join receives nothing"
    );
    from.close().await.expect("closes");
    joined.close().await.expect("closes");
    bystander.close().await.expect("closes");
}

/// An admin port opened with exactly `capabilities`.
async fn port<B: AdminBinding>(binding: &B, capabilities: &[AdminCapability]) -> B::Admin {
    binding
        .admin(capabilities.iter().copied().collect())
        .await
        .expect("a port")
}

/// Whether `events` carries the revocation of `epoch` on `endpoint`.
fn revoked(
    events: &[SessionEvent],
    endpoint: &EndpointId,
    epoch: &interweave_local_client_api::Generation,
) -> bool {
    events.contains(&SessionEvent::Local(
        LocalSessionEvent::EndpointLeaseChanged {
            endpoint: endpoint.clone(),
            revoked_epoch: epoch.clone(),
        },
    ))
}

/// Item 7's runtime half: the admin port is its own authority object --
/// built from the binding, holding no lease, refused without
/// `admin.endpoints` -- and a revocation it makes reaches the holder as
/// `EndpointLeaseChanged` naming the epoch that ended, after which the
/// holder cannot send on it.
pub async fn administration_is_a_separate_authority<B: DataSessionBinding + AdminBinding>(
    binding: &B,
    endpoint: &EndpointId,
    remote: &TransportIdentity,
) {
    let holder = binding.open(full(Some(endpoint))).await.expect("leases");
    let epoch = holder
        .session()
        .endpoint_lease()
        .expect("leased")
        .epoch
        .clone();

    let powerless = port(binding, &[AdminCapability::Shutdown]).await;
    assert_eq!(
        powerless.revoke_endpoint(endpoint.clone()).await,
        Err(TransportError::CapabilityDenied),
        "no admin.endpoints, no revocation"
    );
    let admin = port(binding, &[AdminCapability::Endpoints]).await;
    assert!(
        admin.port().endpoint_lease().is_none(),
        "an admin port holds no lease"
    );
    assert_ne!(
        admin.port().port_id(),
        holder.session().session_id(),
        "its own identity, not a session's"
    );
    admin
        .revoke_endpoint(endpoint.clone())
        .await
        .expect("revoked");

    let got = receive(&holder, Duration::from_secs(2)).await;
    assert!(
        revoked(&got, endpoint, &epoch),
        "the holder is told which epoch ended: {got:?}"
    );
    assert!(
        holder
            .send_direct(
                DirectDestination {
                    peer: remote.clone(),
                    endpoint: None,
                },
                MessageId::from_bytes([9; 16]),
                text("after revocation"),
            )
            .await
            .is_err(),
        "a revoked lease sends nothing"
    );
    holder.close().await.expect("closes");
}

/// `admin.peers.list` (`CONNECTIVITY.md` §19, A 2026-10-06): a port
/// without `admin.status` is refused; with it, the connected `remote` has
/// a row that reads connected with the `connected` outcome, no row is the
/// local peer, and no peer has two rows. Waited for, since a binding's
/// pair may still be connecting when the case starts.
pub async fn peer_rows_answer_under_admin_status<B: AdminBinding>(
    binding: &B,
    local: &TransportIdentity,
    remote: &TransportIdentity,
) {
    use interweave_local_client_api::PeerOutcome;
    let powerless = port(binding, &[AdminCapability::Endpoints]).await;
    assert_eq!(
        powerless.peers().await,
        Err(TransportError::CapabilityDenied)
    );
    let admin = port(binding, &[AdminCapability::Status]).await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    let rows = loop {
        let rows = admin.peers().await.expect("the rows");
        if rows.iter().any(|r| &r.peer == remote && r.connected) {
            break rows;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "remote never read connected: {rows:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    let row = rows
        .iter()
        .find(|r| &r.peer == remote)
        .expect("remote's row");
    assert_eq!(row.last_outcome, Some(PeerOutcome::Connected), "{row:?}");
    assert!(!rows.iter().any(|r| &r.peer == local), "{rows:?}");
    let mut peers: Vec<&TransportIdentity> = rows.iter().map(|r| &r.peer).collect();
    peers.sort();
    peers.dedup();
    assert_eq!(peers.len(), rows.len(), "one row per peer: {rows:?}");
}

/// The first half of item 11's restart check (ADR-0028 A 2026-10-07):
/// `remote`, configured and persisted, is listed -- the control that the
/// second half's absence is the revocation's -- and is revoked. The
/// runner restarts the runtime over the same state and calls
/// [`the_revocation_outlived_the_restart`].
pub async fn a_revocation_is_made_before_a_restart<B: AdminBinding>(
    binding: &B,
    remote: &TransportIdentity,
) {
    let admin = port(binding, &[AdminCapability::Trust]).await;
    let view = admin.trust().await.expect("the policy");
    assert!(
        view.allowed.contains(&TrustedPeer {
            peer: remote.clone(),
            persisted: true,
            source: TrustSource::Configured,
        }),
        "listed before the revocation: {view:?}"
    );
    admin
        .set_trust(remote.clone(), false)
        .await
        .expect("revoked");
}

/// The second half: after the runtime restarted, `remote` is still
/// revoked, and every row the policy lists persists.
pub async fn the_revocation_outlived_the_restart<B: AdminBinding>(
    binding: &B,
    remote: &TransportIdentity,
) {
    let admin = port(binding, &[AdminCapability::Trust]).await;
    let view = admin.trust().await.expect("the policy");
    assert!(!view.allows(remote), "revoked across the restart: {view:?}");
    assert!(
        view.allowed.iter().all(|row| row.persisted),
        "every row persists: {view:?}"
    );
}

/// `admin.trust` (ADR-0032, LOCAL-IPC.md; LOCAL-CLIENT.md §7 item 11): a
/// port without the capability is refused both methods; the policy reads
/// back the local peer, never among the allowed, and the connected
/// `remote` among them; allowing the local peer is refused; allowing a
/// listed peer and revoking an unlisted one succeed and change nothing
/// read back; revoking `remote` reaches EVERY open session holding
/// `events` (two here) as `PeerDisconnected` with the `policy` reason, and
/// the policy no longer lists it.
pub async fn trust_administration_revokes_as_policy<B: DataSessionBinding + AdminBinding>(
    binding: &B,
    local: &TransportIdentity,
    remote: &TransportIdentity,
) {
    let powerless = port(binding, &[AdminCapability::Endpoints]).await;
    assert_eq!(
        powerless.trust().await,
        Err(TransportError::CapabilityDenied)
    );
    assert_eq!(
        powerless.set_trust(remote.clone(), false).await,
        Err(TransportError::CapabilityDenied),
        "no admin.trust, no revocation"
    );
    let admin = port(binding, &[AdminCapability::Trust]).await;
    let view = admin.trust().await.expect("the policy");
    assert_eq!(view.local_peer.as_ref(), Some(local));
    assert!(view.allows(remote), "{view:?}");
    // Every production binding persists, and `remote` is the configured
    // peer (ADR-0028 A 2026-10-07).
    assert!(
        view.allowed.contains(&TrustedPeer {
            peer: remote.clone(),
            persisted: true,
            source: TrustSource::Configured,
        }),
        "{view:?}"
    );
    assert!(
        !view.allows(local),
        "the local peer is never among the allowed: {view:?}"
    );
    assert_eq!(
        admin.set_trust(local.clone(), true).await,
        Err(TransportError::InvalidArgument),
        "the local peer is not a remote to trust"
    );
    admin
        .set_trust(remote.clone(), true)
        .await
        .expect("a listed peer is a no-op");
    let before = sorted(admin.trust().await.expect("the policy"));
    assert_eq!(before, sorted(view.clone()), "allowing a listed peer");
    let unlisted = TransportIdentity::parse(UNLISTED_PEER).expect("a peer id");
    assert!(
        unlisted != *local && unlisted != *remote && !view.allows(&unlisted),
        "the unlisted peer is listed by nobody: {view:?}"
    );
    admin
        .set_trust(unlisted, false)
        .await
        .expect("revoking an unlisted peer is a no-op");
    assert_eq!(
        sorted(admin.trust().await.expect("the policy")),
        before,
        "revoking an unlisted peer"
    );

    // Allowing an unlisted peer lists it as administered, persisted
    // (ADR-0028 A 2026-10-07), and revoking it takes it off again.
    let unlisted = TransportIdentity::parse(UNLISTED_PEER).expect("a peer id");
    admin
        .set_trust(unlisted.clone(), true)
        .await
        .expect("an unlisted peer is allowed");
    let view = admin.trust().await.expect("the policy");
    assert!(
        view.allowed.contains(&TrustedPeer {
            peer: unlisted.clone(),
            persisted: true,
            source: TrustSource::Administered,
        }),
        "{view:?}"
    );
    admin.set_trust(unlisted, false).await.expect("and revoked");
    assert_eq!(
        sorted(admin.trust().await.expect("the policy")),
        before,
        "allowed and revoked again"
    );

    // Two sessions holding `events`: the revocation reaches EVERY open
    // session, not the first or the newest.
    let watchers = [
        binding.open(full(None)).await.expect("opens"),
        binding.open(full(None)).await.expect("opens"),
    ];
    admin
        .set_trust(remote.clone(), false)
        .await
        .expect("revoked");
    let told = SessionEvent::Local(LocalSessionEvent::PeerDisconnected {
        peer: remote.clone(),
        reason_class: "policy".into(),
    });
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut heard = [false; 2];
    while heard.contains(&false) {
        for (watcher, heard) in watchers.iter().zip(heard.iter_mut()) {
            *heard |= take_all(watcher).await.contains(&told);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no policy disconnect within {PATIENCE:?}, heard by {heard:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let view = admin.trust().await.expect("the policy");
    assert!(!view.allows(remote), "{view:?}");
    for watcher in watchers {
        watcher.close().await.expect("closes");
    }
}

/// A well-formed peer id no runner lists: the subject of "revoking an
/// unlisted peer changes nothing". The check asserts it is neither side
/// of the pair before relying on it.
const UNLISTED_PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

/// A trust view with its allowlist sorted, so two reads compare as sets:
/// "changes nothing" is about the policy, not the order a binding lists it.
fn sorted(mut view: TrustAdminView) -> TrustAdminView {
    view.allowed.sort();
    view
}

/// Disabling an endpoint revokes its live lease at once -- the holder
/// told, the epoch returned -- and NEVER rebinds it: while disabled a
/// claim is `EndpointDisabled`, and enabling it again leaves it unleased
/// until a client claims it (`LOCAL-IPC.md`: "never auto-rebinds").
pub async fn disabling_revokes_and_never_rebinds<B: DataSessionBinding + AdminBinding>(
    binding: &B,
    endpoint: &EndpointId,
) {
    let holder = binding.open(full(Some(endpoint))).await.expect("leases");
    let epoch = holder
        .session()
        .endpoint_lease()
        .expect("leased")
        .epoch
        .clone();
    let admin = port(binding, &[AdminCapability::Endpoints]).await;

    assert_eq!(
        admin.set_endpoint_enabled(endpoint.clone(), false).await,
        Ok(Some(epoch.clone())),
        "disabling returns the epoch it revoked"
    );
    let got = receive(&holder, Duration::from_secs(2)).await;
    assert!(
        revoked(&got, endpoint, &epoch),
        "the holder is told: {got:?}"
    );
    assert!(
        matches!(
            binding.open(full(Some(endpoint))).await,
            Err(TransportError::EndpointDisabled)
        ),
        "a disabled endpoint is claimed by nobody"
    );
    let row = |views: Vec<interweave_local_client_api::EndpointAdminView>| {
        views
            .into_iter()
            .find(|v| &v.endpoint == endpoint)
            .expect("the endpoint is listed")
    };
    let disabled = row(admin.leases().await.expect("listed"));
    assert!(
        !disabled.enabled && disabled.lease.is_none(),
        "{disabled:?}"
    );

    assert_eq!(
        admin.set_endpoint_enabled(endpoint.clone(), true).await,
        Ok(None),
        "enabling revokes nothing"
    );
    let enabled = row(admin.leases().await.expect("listed"));
    assert!(
        enabled.enabled && enabled.lease.is_none(),
        "enabled again and still unleased -- nothing rebound it: {enabled:?}"
    );
    assert!(
        arriving_within(&holder, SETTLE).await.is_empty(),
        "enabling owes the old holder nothing: its lease stays ended"
    );
    let next = binding
        .open(full(Some(endpoint)))
        .await
        .expect("a client's own claim succeeds");
    assert_ne!(
        next.session().endpoint_lease().expect("leased").epoch,
        epoch,
        "with a fresh epoch"
    );
    next.close().await.expect("closes");
    holder.close().await.expect("closes");
}

/// The administrative view: every configured endpoint is listed with its
/// state and live lease; `admin.status` is its own read-only authority
/// and counts the leases; a default must name an endpoint that can
/// receive, and may be cleared. `default` is the profile's configured
/// default, `other` a second enabled endpoint; both end as they began.
pub async fn the_admin_view_and_the_default_overlay<B: DataSessionBinding + AdminBinding>(
    binding: &B,
    local_peer: &TransportIdentity,
    default: &EndpointId,
    other: &EndpointId,
) {
    let endpoints_only = port(binding, &[AdminCapability::Endpoints]).await;
    assert_eq!(
        endpoints_only.status().await.map(|_| ()),
        Err(TransportError::CapabilityDenied),
        "status needs admin.status"
    );
    let reader = port(binding, &[AdminCapability::Status]).await;
    assert_eq!(
        reader.leases().await.map(|_| ()),
        Err(TransportError::CapabilityDenied),
        "admin.status reads status and nothing else"
    );
    // ...and changes nothing: each mutation is refused, and the view read
    // through a port that may read it shows none of them landed.
    let listed = endpoints_only.leases().await.expect("listed");
    assert_eq!(
        reader.set_endpoint_enabled(other.clone(), false).await,
        Err(TransportError::CapabilityDenied)
    );
    assert_eq!(
        reader.set_default_endpoint(Some(other.clone())).await,
        Err(TransportError::CapabilityDenied)
    );
    assert_eq!(
        reader.set_default_endpoint(None).await,
        Err(TransportError::CapabilityDenied)
    );
    assert_eq!(
        reader.revoke_endpoint(default.clone()).await,
        Err(TransportError::CapabilityDenied)
    );
    assert_eq!(
        reader.shutdown(Duration::from_secs(1)).await,
        Err(TransportError::CapabilityDenied)
    );
    assert_eq!(
        endpoints_only.leases().await.expect("listed"),
        listed,
        "a refused mutation changed nothing"
    );
    let before = reader.status().await.expect("status");
    assert_eq!(&before.peer, local_peer);

    let holder = binding.open(full(Some(other))).await.expect("leases");
    let after = reader.status().await.expect("status");
    assert_eq!(after.active_leases, before.active_leases + 1);

    let views = endpoints_only.leases().await.expect("listed");
    let ids: Vec<&EndpointId> = views.iter().map(|v| &v.endpoint).collect();
    assert!(ids.contains(&default) && ids.contains(&other), "{views:?}");
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "in id order: {ids:?}");
    let row = views.iter().find(|v| &v.endpoint == other).expect("listed");
    let lease = row.lease.as_ref().expect("the holder's lease is listed");
    // The holder is matched by its grant's epoch, which is on both sides
    // of every binding; `session_id` is binding-local and absent over IPC
    // (`LOCAL-CLIENT.md`, A 2026-09-30).
    assert_eq!(
        (&lease.epoch, lease.client_kind.as_str()),
        (
            &holder.session().endpoint_lease().expect("leased").epoch,
            "conformance"
        )
    );
    assert!(
        views
            .iter()
            .find(|v| &v.endpoint == default)
            .expect("listed")
            .default,
        "the configured default is marked"
    );

    let unknown = EndpointId::parse("nobody-configured").expect("valid");
    assert_eq!(
        endpoints_only.set_default_endpoint(Some(unknown)).await,
        Err(TransportError::EndpointUnknown)
    );
    endpoints_only
        .set_endpoint_enabled(other.clone(), false)
        .await
        .expect("known");
    assert_eq!(
        endpoints_only
            .set_default_endpoint(Some(other.clone()))
            .await,
        Err(TransportError::EndpointDisabled),
        "a default must be able to receive"
    );
    endpoints_only
        .set_endpoint_enabled(other.clone(), true)
        .await
        .expect("known");
    let default_of = |views: Vec<interweave_local_client_api::EndpointAdminView>| {
        views
            .into_iter()
            .filter(|v| v.default)
            .map(|v| v.endpoint)
            .collect::<Vec<_>>()
    };
    endpoints_only
        .set_default_endpoint(Some(other.clone()))
        .await
        .expect("an enabled endpoint");
    assert_eq!(
        default_of(endpoints_only.leases().await.expect("listed")),
        vec![other.clone()]
    );
    endpoints_only
        .set_default_endpoint(None)
        .await
        .expect("cleared");
    assert!(default_of(endpoints_only.leases().await.expect("listed")).is_empty());
    endpoints_only
        .set_default_endpoint(Some(default.clone()))
        .await
        .expect("restored");

    // Disabling the default clears it (the owner, 2026-09-28): a default
    // must be able to receive. Enabling it again restores nothing.
    endpoints_only
        .set_endpoint_enabled(default.clone(), false)
        .await
        .expect("known");
    assert!(default_of(endpoints_only.leases().await.expect("listed")).is_empty());
    endpoints_only
        .set_endpoint_enabled(default.clone(), true)
        .await
        .expect("known");
    assert!(
        default_of(endpoints_only.leases().await.expect("listed")).is_empty(),
        "enabling restores nothing"
    );
    endpoints_only
        .set_default_endpoint(Some(default.clone()))
        .await
        .expect("restored");
    holder.close().await.expect("closes");
}

/// `endpoints.query` is a capability with a method: without it the query
/// is refused locally; with it, a peer's advertised, leased endpoint is
/// listed with a freshness that has not run out.
pub async fn a_directory_query_needs_its_capability<B: DataSessionBinding>(
    querier: &B,
    responder: &B,
    responder_peer: &TransportIdentity,
    advertised: &EndpointId,
) {
    let listed = responder
        .open(full(Some(advertised)))
        .await
        .expect("leases the advertised endpoint");
    let without = querier.open(full(None)).await.expect("opens");
    assert!(matches!(
        without.query_endpoints(responder_peer.clone()).await,
        Err(TransportError::CapabilityDenied)
    ));
    let with = querier
        .open(
            SessionRequest::new("conformance", None, [DataCapability::EndpointsQuery])
                .expect("in bounds"),
        )
        .await
        .expect("opens");
    let directory = with
        .query_endpoints(responder_peer.clone())
        .await
        .expect("a trusted peer answers");
    assert!(
        directory.endpoints.contains(advertised),
        "{:?}",
        directory.endpoints
    );
    assert!(directory.ttl_ms > 0, "fresh from this answer");
    with.close().await.expect("closes");
    without.close().await.expect("closes");
    listed.close().await.expect("closes");
}
