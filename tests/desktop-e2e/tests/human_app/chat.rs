// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Plan §18's exit gate (b): the shipped binary re-runs the two-daemon
//! `HumanChatV2` proof -- direct and broadcast, plain and compressed, in
//! both directions -- the send half seeded into the sending binary's store,
//! the receive half observed in the receiving binary's: each message
//! committed unread there, on the channel or not as it was sent, and
//! compressed on the wire exactly when it did not fit plain.
//!
//! A broadcast is published once and is not retried, so the receiver
//! joins before the sender starts: its daemon has then told the sender's
//! of the subscription, and the publication reaches it. What this does
//! not prove: delivery to a channel member that joins later, which no
//! broadcast promises.

use interweave_human_chat_protocol::HumanChatV2;
use interweave_human_store::OutboundDestination;
use interweave_transport_api::{
    ChannelId, DirectDestination, MAX_PAYLOAD_BYTES, TransportIdentity,
};

use crate::common::{Home, human};
use crate::harness::{self as app, App, until_lease};
use crate::world::{
    World, envelope, seed_pending_to, two_daemons_joining, unread_rows, until_rows,
};

fn general() -> ChannelId {
    ChannelId::parse("general").expect("a channel id")
}

/// A message larger than the payload limit plain: only the compressed
/// form (`;ce=br`) can carry it.
fn large(serial: u64, from: &str) -> HumanChatV2 {
    let line = format!("\n- {from} repeats this line so brotli has something to fold");
    let mut text = format!("a long message from {from}");
    while text.len() <= MAX_PAYLOAD_BYTES + 4096 {
        text.push_str(&line);
    }
    envelope(serial, &text)
}

/// The four a side sends: direct and broadcast, plain and compressed.
/// The second of each pair is the compressed one.
struct Sent {
    direct: [HumanChatV2; 2],
    broadcast: [HumanChatV2; 2],
}

fn seed(home: &Home, to: &TransportIdentity, first: u64, from: &str) -> Sent {
    let sent = Sent {
        direct: [
            envelope(first, &format!("direct from {from}")),
            large(first + 1, from),
        ],
        broadcast: [
            envelope(first + 2, &format!("broadcast from {from}")),
            large(first + 3, from),
        ],
    };
    for message in &sent.direct {
        seed_pending_to(
            home,
            OutboundDestination::Direct(DirectDestination {
                peer: to.clone(),
                endpoint: Some(human()),
            }),
            message,
        );
    }
    for message in &sent.broadcast {
        seed_pending_to(home, OutboundDestination::Broadcast(general()), message);
    }
    sent
}

/// The receiving binary committed each of `sent` unread, from `from`:
/// the direct ones on no channel, the broadcasts on `general`, and the
/// large ones -- and only those -- compressed on the wire.
async fn received(world: &World, at: &Home, sent: &Sent, from: &TransportIdentity, app: &App) {
    until_rows(at, "unread_inbound", 4, || {
        format!("{}\n{}", app.log(), world.logs())
    })
    .await;
    let rows = unread_rows(at);
    let row = |message: &HumanChatV2| {
        rows.iter()
            .find(|r| r.app_message_id == message.app_message_id)
            .unwrap_or_else(|| panic!("{} not received: {rows:?}", message.app_message_id))
            .clone()
    };
    for (messages, channel) in [
        (&sent.direct, None),
        (&sent.broadcast, Some(general().as_str().to_owned())),
    ] {
        for (index, message) in messages.iter().enumerate() {
            let got = row(message);
            assert_eq!(got.source_peer, from.as_str(), "{got:?}");
            assert_eq!(got.channel_id, channel, "{got:?}");
            let compressed = got
                .media_type
                .as_deref()
                .is_some_and(|m| m.ends_with(";ce=br"));
            assert_eq!(
                compressed,
                index == 1,
                "compressed exactly when large: {got:?}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_shipped_binary_carries_human_chat_direct_and_broadcast_plain_and_compressed() {
    let world = two_daemons_joining(Some(&general())).await;

    // A to B: B's binary joined first, then A's starts with its four.
    let mut b = app::start(&world.b);
    until_lease(&world.b.binding(), true, &b).await;
    let from_a = seed(&world.a, &world.b_peer, 0xa0, "A");
    let mut a = app::start(&world.a);
    until_lease(&world.a.binding(), true, &a).await;
    received(&world, &world.b, &from_a, &world.a_peer, &b).await;
    until_rows(&world.a, "pending_outbound", 0, || a.log()).await;

    // B to A: A's binary is still joined; B's comes back with its four.
    assert!(b.terminate().success(), "{}", b.log());
    let from_b = seed(&world.b, &world.a_peer, 0xb0, "B");
    let mut b = app::start(&world.b);
    until_lease(&world.b.binding(), true, &b).await;
    received(&world, &world.a, &from_b, &world.b_peer, &a).await;
    until_rows(&world.b, "pending_outbound", 0, || b.log()).await;

    assert!(a.terminate().success(), "{}", a.log());
    assert!(b.terminate().success(), "{}", b.log());
}
