// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The facade against the conformance-proven in-memory fake (plan §17
//! (2), P1): the agreed contract (relay seqs 10522, 10534, 10540), item
//! by item. The fake's network outcomes are injections (its README), so
//! these prove the facade's handling of each outcome, not that a network
//! produces it.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeSet;

use interweave_human_chat_protocol::{HumanChatV2, MessageKind};
use interweave_human_store::{HumanStore, StoreOptions};
use interweave_human_transport_client::{
    ClientConfig, ClientEvent, Connectivity, Destination, Origin, OutboundStatus, Received,
    SendError, SendProblem, SessionProblem, SessionState, TransportClient,
};
use interweave_local_client_api::{
    AdminBinding, AdminCapability, AdminPort, DataCapability, DataSessionBinding, DataSessionPort,
    SessionEvent, SessionRequest,
};
use interweave_local_client_fake::{FakeConfig, FakeEndpoint, FakeNetwork, FakeNode};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{
    ChannelId, DirectDestination, EndpointId, MediaType, MessageId, Payload, TransportError,
    TransportIdentity,
};

const LIMIT: usize = 49_152;

fn human() -> EndpointId {
    EndpointId::parse("human").expect("valid")
}

fn agent() -> EndpointId {
    EndpointId::parse("agent").expect("valid")
}

fn room() -> ChannelId {
    ChannelId::parse("room").expect("valid")
}

fn node_config() -> FakeConfig {
    FakeConfig {
        peer: ProfileIdentity::generate()
            .transport_identity()
            .expect("peer id"),
        endpoints: vec![
            FakeEndpoint::open(human(), false),
            FakeEndpoint::open(agent(), true),
        ],
        default_endpoint: Some(human()),
        queue_bound: 16,
    }
}

type Client = TransportClient<FakeNode, FakeNode>;

fn client(node: &FakeNode, endpoint: EndpointId, store: HumanStore) -> Client {
    TransportClient::new(
        node.clone(),
        node.clone(),
        store,
        ClientConfig {
            client_kind: "human-client".to_owned(),
            endpoint: Some(endpoint),
            channels: vec![room()],
            max_payload_bytes: LIMIT,
        },
        0,
    )
    .expect("pending rows read")
}

fn memory() -> HumanStore {
    HumanStore::open_in_memory(StoreOptions::default()).expect("store")
}

fn envelope(text: &str) -> HumanChatV2 {
    HumanChatV2 {
        v: 2,
        kind: MessageKind::Text,
        app_message_id: format!("{:032x}", rand_id()),
        text: text.to_owned(),
        reply_to: None,
        sent_at_ms: None,
        from_endpoint: None,
    }
}

fn rand_id() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    u128::from(NEXT.fetch_add(1, Ordering::Relaxed)) << 64 | 0xabc
}

fn to(peer: &TransportIdentity) -> Destination {
    Destination::Direct {
        peer: peer.clone(),
        endpoint: None,
    }
}

/// Every event queued so far, oldest key first.
fn events(c: &mut Client) -> Vec<ClientEvent> {
    std::iter::from_fn(|| c.next_event()).collect()
}

/// The last status reported for any row.
fn last_status(c: &mut Client) -> OutboundStatus {
    events(c)
        .into_iter()
        .filter_map(|e| match e {
            ClientEvent::Outbound(u) => Some(u.status),
            _ => None,
        })
        .next_back()
        .expect("a row was reported")
}

async fn ready(c: &mut Client, now: u64) {
    c.tick(now).await;
    assert!(
        matches!(c.session_state(), SessionState::Ready { .. }),
        "{:?}",
        c.session_state()
    );
}

/// A raw session on `node` holding `endpoint`, for the network around
/// the facade under test.
async fn raw(node: &FakeNode, endpoint: Option<EndpointId>) -> impl DataSessionPort {
    node.open(
        SessionRequest::new(
            "raw",
            endpoint,
            [DataCapability::Commands, DataCapability::Events],
        )
        .expect("bounds"),
    )
    .await
    .expect("opens")
}

