// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Plan §19 P4: the in-memory fake (`tests/local-client-fake`, Stage 14)
//! drives the conversion and the reply-token routing before any daemon
//! does. Two fake nodes: a bridge's session on node A's `claude` endpoint,
//! a peer on node B. What the fake cannot prove -- that the network
//! carries it, that Noise proved the peer -- is the daemon harness's and
//! that crate's README's to say.

#![allow(clippy::expect_used, clippy::panic)]

use interweave_claude_channel_core::{BridgeState, ChannelNotification, ReplyRoute};
use interweave_local_client_api::{
    DataCapability, DataSessionBinding as _, DataSessionPort as _, SessionEvent, SessionRequest,
};
use interweave_local_client_fake::{FakeConfig, FakeEndpoint, FakeNetwork, FakeNode, FakeSession};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{
    BroadcastMessageV1, ChannelId, DirectDestination, EndpointId, MAX_PAYLOAD_BYTES, MessageId,
    Payload, TransportError,
};

const NOW: u64 = 1_791_227_222_497;

fn endpoint(s: &str) -> EndpointId {
    EndpointId::parse(s).expect("endpoint")
}

fn general() -> ChannelId {
    ChannelId::parse("general").expect("channel")
}

fn config() -> FakeConfig {
    FakeConfig {
        peer: ProfileIdentity::generate()
            .transport_identity()
            .expect("peer id"),
        endpoints: vec![
            FakeEndpoint::open(endpoint("human"), false),
            FakeEndpoint::open(endpoint("claude"), false),
        ],
        default_endpoint: Some(endpoint("human")),
        queue_bound: 8,
    }
}

async fn open(node: &FakeNode, on: &str) -> FakeSession {
    node.open(
        SessionRequest::new(
            "claude-channel",
            Some(endpoint(on)),
            [DataCapability::Events, DataCapability::Commands],
        )
        .expect("request"),
    )
    .await
    .expect("opens")
}

/// A bridge on `session`, its lease recorded as the open granted it.
fn bridge_on(session: &FakeSession) -> BridgeState {
    let mut bridge = BridgeState::new();
    let lease = session
        .session()
        .endpoint_lease()
        .expect("a leased session");
    bridge.leased(lease.endpoint.clone(), lease.epoch.clone());
    bridge
}

fn text(content: &str) -> Payload {
    Payload::new(None, content.as_bytes().to_vec(), MAX_PAYLOAD_BYTES).expect("payload")
}

fn id(n: u8) -> MessageId {
    MessageId::parse_hex(&format!("{n:032x}")).expect("id")
}

/// `key` of `n`'s meta, read the way a host reads it: from its
/// serialization.
fn meta(n: &ChannelNotification, key: &str) -> Option<String> {
    serde_json::to_value(&n.meta)
        .expect("meta serializes")
        .get(key)
        .and_then(|v| v.as_str().map(str::to_owned))
}

/// The one message waiting for `session`, as the bridge notifies it.
async fn notified(
    session: &FakeSession,
    bridge: &mut BridgeState,
    salt: u8,
) -> ChannelNotification {
    let events = session.events(usize::MAX).await.expect("events");
    let messages: Vec<&SessionEvent> = events
        .iter()
        .filter(|e| !matches!(e, SessionEvent::Local(_)))
        .collect();
    let [event] = messages.as_slice() else {
        panic!("one message, got {events:?}");
    };
    bridge
        .notification(event, [salt; 16], NOW)
        .expect("converted")
        .expect("a notification")
}

/// Incoming direct → both endpoints in `meta`; `reply` → the exact route,
/// so the peer receives it at the endpoint it sent from, from the bridge's
/// endpoint.
#[tokio::test]
async fn a_direct_message_is_notified_and_a_reply_takes_its_exact_route() {
    let (a, b) = FakeNetwork::pair(config(), config());
    let claude = open(&a, "claude").await;
    let human = open(&b, "human").await;
    let mut bridge = bridge_on(&claude);

    human
        .send_direct(
            DirectDestination {
                peer: a.peer().clone(),
                endpoint: Some(endpoint("claude")),
            },
            id(1),
            text("hello claude"),
        )
        .await
        .expect("accepted");
    let n = notified(&claude, &mut bridge, 1).await;
    assert_eq!(n.content, "hello claude");
    assert_eq!(meta(&n, "source_peer").as_deref(), Some(b.peer().as_str()));
    assert_eq!(meta(&n, "source_endpoint").as_deref(), Some("human"));
    assert_eq!(meta(&n, "destination_endpoint").as_deref(), Some("claude"));

    let token = meta(&n, "reply_token").expect("a token");
    let token = token.as_str();
    let ReplyRoute::Direct {
        remote_peer,
        remote_endpoint,
        ..
    } = bridge.reply_route(token, NOW).expect("routes")
    else {
        panic!("a direct route");
    };
    let accepted = claude
        .send_direct(
            DirectDestination {
                peer: remote_peer,
                endpoint: Some(remote_endpoint),
            },
            id(2),
            text("hello human"),
        )
        .await
        .expect("accepted");
    assert_eq!(accepted, endpoint("human"));
    let events = human.events(usize::MAX).await.expect("events");
    let Some(SessionEvent::Direct(reply)) =
        events.iter().find(|e| matches!(e, SessionEvent::Direct(_)))
    else {
        panic!("the reply arrived: {events:?}");
    };
    assert_eq!(reply.source_endpoint, endpoint("claude"), "from the lease");
    assert_eq!(reply.payload.bytes(), b"hello human");
}

