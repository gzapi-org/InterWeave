// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `human-client-ui.md` §13's accessibility bullet on a real platform
//! adapter: the shipped client's tree, read back over AT-SPI the way a
//! screen reader reads it, carries meaningful labels and actions for the
//! message, route and connectivity controls, and its one announcement is
//! raised to the screen reader as an AT-SPI `Announcement`.
//!
//! The trust controls the same bullet names do not exist yet: they are
//! batch 9's, and their case is added with them.

use interweave_human_transport_client::Connectivity;
use interweave_human_ui_model::{UiText, fill, placeholder_en, short_peer};

use crate::a11y::{Bus, Element};
use crate::common::human;
use crate::display;
use crate::harness::{self as app, until_lease};
use crate::world::{Peer, envelope, two_daemons};

fn text(key: UiText) -> &'static str {
    placeholder_en::text(key)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_tree_read_over_atspi_labels_message_route_and_connectivity_controls() {
    let _focus = display::exclusive().await;
    let world = two_daemons().await;
    let mut peer = Peer::new(&world.b);
    let bus = Bus::connect().await;
    let mut client = app::start(&world.a);
    until_lease(&world.a.binding(), true, &client).await;
    let window = bus.window_of(client.child.id(), || client.log()).await;
    display::focus(client.child.id(), || client.log()).await;
    let mut heard = window.announcements().await;
    peer.until("B ready", || world.logs(), Peer::is_ready).await;

    let b = world.b_peer.as_str();
    let short = short_peer(b);
    let title = fill(
        text(UiText::DirectTitle),
        &[("peer", &short), ("route", human().as_str())],
    );
    let log = || client.log();

    // A message arrives in a conversation not open: the announcement names
    // the conversation, and the row says what it is and how much is
    // unread, and can be pressed.
    let destination = "https://example.org/docs";
    peer.deliver(
        &world.a_peer,
        Some(human()),
        &envelope(71, &format!("see [the docs]({destination})")),
        || world.logs(),
    )
    .await;
    heard
        .until(
            &fill(text(UiText::AnnounceElsewhere), &[("conversation", &title)]),
            log,
        )
        .await;
    let row = window
        .until(
            "the conversation's row, labelled and pressable",
            |e: &Element| {
                e.role == "list item"
                    && e.name == title
                    && e.description.contains(text(UiText::DirectConversation))
                    && e.actions == ["click"]
            },
            log,
        )
        .await;
    window.activate(&row, "click").await;

    // The message: the item read as author, status and the drawn body; the
    // author carrying the exact PeerId; the route as a routing label; the
    // link a control naming its full destination; the source one press
    // away.
    window
        .until(
            "the message item",
            |e: &Element| {
                e.role == "list item"
                    && e.name.starts_with(&format!("{short}, "))
                    && e.name.ends_with(": see the docs")
            },
            log,
        )
        .await;
    window
        .until(
            "the author, with the exact PeerId",
            |e: &Element| e.role == "label" && e.name == short && e.description == b,
            log,
        )
        .await;
    window
        .until(
            "the route",
            |e: &Element| {
                e.role == "label"
                    && e.name == fill(text(UiText::Route), &[("route", human().as_str())])
            },
            log,
        )
        .await;
    for control in [
        fill(text(UiText::OpenLink), &[("destination", destination)]),
        text(UiText::ShowSource).to_owned(),
        text(UiText::Send).to_owned(),
    ] {
        window
            .until(
                &format!("the {control:?} control"),
                |e: &Element| e.role == "button" && e.name == control && e.actions == ["click"],
                log,
            )
            .await;
    }
    window
        .until(
            "the composer",
            |e: &Element| e.name == text(UiText::Composer),
            log,
        )
        .await;
    window
        .until(
            "the connectivity indicator",
            |e: &Element| {
                e.role == "label"
                    && [
                        Connectivity::OnlineDirect,
                        Connectivity::OnlineRelay,
                        Connectivity::OnlinePartial,
                        Connectivity::Offline,
                        Connectivity::Unknown,
                    ]
                    .iter()
                    .any(|c| e.name == placeholder_en::connectivity(*c))
            },
            log,
        )
        .await;

    // A message arriving in the open conversation: who sent it, never
    // what it says.
    peer.deliver(
        &world.a_peer,
        Some(human()),
        &envelope(72, "private words"),
        || world.logs(),
    )
    .await;
    let said = heard
        .until(
            &fill(text(UiText::AnnounceArrival), &[("author", &short)]),
            log,
        )
        .await;
    assert!(
        !said.iter().any(|s| s.contains("private words")),
        "a message's text is never announced: {said:?}"
    );
    assert!(client.terminate().success(), "{}", client.log());
}
