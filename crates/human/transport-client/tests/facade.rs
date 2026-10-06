// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The facade against the conformance-proven in-memory fake (plan §17
//! (2), P1): the agreed contract (relay seqs 10522, 10534, 10540), item
//! by item. The fake's network outcomes are injections (its README), so
//! these prove the facade's handling of each outcome, not that a network
//! produces it.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use interweave_human_chat_protocol::{HumanChatV2, MessageKind};
use interweave_human_store::{HumanStore, StoreOptions};
use interweave_human_transport_client::{
    ClientConfig, ClientEvent, Connectivity, Destination, Origin, OutboundStatus, Received,
    SendError, SendProblem, SessionProblem, SessionState, TransportClient, TrustList, TrustProblem,
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
        wall(),
        0,
    )
    .expect("pending rows read")
}

/// A wall clock fixed at a known Unix time, so persisted fields are
/// checkable.
const WALL_MS: u64 = 1_786_600_000_000;

fn wall() -> interweave_human_transport_client::WallClock {
    Box::new(|| WALL_MS)
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
    assert_eq!(
        *received_at, WALL_MS,
        "the wall clock, the order it is shown in"
    );
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
    // Busy: the remote answered and did not take it, so nothing may have
    // reached it and the row is still plainly sending.
    a.inject_send(TransportError::Overloaded);
    sender
        .send(to(b.peer()), &envelope("x"), 0)
        .await
        .expect("row");
    let OutboundStatus::Sending {
        attempts: 1,
        next_retry_at: Some(due),
        last_problem: Some(SendProblem::Busy),
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
            problem: SendProblem::PeerUntrusted,
            may_have_reached: false,
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
    a.inject_send(TransportError::Overloaded);
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

// --- the review's findings (#167) ------------------------------------------

#[tokio::test]
async fn an_unreachable_or_lost_attempt_may_have_reached_and_never_goes_back() {
    // A1: PeerUnreachable and a connection lost mid-call can follow a
    // request that left, so the row is unconfirmed -- and stays so after
    // a later failure that says "not taken" (A1a).
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let _held = raw(&b, Some(human())).await;
    let mut sender = client(&a, agent(), memory());
    ready(&mut sender, 0).await;
    a.inject_send(TransportError::PeerUnreachable);
    let row = sender
        .send(to(b.peer()), &envelope("x"), 0)
        .await
        .expect("row");
    let OutboundStatus::Unconfirmed {
        next_retry_at: Some(due),
        last_problem: Some(SendProblem::NoNetworkPath),
    } = last_status(&mut sender)
    else {
        panic!("not confirmed, with its reason");
    };
    a.inject_send(TransportError::Overloaded);
    sender.tick(due).await;
    assert!(
        matches!(last_status(&mut sender), OutboundStatus::Unconfirmed { .. }),
        "a later Busy does not make it Sending again"
    );
    sender.cancel(row).expect("cancelled");
    assert_eq!(
        last_status(&mut sender),
        OutboundStatus::Cancelled {
            may_have_reached: true
        }
    );
}

#[tokio::test]
async fn a_send_this_client_cannot_make_is_refused_with_no_row() {
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let mut sender = client(&a, agent(), memory());
    ready(&mut sender, 0).await;
    let elsewhere = ChannelId::parse("elsewhere").expect("valid");
    assert_eq!(
        sender
            .send(Destination::Broadcast(elsewhere), &envelope("x"), 0)
            .await,
        Err(SendError::NotConfigured)
    );
    let mut no_endpoint = TransportClient::new(
        a.clone(),
        a.clone(),
        memory(),
        ClientConfig {
            client_kind: "human-client".to_owned(),
            endpoint: None,
            channels: vec![room()],
            max_payload_bytes: LIMIT,
        },
        wall(),
        0,
    )
    .expect("client");
    ready(&mut no_endpoint, 0).await;
    assert_eq!(
        no_endpoint.send(to(b.peer()), &envelope("y"), 0).await,
        Err(SendError::NotConfigured)
    );
    assert!(
        sender
            .store_mut()
            .pending_outbound()
            .expect("read")
            .is_empty()
    );
    assert!(
        no_endpoint
            .store_mut()
            .pending_outbound()
            .expect("read")
            .is_empty()
    );
}

#[tokio::test]
async fn a_restarted_row_the_config_no_longer_allows_needs_attention_and_keeps_the_session() {
    // P2-1: such a row used to bounce the session forever, dropping inbound
    // that was already accepted with every close.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("human.sqlite3");
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    {
        let mut before = client(
            &a,
            human(),
            HumanStore::open(&path, StoreOptions::default()).expect("store"),
        );
        // Not ticked: committed, never attempted.
        before
            .send(Destination::Broadcast(room()), &envelope("to the room"), 0)
            .await
            .expect("row");
    }
    let mut after = TransportClient::new(
        a.clone(),
        a.clone(),
        HumanStore::open(&path, StoreOptions::default()).expect("reopen"),
        ClientConfig {
            client_kind: "human-client".to_owned(),
            endpoint: Some(human()),
            channels: vec![],
            max_payload_bytes: LIMIT,
        },
        wall(),
        0,
    )
    .expect("client");
    // Inbound already accepted for this client, before the row is tried.
    let from = raw(&b, Some(agent())).await;
    ready(&mut after, 0).await;
    from.send_direct(
        DirectDestination::to_default(a.peer().clone()),
        MessageId::from_bytes([9; 16]),
        Payload::at_ceiling(
            Some(
                MediaType::parse("application/vnd.interweave-human-chat+json;v=2").expect("valid"),
            ),
            serde_json::to_vec(&envelope("for you")).expect("json"),
        )
        .expect("fits"),
    )
    .await
    .expect("accepted");
    after.tick(10).await;
    assert_eq!(
        last_status(&mut after),
        OutboundStatus::NeedsAttention {
            problem: SendProblem::NotConfigured,
            may_have_reached: false,
        }
    );
    assert!(
        matches!(after.session_state(), SessionState::Ready { .. }),
        "the session is not touched"
    );
    assert_eq!(
        after.drain(16, 20).await.len(),
        1,
        "the accepted inbound is kept"
    );
}

#[tokio::test]
async fn a_lease_loss_hands_over_everything_already_accepted() {
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let mut receiver = client(&b, human(), memory());
    let mut sender = client(&a, agent(), memory());
    ready(&mut receiver, 0).await;
    ready(&mut sender, 0).await;
    for n in 0..3 {
        sender
            .send(
                Destination::Broadcast(room()),
                &envelope(&format!("b{n}")),
                0,
            )
            .await
            .expect("published");
    }
    let admin = b
        .admin(BTreeSet::from([AdminCapability::Endpoints]))
        .await
        .expect("admin");
    admin.revoke_endpoint(human()).await.expect("revoked");
    // The ServerState owed at open (LOCAL-CLIENT.md, #175) is taken
    // first: it takes a slot and yields no message.
    assert!(receiver.drain(1, 1).await.is_empty(), "the open-time state");
    // One at a time: the lease notice comes first, the three broadcasts
    // after it, and the facade must take them all before it closes.
    let mut got = Vec::new();
    for t in 1..=4 {
        got.extend(receiver.drain(1, t).await);
    }
    assert_eq!(got.len(), 3, "{got:?}");
    assert!(matches!(
        receiver.session_state(),
        SessionState::Reconnecting { .. }
    ));
}

/// A store with room for ONE 40 KiB body and the recheck's 48 KiB probe
/// once that body is gone, but not for two bodies: SQLite's own
/// `SQLITE_FULL`, not an injected failure.
fn store_with_room_for_one(dir: &std::path::Path) -> HumanStore {
    let path = dir.join("state").join("human.sqlite3");
    drop(HumanStore::open(&path, StoreOptions::default()).expect("create"));
    let pages: u32 = rusqlite::Connection::open(&path)
        .expect("raw")
        .query_row("PRAGMA page_count", [], |r| r.get(0))
        .expect("pages");
    HumanStore::open(
        &path,
        StoreOptions {
            max_pages: Some(pages + 16),
        },
    )
    .expect("opens healthy")
}

fn body(text: &str) -> HumanChatV2 {
    envelope(&format!("{text}{}", "z".repeat(40_000)))
}

async fn deliver(from: &impl DataSessionPort, to: &TransportIdentity, id: u8, text: &str) {
    from.send_direct(
        DirectDestination::to_default(to.clone()),
        MessageId::from_bytes([id; 16]),
        Payload::at_ceiling(
            Some(
                MediaType::parse("application/vnd.interweave-human-chat+json;v=2").expect("valid"),
            ),
            serde_json::to_vec(&body(text)).expect("json"),
        )
        .expect("fits"),
    )
    .await
    .expect("accepted by the queue");
}

#[tokio::test]
async fn storage_failing_mid_session_releases_the_held_lease_and_a_recheck_restores_it() {
    // P2-3: a READY session, holding its lease, whose inbound commit meets
    // a full store.
    let dir = tempfile::tempdir().expect("tempdir");
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let mut receiver = client(&b, human(), store_with_room_for_one(dir.path()));
    ready(&mut receiver, 0).await;
    let from = raw(&a, Some(agent())).await;
    deliver(&from, b.peer(), 1, "first").await;
    let first = receiver.drain(16, 1).await;
    assert_eq!(first.len(), 1, "the first fits");
    deliver(&from, b.peer(), 2, "second").await;
    assert!(
        receiver.drain(16, 2).await.is_empty(),
        "never handed over unstored"
    );
    assert_eq!(receiver.session_state(), &SessionState::StorageDegraded);
    assert_eq!(receiver.diagnostics().dropped_unstored, 1);
    // Released: another session can claim the endpoint now.
    let other = raw(&b, Some(human())).await;
    // Still degraded on a recheck while nothing is freed: the probe is a
    // full-size durable write (HumanStore::recheck_health).
    receiver.recheck(3);
    assert_eq!(receiver.session_state(), &SessionState::StorageDegraded);
    // The person reads the first, which frees its pages.
    receiver
        .store_mut()
        .mark_read(first[0].row, 4)
        .expect("read");
    other.close().await.expect("released");
    receiver.recheck(5);
    assert!(
        matches!(receiver.session_state(), SessionState::Reconnecting { .. }),
        "{:?}",
        receiver.session_state()
    );
    ready(&mut receiver, 6).await;
}

#[tokio::test]
async fn a_degrade_returns_to_refused_and_never_leaves_closed() {
    let (_a, b) = FakeNetwork::pair(node_config(), node_config());
    let holder = raw(&b, Some(human())).await;
    let dir = tempfile::tempdir().expect("tempdir");
    let mut refused = client(&b, human(), store_with_room_for_one(dir.path()));
    refused.tick(0).await;
    assert!(matches!(
        refused.session_state(),
        SessionState::Refused { .. }
    ));
    let kept = refused
        .send(Destination::Broadcast(room()), &body("one"), 1)
        .await
        .expect("the first fits");
    assert_eq!(
        refused
            .send(Destination::Broadcast(room()), &body("two"), 1)
            .await,
        Err(SendError::StorageUnavailable)
    );
    assert_eq!(refused.session_state(), &SessionState::StorageDegraded);
    refused.cancel(kept).expect("frees its pages");
    refused.recheck(2);
    assert_eq!(
        refused.session_state(),
        &SessionState::Refused {
            problem: SessionProblem::EndpointInUse
        },
        "back to Refused, not re-opening on a timer"
    );
    drop(refused);
    holder.close().await.expect("released");

    let dir = tempfile::tempdir().expect("tempdir");
    let mut closed = client(&b, human(), store_with_room_for_one(dir.path()));
    ready(&mut closed, 0).await;
    closed.close().await;
    closed
        .send(Destination::Broadcast(room()), &body("one"), 1)
        .await
        .expect("committed, never sent");
    assert_eq!(
        closed
            .send(Destination::Broadcast(room()), &body("two"), 1)
            .await,
        Err(SendError::StorageUnavailable)
    );
    closed.tick(100_000).await;
    assert_eq!(closed.session_state(), &SessionState::Closed);
}

#[tokio::test]
async fn the_transport_id_is_never_the_application_id_and_times_are_wall_clock() {
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let held = raw(&b, Some(human())).await;
    let mut sender = client(&a, agent(), memory());
    ready(&mut sender, 0).await;
    let sent = envelope("x");
    a.inject_send(TransportError::Overloaded);
    sender.send(to(b.peer()), &sent, 7).await.expect("row");
    let pending = &sender.store_mut().pending_outbound().expect("read")[0];
    assert_eq!(pending.created_at, WALL_MS, "persisted on the wall clock");
    assert_eq!(pending.last_attempt_at, Some(WALL_MS));
    sender.tick(10_000).await;
    let ids = raw_direct_ids(&held).await;
    assert_eq!(ids.len(), 1);
    assert_ne!(
        ids[0],
        MessageId::parse_hex(&sent.app_message_id).expect("32 hex"),
        "the dedup identity is not the application identity (HUMAN-CHAT.md:38)"
    );
}

#[tokio::test]
async fn an_envelope_a_receiver_would_discard_is_refused_with_no_row() {
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let mut sender = client(&a, agent(), memory());
    ready(&mut sender, 0).await;
    let mut bad = envelope("x");
    bad.v = 3;
    assert_eq!(
        sender.send(to(b.peer()), &bad, 0).await,
        Err(SendError::InvalidEnvelope)
    );
    let mut late = envelope("y");
    late.sent_at_ms = Some(u64::MAX);
    assert_eq!(
        sender.send(to(b.peer()), &late, 0).await,
        Err(SendError::InvalidEnvelope)
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
async fn a_session_closed_after_a_lost_connection_keeps_what_it_had_accepted() {
    // TRANSPORT.md: a REMOTE shutdown reaches the caller as
    // BackendUnavailable, with this session healthy and holding accepted
    // inbound. The re-open must not drop it.
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let mut client_b = client(&b, human(), memory());
    ready(&mut client_b, 0).await;
    let from = raw(&a, Some(agent())).await;
    deliver(&from, b.peer(), 4, "accepted before the close").await;
    b.inject_send(TransportError::BackendUnavailable);
    client_b
        .send(to(a.peer()), &envelope("x"), 1)
        .await
        .expect("row");
    assert!(matches!(
        client_b.session_state(),
        SessionState::Reconnecting { .. }
    ));
    assert_eq!(
        client_b.drain(16, 2).await.len(),
        1,
        "committed before the close, handed over after it"
    );
}

#[tokio::test]
async fn a_send_after_the_lease_was_revoked_is_retried_once_the_lease_is_reclaimed() {
    // A transport EndpointNotRegistered from a configuration that HAS an
    // endpoint means the session's lease went: re-claim and retry, never
    // "not configured" (#167 re-review N1).
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let held = raw(&b, Some(human())).await;
    let mut sender = client(&a, agent(), memory());
    ready(&mut sender, 0).await;
    let admin = a
        .admin(BTreeSet::from([AdminCapability::Endpoints]))
        .await
        .expect("admin");
    admin.revoke_endpoint(agent()).await.expect("revoked");
    // Sent before any drain has read the revocation notice.
    sender
        .send(to(b.peer()), &envelope("x"), 1)
        .await
        .expect("row");
    assert!(
        matches!(
            last_status(&mut sender),
            OutboundStatus::Sending { .. } | OutboundStatus::Unconfirmed { .. }
        ),
        "retried on its own, not parked for the person"
    );
    sender.drain(16, 2).await;
    for now in [10_000, 20_000, 40_000] {
        sender.tick(now).await;
    }
    assert_eq!(
        raw_direct_ids(&held).await.len(),
        1,
        "delivered after the re-claim"
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
async fn a_pending_row_that_no_longer_reads_is_counted_not_silently_stalled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("human.sqlite3");
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let _held = raw(&b, Some(human())).await;
    let mut sender = client(
        &a,
        agent(),
        HumanStore::open(&path, StoreOptions::default()).expect("store"),
    );
    ready(&mut sender, 0).await;
    a.inject_send(TransportError::Overloaded);
    sender
        .send(to(b.peer()), &envelope("x"), 0)
        .await
        .expect("row");
    // Corrupted on disk behind the facade's back.
    rusqlite::Connection::open(&path)
        .expect("raw")
        .execute(
            "UPDATE pending_outbound SET destination_peer = 'not a peer id'",
            [],
        )
        .expect("corrupt");
    assert_eq!(sender.diagnostics().pending_unreadable, 0);
    sender.tick(10_000).await;
    assert_eq!(sender.diagnostics().pending_unreadable, 1, "counted");
}

/// Every unread row, a page at a time.
fn unread_rows(store: &mut HumanStore) -> usize {
    let mut n = 0;
    let mut after = None;
    loop {
        let page = store
            .unread_inbound_page(after, interweave_human_store::PageLimits::default())
            .expect("page");
        n += page.items.len();
        match page.next {
            Some(next) => after = Some(next),
            None => return n,
        }
    }
}

#[tokio::test]
async fn a_snapshot_past_the_cap_is_kept_in_the_store_and_announced() {
    // One session queue's worth is handed over through drain; the rest of
    // the snapshot stays unread in the store, is counted, and the caller
    // is told to re-list (#167 re-review F1, F2; amendment A5).
    let cap = interweave_local_client_api::MAX_EVENT_QUEUE;
    let (a, b) = FakeNetwork::pair(
        node_config(),
        FakeConfig {
            queue_bound: cap,
            ..node_config()
        },
    );
    let mut receiver = client(&b, human(), memory());
    ready(&mut receiver, 0).await;
    let from = raw(&a, Some(agent())).await;
    from.join(room()).await.expect("joined");
    let media = MediaType::parse("application/vnd.interweave-human-chat+json;v=2").expect("valid");
    for n in 0..cap {
        let id = u32::try_from(n).expect("small").to_be_bytes();
        let bytes = serde_json::to_vec(&envelope("d")).expect("json");
        from.send_direct(
            DirectDestination::to_default(b.peer().clone()),
            MessageId::from_bytes([
                id[0], id[1], id[2], id[3], 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ]),
            Payload::at_ceiling(Some(media.clone()), bytes).expect("fits"),
        )
        .await
        .expect("direct queued");
        let bytes = serde_json::to_vec(&envelope("b")).expect("json");
        from.broadcast(
            room(),
            interweave_transport_api::BroadcastMessageV1 {
                message_id: MessageId::from_bytes([
                    id[0], id[1], id[2], id[3], 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                ]),
                sent_at_ms: 0,
                payload: Payload::at_ceiling(Some(media.clone()), bytes).expect("fits"),
            },
        )
        .await
        .expect("broadcast published");
    }
    events(&mut receiver);
    // A lost connection: the facade closes the session, taking its
    // snapshot first.
    b.inject_send(TransportError::BackendUnavailable);
    receiver
        .send(to(a.peer()), &envelope("x"), 1)
        .await
        .expect("row");
    let overflow = u64::try_from(cap).expect("fits");
    assert_eq!(receiver.diagnostics().held_overflow, overflow);
    assert!(
        events(&mut receiver).contains(&ClientEvent::UnreadInStore {
            not_handed_over: overflow
        }),
        "the caller is told to re-list"
    );
    assert_eq!(
        receiver.drain(usize::MAX, 2).await.len(),
        cap,
        "exactly the cap"
    );
    assert!(receiver.drain(usize::MAX, 3).await.is_empty());
    assert_eq!(
        unread_rows(receiver.store_mut()),
        2 * cap,
        "every message is unread in the store, the overflow included"
    );

    // A5 rule 3: cumulative over the facade's life, never reset by a
    // re-open. Re-open, then overflow once more by one.
    receiver.tick(100_000).await;
    assert!(matches!(
        receiver.session_state(),
        SessionState::Ready { .. }
    ));
    for n in 0..cap {
        let id = u32::try_from(n).expect("small").to_be_bytes();
        from.send_direct(
            DirectDestination::to_default(b.peer().clone()),
            MessageId::from_bytes([
                id[0], id[1], id[2], id[3], 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ]),
            Payload::at_ceiling(
                Some(media.clone()),
                serde_json::to_vec(&envelope("again")).expect("json"),
            )
            .expect("fits"),
        )
        .await
        .expect("direct queued");
    }
    // The one row past the cap: the direct queue is full, so it goes as a
    // broadcast.
    from.broadcast(
        room(),
        interweave_transport_api::BroadcastMessageV1 {
            message_id: MessageId::from_bytes([9; 16]),
            sent_at_ms: 0,
            payload: Payload::at_ceiling(
                Some(media.clone()),
                serde_json::to_vec(&envelope("one more")).expect("json"),
            )
            .expect("fits"),
        },
    )
    .await
    .expect("published");
    events(&mut receiver);
    b.inject_send(TransportError::BackendUnavailable);
    receiver
        .send(to(a.peer()), &envelope("y"), 100_001)
        .await
        .expect("row");
    let total = overflow + 1;
    assert_eq!(receiver.diagnostics().held_overflow, total);
    assert!(
        events(&mut receiver).contains(&ClientEvent::UnreadInStore {
            not_handed_over: total
        }),
        "the count went on from {overflow}, not from one"
    );
}

#[tokio::test]
async fn a_close_whose_take_meets_a_full_store_degrades_rather_than_reconnects() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let mut receiver = client(&b, human(), store_with_room_for_one(dir.path()));
    ready(&mut receiver, 0).await;
    let from = raw(&a, Some(agent())).await;
    deliver(&from, b.peer(), 1, "first").await;
    deliver(&from, b.peer(), 2, "second").await;
    // Neither drained: the close's take commits the first and meets a
    // full store on the second.
    b.inject_send(TransportError::BackendUnavailable);
    receiver
        .send(to(a.peer()), &envelope("x"), 1)
        .await
        .expect("a small row fits");
    assert_eq!(
        receiver.session_state(),
        &SessionState::StorageDegraded,
        "degraded, not merely reconnecting"
    );
}

#[tokio::test]
async fn a_fabricated_row_id_is_refused_by_retry_and_cancel() {
    let (a, _b) = FakeNetwork::pair(node_config(), node_config());
    let mut sender = client(&a, agent(), memory());
    let made_up = interweave_human_core::RowId::from_stored(4_242);
    assert_eq!(
        sender.retry(made_up, 0).await,
        Err(interweave_human_transport_client::RowError::NoSuchRow)
    );
    assert_eq!(
        sender.cancel(made_up),
        Err(interweave_human_transport_client::RowError::NoSuchRow)
    );
}

/// The runtime's pushed state reaches the connectivity indicator at the
/// next drain, with no admin status asked: the runtime stopping reads as
/// offline at once.
#[tokio::test]
async fn the_runtimes_pushed_state_reaches_connectivity_without_the_admin_port() {
    let (a, _b) = FakeNetwork::pair(node_config(), node_config());
    let mut c = client(&a, human(), memory());
    ready(&mut c, 0).await;
    let _ = c.drain(16, 1).await;
    let _ = events(&mut c);
    a.set_health(interweave_transport_api::Health::Unavailable);
    // Drained at a time before the admin port is asked again.
    let _ = c.drain(16, 2).await;
    assert!(
        events(&mut c).contains(&ClientEvent::Connectivity(Connectivity::Offline)),
        "offline, from the pushed state"
    );
    assert_eq!(c.connectivity(), Connectivity::Offline);
}

/// A path change to a peer this session has a route to is one route
/// indicator event, its newest path: no message, no disconnection.
#[tokio::test]
async fn a_path_change_is_the_peers_newest_path_and_nothing_else() {
    use interweave_transport_api::PeerPath;
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let mut sender = client(&a, agent(), memory());
    let mut receiver = client(&b, human(), memory());
    ready(&mut sender, 0).await;
    ready(&mut receiver, 0).await;
    sender
        .send(to(b.peer()), &envelope("a route"), 0)
        .await
        .expect("sent");
    assert_eq!(receiver.drain(16, 1).await.len(), 1, "the route is there");
    let _ = events(&mut receiver);
    b.path_changed(a.peer(), PeerPath::Relayed, PeerPath::Direct, "dcutr", 5);
    b.path_changed(
        a.peer(),
        PeerPath::Direct,
        PeerPath::Relayed,
        "direct_lost",
        6,
    );
    b.path_changed(a.peer(), PeerPath::Relayed, PeerPath::Direct, "dcutr", 7);
    let got = receiver.drain(16, 2).await;
    assert!(got.is_empty(), "no message: {got:?}");
    let raised = events(&mut receiver);
    assert_eq!(
        raised,
        [ClientEvent::PeerPath {
            peer: a.peer().clone(),
            path: PeerPath::Direct,
        }],
        "one event, the newest path"
    );
}

/// An admin binding over a fake node that records the capabilities each
/// connection asked for: what a trust call holds, said by the port it
/// opened rather than by the facade's word.
#[derive(Clone)]
struct Recording {
    node: FakeNode,
    asked: Arc<Mutex<Vec<BTreeSet<AdminCapability>>>>,
}

impl AdminBinding for Recording {
    type Admin = <FakeNode as AdminBinding>::Admin;

    fn admin(
        &self,
        capabilities: BTreeSet<AdminCapability>,
    ) -> impl std::future::Future<Output = Result<Self::Admin, TransportError>> + Send {
        self.asked
            .lock()
            .expect("the record")
            .push(capabilities.clone());
        let node = self.node.clone();
        async move { node.admin(capabilities).await }
    }
}

fn trusting(node: &FakeNode) -> (TransportClient<FakeNode, Recording>, Recording) {
    let recording = Recording {
        node: node.clone(),
        asked: Arc::default(),
    };
    let client = TransportClient::new(
        node.clone(),
        recording.clone(),
        memory(),
        ClientConfig {
            client_kind: "human-client".to_owned(),
            endpoint: Some(human()),
            channels: Vec::new(),
            max_payload_bytes: LIMIT,
        },
        wall(),
        0,
    )
    .expect("an empty store");
    (client, recording)
}

/// The allowlist as the daemon holds it, this profile's own identity
/// beside it; a change is read back, and a revocation reaches an open
/// session as the peer's disconnection (`LOCAL-CLIENT.md` section 7 item 11).
#[tokio::test]
async fn trust_is_read_allowed_and_revoked_and_read_back() {
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let (settings, _) = trusting(&a);
    let mut session = client(&a, human(), memory());
    ready(&mut session, 0).await;
    let _ = events(&mut session);

    let read = settings.trust().await.expect("the allowlist");
    assert_eq!(
        read,
        TrustList {
            local_peer: Some(a.peer().clone()),
            allowed: vec![b.peer().clone()],
        }
    );

    let stranger = ProfileIdentity::generate()
        .transport_identity()
        .expect("a peer");
    let after = settings
        .set_trust(stranger.clone(), true)
        .await
        .expect("allowed");
    assert!(
        after.allowed.contains(&stranger),
        "read back with it: {after:?}"
    );

    let after = settings
        .set_trust(b.peer().clone(), false)
        .await
        .expect("revoked");
    assert!(
        !after.allowed.contains(b.peer()),
        "read back without it: {after:?}"
    );
    let _ = session.drain(16, 1).await;
    assert!(
        events(&mut session).contains(&ClientEvent::PeerDisconnected {
            peer: b.peer().clone()
        }),
        "the open session was told"
    );
}

/// This profile's own identity is never a peer to trust: the daemon's
/// refusal is one the person can act on, and nothing changed.
#[tokio::test]
async fn trusting_this_profiles_own_identity_is_refused() {
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let (settings, _) = trusting(&a);
    assert_eq!(
        settings.set_trust(a.peer().clone(), true).await,
        Err(TrustProblem::Refused)
    );
    assert_eq!(
        settings.trust().await.expect("the allowlist").allowed,
        vec![b.peer().clone()]
    );
}

/// Every connection a trust call opens holds `admin.trust` and nothing
/// else, and one is opened per call: no trust authority is held between
/// a person's settings actions, and none rides the status connection.
#[tokio::test]
async fn a_trust_call_opens_a_connection_holding_admin_trust_alone() {
    let (a, b) = FakeNetwork::pair(node_config(), node_config());
    let (mut settings, recording) = trusting(&a);
    let _ = settings.trust().await.expect("the allowlist");
    let _ = settings
        .set_trust(b.peer().clone(), true)
        .await
        .expect("a no-op allow");
    let trust_only = BTreeSet::from([AdminCapability::Trust]);
    assert_eq!(
        *recording.asked.lock().expect("the record"),
        vec![trust_only.clone(), trust_only],
        "one connection per call, each holding admin.trust alone"
    );
    // Control: the facade's own status read asks for status alone.
    settings.tick(0).await;
    assert_eq!(
        recording.asked.lock().expect("the record").last(),
        Some(&BTreeSet::from([AdminCapability::Status])),
    );
}
