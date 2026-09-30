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
    LocalSessionEvent, SessionEvent, SessionRequest,
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
        let got = session.events(usize::MAX).await.expect("events answer");
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
    let got = to.events(usize::MAX).await.expect("events answer");
    assert_eq!(got.len(), bound, "exactly the admitted ones wait: {got:?}");
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

    let id_of = |event: &SessionEvent| match event {
        SessionEvent::Direct(message) => message.message_id,
        SessionEvent::Broadcast(message) => message.message_id,
        other @ SessionEvent::Local(_) => panic!("a message: {other:?}"),
    };
    assert!(
        to.events(0).await.expect("answers").is_empty(),
        "max 0 takes nothing"
    );
    let first: Vec<MessageId> = to
        .events(1)
        .await
        .expect("answers")
        .iter()
        .map(id_of)
        .collect();
    assert_eq!(first, direct[..1], "one taken: the oldest direct message");
    let rest: Vec<MessageId> = to
        .events(usize::MAX)
        .await
        .expect("answers")
        .iter()
        .map(id_of)
        .collect();
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
        bystander
            .events(usize::MAX)
            .await
            .expect("answers")
            .is_empty(),
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
        holder.events(usize::MAX).await.expect("answers").is_empty(),
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