async fn raw_direct_ids(session: &impl DataSessionPort) -> Vec<MessageId> {
    session
        .events(usize::MAX)
        .await
        .expect("events")
        .into_iter()
        .filter_map(|e| match e {
            SessionEvent::Direct(d) => Some(d.message_id),
            _ => None,
        })
        .collect()
}

// --- 1. outbound -----------------------------------------------------------

#[tokio::test]
async fn a_send_with_no_session_is_committed_and_goes_out_when_one_opens() {
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let mut sender = client(&a, agent(), memory());
    // Not yet ticked: no session, so no transport call can be made.
    let row = sender
        .send(to(b.peer()), &envelope("hello"), 0)
        .await
        .expect("committed");
    assert_eq!(
        sender.store_mut().pending_outbound().expect("read").len(),
        1
    );
    assert_eq!(
        last_status(&mut sender),
        OutboundStatus::Sending {
            attempts: 0,
            next_retry_at: Some(0),
            last_problem: None
        }
    );
    let _held = raw(&b, Some(human())).await;
    ready(&mut sender, 0).await;
    assert_eq!(
        last_status(&mut sender),
        OutboundStatus::Accepted { endpoint: human() }
    );
    assert!(
        sender
            .store_mut()
            .pending_outbound()
            .expect("read")
            .is_empty(),
        "the pending copy of row {row:?} went at transport-terminal"
    );
}

#[tokio::test]
async fn an_accepted_direct_message_arrives_committed_with_its_route_label() {
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let mut sender = client(&a, agent(), memory());
    let mut receiver = client(&b, human(), memory());
    ready(&mut sender, 0).await;
    ready(&mut receiver, 0).await;
    let sent = envelope("over the wire");
    sender.send(to(b.peer()), &sent, 0).await.expect("sent");
    let got = receiver.drain(16, 5).await;
    let [
        Received {
            origin,
            envelope,
            received_at,
            ..
        },
    ] = got.as_slice()
    else {
        panic!("one message: {got:?}");
    };
    assert_eq!(envelope, &sent);
    assert_eq!(*received_at, 5);
    assert_eq!(
        origin,
        &Origin::Direct {
            peer: a.peer().clone(),
            endpoint: agent()
        },
        "the asserted source endpoint, as a route label"
    );
    assert_eq!(
        receiver.store_mut().unread_inbound().expect("read").len(),
        1,
        "committed as unread before it was returned"
    );
}

#[tokio::test]
async fn a_transient_failure_retries_on_its_schedule_and_says_why() {
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let _held = raw(&b, Some(human())).await;
    let mut sender = client(&a, agent(), memory());
    ready(&mut sender, 0).await;
    a.inject_send(TransportError::PeerUnreachable);
    sender
        .send(to(b.peer()), &envelope("x"), 0)
        .await
        .expect("row");
    let OutboundStatus::Sending {
        attempts: 1,
        next_retry_at: Some(due),
        last_problem: Some(SendProblem::NoNetworkPath),
    } = last_status(&mut sender)
    else {
        panic!("retrying, with its reason");
    };
    assert!((500..=1_000).contains(&due), "the first retry: {due}");
    sender.tick(due - 1).await;
    assert!(events(&mut sender).is_empty(), "not before it is due");
    sender.tick(due).await;
    assert_eq!(
        last_status(&mut sender),
        OutboundStatus::Accepted { endpoint: human() }
    );
}