/// A reconnect is a new lease epoch: a token minted before it fails
/// rather than switching routes (TOOL-SURFACE.md §Reply semantics).
#[tokio::test]
async fn a_token_from_an_earlier_lease_fails_after_reconnecting() {
    let (a, b) = FakeNetwork::pair(config(), config());
    let claude = open(&a, "claude").await;
    let human = open(&b, "human").await;
    let mut bridge = bridge_on(&claude);
    human
        .send_direct(
            DirectDestination {
                peer: a.peer().clone(),
                endpoint: Some(endpoint("claude")),
            },
            id(3),
            text("before"),
        )
        .await
        .expect("accepted");
    let n = notified(&claude, &mut bridge, 2).await;
    let token = meta(&n, "reply_token").expect("a token");
    assert!(bridge.reply_route(&token, NOW).is_ok(), "the control");

    claude.close().await.expect("closed");
    bridge.lease_lost();
    let again = open(&a, "claude").await;
    let lease = again.session().endpoint_lease().expect("leased");
    bridge.leased(lease.endpoint.clone(), lease.epoch.clone());
    assert_eq!(
        bridge.reply_route(&token, NOW),
        Err(TransportError::InvalidArgument)
    );
}

/// Broadcast join → publish → reply on the same channel; once left, the
/// token fails `ChannelNotJoined` and nothing rejoins.
#[tokio::test]
async fn a_broadcast_reply_needs_the_join_and_never_recreates_it() {
    let (a, b) = FakeNetwork::pair(config(), config());
    let claude = open(&a, "claude").await;
    let human = open(&b, "human").await;
    let mut bridge = bridge_on(&claude);
    claude.join(general()).await.expect("joined");
    bridge.joined(general());
    human.join(general()).await.expect("joined");

    human
        .broadcast(
            general(),
            BroadcastMessageV1 {
                message_id: id(4),
                sent_at_ms: NOW,
                payload: text("hi all"),
            },
        )
        .await
        .expect("accepted");
    let n = notified(&claude, &mut bridge, 3).await;
    assert_eq!(meta(&n, "channel").as_deref(), Some("general"));
    let token = meta(&n, "reply_token").expect("a token");
    assert_eq!(
        bridge.reply_route(&token, NOW),
        Ok(ReplyRoute::Broadcast { channel: general() })
    );
    claude
        .broadcast(
            general(),
            BroadcastMessageV1 {
                message_id: id(5),
                sent_at_ms: NOW,
                payload: text("hi back"),
            },
        )
        .await
        .expect("accepted for local publish");

    claude.leave(general()).await.expect("left");
    bridge.left(&general());
    assert_eq!(
        bridge.reply_route(&token, NOW),
        Err(TransportError::ChannelNotJoined)
    );
    assert_eq!(
        claude
            .broadcast(
                general(),
                BroadcastMessageV1 {
                    message_id: id(6),
                    sent_at_ms: NOW,
                    payload: text("after"),
                },
            )
            .await,
        Err(TransportError::ChannelNotJoined),
        "and the session agrees: nothing rejoined"
    );
}

/// The human and Claude endpoints of one `PeerId` are routed apart: each
/// session sees only what was sent to its own endpoint.
#[tokio::test]
async fn the_human_and_claude_endpoints_of_one_peer_are_routed_apart() {
    let (a, b) = FakeNetwork::pair(config(), config());
    let claude = open(&a, "claude").await;
    let a_human = open(&a, "human").await;
    let sender = open(&b, "human").await;
    let mut bridge = bridge_on(&claude);
    for (to, n) in [("claude", 7), ("human", 8)] {
        sender
            .send_direct(
                DirectDestination {
                    peer: a.peer().clone(),
                    endpoint: Some(endpoint(to)),
                },
                id(n),
                text(&format!("to {to}")),
            )
            .await
            .expect("accepted");
    }
    let n = notified(&claude, &mut bridge, 4).await;
    assert_eq!(n.content, "to claude");
    let events = a_human.events(usize::MAX).await.expect("events");
    let directs: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::Direct(d) => Some(d.payload.bytes().to_vec()),
            _ => None,
        })
        .collect();
    assert_eq!(directs, [b"to human".to_vec()]);
}
