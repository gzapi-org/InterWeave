// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The read half of retention (ADR-0044, RETENTION.md), with the reading
//! done the way a person does it: the window given the focus, and the
//! conversation opened and Keep pressed through their accessible actions.
//! A message read and not kept leaves the store; one the person keeps
//! after reading stays across a restart; and a read message re-sent after
//! a restart does not come back as unread (`read_pairs`).

use std::time::Duration;

use interweave_human_chat_protocol::HumanChatV2;
use interweave_human_ui_model::{LabelKey, UiText, placeholder_en};

use crate::a11y::{Bus, Element, Window, describe};
use crate::common::human;
use crate::display;
use crate::harness::{self as app, App, until_lease};
use crate::world::{Peer, World, envelope, ids, two_daemons, until_rows};

/// The conversation list's row: the one selectable, pressable item.
fn conversation(element: &Element) -> bool {
    element.role == "list item" && element.actions.iter().any(|a| a == "click")
}

/// The message item whose body is `text`, with the status `status`.
fn message<'a>(text: &'a str, status: &'a str) -> impl Fn(&Element) -> bool + 'a {
    move |e| {
        e.role == "list item"
            && e.actions.is_empty()
            && e.name.ends_with(&format!(", {status}: {text}"))
    }
}

/// Any message item whose body is `text`, whatever its status.
fn any_message(text: &str) -> impl Fn(&Element) -> bool + '_ {
    move |e| e.role == "list item" && e.actions.is_empty() && e.name.ends_with(&format!(": {text}"))
}

/// The client on `world`'s side A: started, holding its lease, its
/// window on the AT-SPI bus and given the focus.
async fn started<'b>(world: &World, bus: &'b Bus) -> (App, Window<'b>) {
    let client = app::start(&world.a);
    until_lease(&world.a.binding(), true, &client).await;
    let window = bus.window_of(client.child.id(), || client.log()).await;
    display::focus(client.child.id(), || client.log()).await;
    (client, window)
}

/// Open the conversation with B, as a person would.
async fn open(window: &Window<'_>, client: &App) {
    let row = window
        .until("the conversation with B", conversation, || client.log())
        .await;
    window.activate(&row, "click").await;
}