#[tokio::test]
async fn a_problem_that_needs_the_person_is_never_retried_on_a_timer() {
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let held = raw(&b, Some(human())).await;
    let mut sender = client(&a, agent(), memory());
    ready(&mut sender, 0).await;
    a.inject_send(TransportError::UnauthorizedPeer);
    let row = sender
        .send(to(b.peer()), &envelope("x"), 0)
        .await
        .expect("row");
    assert_eq!(
        last_status(&mut sender),
        OutboundStatus::NeedsAttention {
            problem: SendProblem::PeerUntrusted
        }
    );
    for now in [1_000, 60_000, 3_600_000] {
        sender.tick(now).await;
    }
    assert!(
        raw_direct_ids(&held).await.is_empty(),
        "no attempt on a timer"
    );
    assert_eq!(
        sender.store_mut().pending_outbound().expect("read").len(),
        1,
        "still pending and durable: there is no failed terminal state"
    );
    sender
        .retry(row, 3_600_001)
        .await
        .expect("the person retries");
    assert_eq!(
        last_status(&mut sender),
        OutboundStatus::Accepted { endpoint: human() }
    );
}

#[tokio::test]
async fn an_unconfirmed_send_is_retried_under_the_stored_transport_id() {
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let held = raw(&b, Some(human())).await;
    let mut sender = client(&a, agent(), memory());
    ready(&mut sender, 0).await;
    a.inject_send(TransportError::Timeout);
    sender
        .send(to(b.peer()), &envelope("x"), 0)
        .await
        .expect("row");
    let OutboundStatus::Unconfirmed {
        next_retry_at: Some(due),
        ..
    } = last_status(&mut sender)
    else {
        panic!("not confirmed, never failed");
    };
    let stored = sender.store_mut().pending_outbound().expect("read")[0].transport_message_id;
    sender.tick(due).await;
    assert_eq!(
        last_status(&mut sender),
        OutboundStatus::Accepted { endpoint: human() }
    );
    assert_eq!(
        raw_direct_ids(&held).await,
        [stored],
        "the retry went out under the id stored with the row"
    );
}

#[tokio::test]
async fn after_a_restart_a_pending_row_is_reported_once_and_resent_under_its_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("human.sqlite3");
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let held = raw(&b, Some(human())).await;
    let stored = {
        let mut sender = client(
            &a,
            agent(),
            HumanStore::open(&path, StoreOptions::default()).expect("store"),
        );
        ready(&mut sender, 0).await;
        a.inject_send(TransportError::Timeout);
        sender
            .send(to(b.peer()), &envelope("x"), 0)
            .await
            .expect("row");
        let id = sender.store_mut().pending_outbound().expect("read")[0].transport_message_id;
        sender.close().await;
        id
    };
    let mut restarted = client(
        &a,
        agent(),
        HumanStore::open(&path, StoreOptions::default()).expect("reopen"),
    );
    let reported: Vec<_> = events(&mut restarted)
        .into_iter()
        .filter(|e| matches!(e, ClientEvent::Outbound(_)))
        .collect();
    assert_eq!(reported.len(), 1, "every surviving row reported once");
    ready(&mut restarted, 10).await;
    assert_eq!(
        last_status(&mut restarted),
        OutboundStatus::Accepted { endpoint: human() }
    );
    assert_eq!(
        raw_direct_ids(&held).await,
        [stored],
        "the same transport id across the restart (schema v6)"
    );
}

#[tokio::test]
async fn a_cancel_after_an_unconfirmed_attempt_says_it_may_have_reached() {
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let _held = raw(&b, Some(human())).await;
    let mut sender = client(&a, agent(), memory());
    ready(&mut sender, 0).await;
    a.inject_send(TransportError::Timeout);
    let maybe = sender
        .send(to(b.peer()), &envelope("x"), 0)
        .await
        .expect("row");
    a.inject_send(TransportError::PeerUnreachable);
    let not = sender
        .send(to(b.peer()), &envelope("y"), 0)
        .await
        .expect("row");
    events(&mut sender);
    sender.cancel(maybe).expect("cancelled");
    sender.cancel(not).expect("cancelled");
    let statuses: Vec<_> = events(&mut sender)
        .into_iter()
        .filter_map(|e| match e {
            ClientEvent::Outbound(u) => Some((u.row, u.status)),
            _ => None,
        })
        .collect();
    assert_eq!(
        statuses,
        [
            (
                maybe,
                OutboundStatus::Cancelled {
                    may_have_reached: true
                }
            ),
            (
                not,
                OutboundStatus::Cancelled {
                    may_have_reached: false
                }
            ),
        ]
    );
    assert!(
        sender
            .store_mut()
            .pending_outbound()
            .expect("read")
            .is_empty()
    );
}

#[tokio::test]
async fn a_message_too_large_to_send_commits_nothing() {
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let mut sender = client(&a, agent(), memory());
    ready(&mut sender, 0).await;
    let huge = envelope(&"x".repeat(200_000));
    assert_eq!(
        sender.send(to(b.peer()), &huge, 0).await,
        Err(SendError::TooLarge)
    );
    assert!(
        sender
            .store_mut()
            .pending_outbound()
            .expect("read")
            .is_empty()
    );
}

#[tokio::test]
async fn a_broadcast_is_published_and_arrives_with_its_channel_and_publisher() {
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let mut sender = client(&a, agent(), memory());
    let mut receiver = client(&b, human(), memory());
    ready(&mut sender, 0).await;
    ready(&mut receiver, 0).await;
    let sent = envelope("to the room");
    sender
        .send(Destination::Broadcast(room()), &sent, 0)
        .await
        .expect("row");
    assert_eq!(last_status(&mut sender), OutboundStatus::Published);
    let got = receiver.drain(16, 1).await;
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(
        got[0].origin,
        Origin::Channel {
            channel: room(),
            publisher: a.peer().clone()
        }
    );
    assert_eq!(got[0].envelope, sent);
}

// --- 3. inbound ------------------------------------------------------------

#[tokio::test]
async fn a_malformed_inbound_is_counted_by_reason_and_never_stored() {
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let mut receiver = client(&b, human(), memory());
    ready(&mut receiver, 0).await;
    let from = raw(&a, Some(agent())).await;
    let human_chat =
        MediaType::parse("application/vnd.interweave-human-chat+json;v=2").expect("valid");
    let compressed =
        MediaType::parse("application/vnd.interweave-human-chat+json;v=2;ce=br").expect("valid");
    let cases = [
        (
            Some(MediaType::parse("text/plain").expect("valid")),
            b"hi".to_vec(),
        ),
        (None, b"{}".to_vec()),
        (Some(compressed), b"not brotli".to_vec()),
        (Some(human_chat), br#"{"v":2}"#.to_vec()),
    ];
    for (n, (media, bytes)) in cases.into_iter().enumerate() {
        from.send_direct(
            DirectDestination::to_default(b.peer().clone()),
            MessageId::from_bytes([u8::try_from(n).expect("small"); 16]),
            Payload::at_ceiling(media, bytes).expect("fits"),
        )
        .await
        .expect("accepted by the queue");
    }
    assert!(receiver.drain(16, 1).await.is_empty(), "nothing presented");
    assert!(
        receiver
            .store_mut()
            .unread_inbound()
            .expect("read")
            .is_empty(),
        "nothing stored"
    );
    let d = receiver.diagnostics();
    assert_eq!(d.malformed_unknown_media_type, 2);
    assert_eq!(d.malformed_undecodable, 1);
    assert_eq!(d.malformed_invalid_envelope, 1);
}

#[tokio::test]
async fn a_second_copy_of_a_held_message_is_not_yielded() {
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let mut receiver = client(&b, human(), memory());
    ready(&mut receiver, 0).await;
    let from = raw(&a, Some(agent())).await;
    let bytes = serde_json::to_vec(&envelope("twice")).expect("json");
    for id in [1u8, 2] {
        from.send_direct(
            DirectDestination::to_default(b.peer().clone()),
            MessageId::from_bytes([id; 16]),
            Payload::at_ceiling(
                Some(
                    MediaType::parse("application/vnd.interweave-human-chat+json;v=2")
                        .expect("valid"),
                ),
                bytes.clone(),
            )
            .expect("fits"),
        )
        .await
        .expect("accepted");
    }
    assert_eq!(receiver.drain(16, 1).await.len(), 1, "one message, not two");
    // Absorbed, not a storage failure: the session stays up and nothing
    // was counted as lost.
    assert!(matches!(
        receiver.session_state(),
        SessionState::Ready { .. }
    ));
    assert_eq!(receiver.diagnostics().dropped_unstored, 0);
}

// --- 4. session ------------------------------------------------------------

#[tokio::test]
async fn an_endpoint_in_use_is_refused_and_not_retried_until_the_person_asks() {
    let (_a, b) = FakeNetwork::pair(node_config(), node_config());
    let holder = raw(&b, Some(human())).await;
    let mut receiver = client(&b, human(), memory());
    receiver.tick(0).await;
    assert_eq!(
        receiver.session_state(),
        &SessionState::Refused {
            problem: SessionProblem::EndpointInUse
        }
    );
    for now in [1_000, 60_000] {
        receiver.tick(now).await;
    }
    assert!(
        matches!(receiver.session_state(), SessionState::Refused { .. }),
        "Reconnecting never loops on a refusal"
    );
    holder.close().await.expect("released");
    receiver.reopen(60_001);
    ready(&mut receiver, 60_001).await;
}

#[tokio::test]
async fn a_revoked_lease_is_reclaimed() {
    let (_a, b) = FakeNetwork::pair(node_config(), node_config());
    let mut receiver = client(&b, human(), memory());
    ready(&mut receiver, 0).await;
    let admin = b
        .admin(BTreeSet::from([AdminCapability::Endpoints]))
        .await
        .expect("admin");
    admin.revoke_endpoint(human()).await.expect("revoked");
    receiver.drain(16, 10).await;
    let SessionState::Reconnecting { next_at, .. } = receiver.session_state().clone() else {
        panic!("reconnecting: {:?}", receiver.session_state());
    };
    ready(&mut receiver, next_at).await;
}

#[tokio::test]
async fn a_stopped_runtime_takes_the_session_and_connectivity_to_reconnecting_and_unknown() {
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let mut sender = client(&a, agent(), memory());
    ready(&mut sender, 0).await;
    assert_eq!(
        sender.connectivity(),
        Connectivity::OnlinePartial,
        "the fake reports healthy with no verified inbound path"
    );
    a.stop();
    sender
        .send(to(b.peer()), &envelope("x"), 1)
        .await
        .expect("row");
    assert!(
        matches!(sender.session_state(), SessionState::Reconnecting { .. }),
        "{:?}",
        sender.session_state()
    );
    sender.tick(10_000).await;
    assert_eq!(
        sender.connectivity(),
        Connectivity::Unknown,
        "a status nobody can read is unknown, never offline"
    );
    assert_eq!(
        sender.store_mut().pending_outbound().expect("read").len(),
        1,
        "the row waits, durable"
    );
}

#[tokio::test]
async fn degraded_storage_refuses_a_send_and_releases_the_lease() {
    let (_a, b) = FakeNetwork::pair(node_config(), node_config());
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("human.sqlite3");
    drop(HumanStore::open(&path, StoreOptions::default()).expect("create"));
    // A quota below the file's size opens degraded (StoreOptions docs).
    let tight =
        HumanStore::open(&path, StoreOptions { max_pages: Some(1) }).expect("opens degraded");
    let mut receiver = client(&b, human(), tight);
    receiver.tick(0).await;
    // Noticed on the tick, before anything was sent: no lease is taken
    // over a store that cannot hold what it would receive.
    assert_eq!(receiver.session_state(), &SessionState::StorageDegraded);
    let other = raw(&b, Some(human())).await;
    other.close().await.expect("released");
    let refused = receiver
        .send(to(b.peer()), &envelope("x"), 1)
        .await
        .expect_err("nothing can be committed");
    assert_eq!(refused, SendError::StorageUnavailable);
    assert_eq!(receiver.session_state(), &SessionState::StorageDegraded);
    // The lease is free: another session can claim the human endpoint.
    let _other = raw(&b, Some(human())).await;
}