/// B sends `message` to A's `human` endpoint, and A's store holds it
/// unread.
async fn arrives(world: &World, peer: &mut Peer, message: &HumanChatV2, client: &App) {
    peer.send(&world.a_peer, Some(human()), message).await;
    let deadline = tokio::time::Instant::now() + crate::common::PATIENCE;
    while !ids(&world.a, "unread_inbound").contains(&message.app_message_id) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{} never arrived unread: {}",
            message.app_message_id,
            client.log()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Open the conversation and check `gone` is not in it while `shown` is:
/// the item that is there shows the list was drawn, so the one that is
/// not was not left out by an empty window.
async fn absent_beside(window: &Window<'_>, client: &App, gone: &str, shown: &str) {
    open(window, client).await;
    window
        .until("the control message, shown", any_message(shown), || {
            client.log()
        })
        .await;
    let tree = window.read().await;
    assert!(
        tree.iter().any(any_message(shown)) && !tree.iter().any(any_message(gone)),
        "{gone:?} is not shown beside {shown:?}:\n{}",
        describe(&tree)
    );
}

fn label(key: LabelKey) -> &'static str {
    placeholder_en::label(key)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_read_and_not_kept_leaves_the_store() {
    let _focus = display::exclusive().await;
    let world = two_daemons().await;
    let mut peer = Peer::new(&world.b);
    let bus = Bus::connect().await;
    let (mut client, window) = started(&world, &bus).await;
    peer.until("B ready", || world.logs(), Peer::is_ready).await;

    let text = "read me and let me go";
    let sent = envelope(41, text);
    arrives(&world, &mut peer, &sent, &client).await;
    open(&window, &client).await;
    window
        .until(
            "the message, read",
            message(text, label(LabelKey::ReadNotKept)),
            || client.log(),
        )
        .await;
    until_rows(&world.a, "unread_inbound", 0, || client.log()).await;
    assert!(ids(&world.a, "kept_inbound").is_empty(), "read is not kept");
    assert!(client.terminate().success(), "{}", client.log());

    let (mut again, window) = started(&world, &bus).await;
    assert!(ids(&world.a, "unread_inbound").is_empty());
    // The conversation opened again, around a new message: the list then
    // shows what the client holds, and the read one is not in it.
    let control = envelope(42, "new since the restart");
    arrives(&world, &mut peer, &control, &again).await;
    absent_beside(&window, &again, text, "new since the restart").await;
    assert!(again.terminate().success(), "{}", again.log());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_kept_after_reading_stays_across_a_restart() {
    let _focus = display::exclusive().await;
    let world = two_daemons().await;
    let mut peer = Peer::new(&world.b);
    let bus = Bus::connect().await;
    let (mut client, window) = started(&world, &bus).await;
    peer.until("B ready", || world.logs(), Peer::is_ready).await;

    let text = "keep me";
    let sent = envelope(51, text);
    arrives(&world, &mut peer, &sent, &client).await;
    open(&window, &client).await;
    window
        .until(
            "the message, read",
            message(text, label(LabelKey::ReadNotKept)),
            || client.log(),
        )
        .await;
    let keep = window
        .until(
            "its Keep control",
            |e| e.role == "button" && e.name == placeholder_en::text(UiText::Keep),
            || client.log(),
        )
        .await;
    window.activate(&keep, "click").await;
    window
        .until(
            "the message, kept",
            message(text, label(LabelKey::Kept)),
            || client.log(),
        )
        .await;
    until_rows(&world.a, "kept_inbound", 1, || client.log()).await;
    assert_eq!(
        ids(&world.a, "kept_inbound"),
        [sent.app_message_id.clone()].into()
    );
    assert!(client.terminate().success(), "{}", client.log());

    let (mut again, window) = started(&world, &bus).await;
    open(&window, &again).await;
    window
        .until(
            "the kept message, after a restart",
            message(text, label(LabelKey::Kept)),
            || again.log(),
        )
        .await;
    assert_eq!(
        ids(&world.a, "kept_inbound"),
        [sent.app_message_id.clone()].into()
    );
    assert!(ids(&world.a, "unread_inbound").is_empty());
    assert!(again.terminate().success(), "{}", again.log());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_message_re_sent_after_a_restart_does_not_come_back_unread() {
    let _focus = display::exclusive().await;
    let world = two_daemons().await;
    let mut peer = Peer::new(&world.b);
    let bus = Bus::connect().await;
    let (mut client, window) = started(&world, &bus).await;
    peer.until("B ready", || world.logs(), Peer::is_ready).await;

    let text = "once is enough";
    let sent = envelope(61, text);
    arrives(&world, &mut peer, &sent, &client).await;
    open(&window, &client).await;
    until_rows(&world.a, "unread_inbound", 0, || client.log()).await;
    assert_eq!(
        ids(&world.a, "read_pairs"),
        [sent.app_message_id.clone()].into(),
        "the read is remembered"
    );
    assert!(client.terminate().success(), "{}", client.log());

    let (mut again, window) = started(&world, &bus).await;
    // The same message again -- the same application id, a new send --
    // admitted to A's `human` queue (B sees it accepted), and then watched
    // for a while: the transport promises no order, so its refusal is
    // observed as its absence over that time, not inferred from another
    // message overtaking it. The control after it shows the path still
    // delivers, so the absence is not a dead client's.
    peer.outbound.remove(&sent.app_message_id);
    peer.send(&world.a_peer, Some(human()), &sent).await;
    peer.until(
        "the re-send admitted at A",
        || world.logs(),
        |p| p.accepted(&sent.app_message_id),
    )
    .await;
    let watch = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < watch {
        assert!(
            !ids(&world.a, "unread_inbound").contains(&sent.app_message_id),
            "the re-sent message came back unread: {}",
            again.log()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let control = envelope(62, "new, so unread");
    arrives(&world, &mut peer, &control, &again).await;
    assert_eq!(
        ids(&world.a, "unread_inbound"),
        [control.app_message_id.clone()].into(),
        "only the new message is unread"
    );
    absent_beside(&window, &again, text, "new, so unread").await;
    assert!(again.terminate().success(), "{}", again.log());
}
