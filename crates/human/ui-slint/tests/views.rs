// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The reference views against the surface agreed with rust-ui-dev (relay
//! seqs 10823, 10826, 10849, 10867, 10870, 10879, 10882, 10885), and
//! `human-client-ui.md` §13's accessibility-tree bullet, through Slint's
//! testing backend: the tree a screen reader would be handed, read by
//! label, role, description and live region, its default actions
//! invoked and keyboard focus moved by Tab.
//!
//! What this cannot prove, stated once: that a platform adapter exports
//! this tree (the AccessKit adapter is the `desktop` feature's, and its
//! AT-SPI cases are batch 7's), that a live region is announced,
//! contrast, text scaling, reduced motion,
//! or copying a `PeerId` (no clipboard without a backend).

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use i_slint_backend_testing::ElementHandle;
use interweave_human_chat_protocol::{HumanChatV2, MessageKind};
use interweave_human_client_api::{
    ClientEvent, Connectivity, Destination, Origin, OutboundStatus, OutboundUpdate, Received,
    SessionProblem, SessionState,
};
use interweave_human_core::{AppMessageId, RowId};
use interweave_human_ui_model::{ConversationKey, Intent, UiModel, UiText, placeholder_en};
use interweave_human_ui_slint::{INPUT_CAP, View, ViewEvent};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{ChannelId, EndpointId, TransportIdentity};
use slint::ComponentHandle as _;
use slint::platform::{Key, WindowEvent};

fn peer() -> TransportIdentity {
    ProfileIdentity::generate()
        .transport_identity()
        .expect("a peer id")
}

fn human() -> EndpointId {
    EndpointId::parse("human").expect("an endpoint")
}

fn envelope(serial: u128, text: &str) -> HumanChatV2 {
    HumanChatV2 {
        v: 2,
        kind: MessageKind::Text,
        app_message_id: format!("{serial:032x}"),
        text: text.to_owned(),
        reply_to: None,
        sent_at_ms: None,
        from_endpoint: None,
    }
}

fn direct(peer: &TransportIdentity) -> ConversationKey {
    ConversationKey::Direct {
        peer: peer.clone(),
        endpoint: Some(human()),
    }
}

fn received(row: i64, from: &TransportIdentity, text: &str) -> Received {
    Received {
        row: RowId::from_stored(row),
        origin: Origin::Direct {
            peer: from.clone(),
            endpoint: human(),
        },
        envelope: envelope(u128::try_from(row).expect("positive") + 1_000, text),
        received_at: u64::try_from(row).expect("positive"),
    }
}

/// An outbound row this client sent to `to`, pending.
fn sent(model: &mut UiModel, row: i64, to: &TransportIdentity, text: &str) -> AppMessageId {
    let env = envelope(u128::try_from(row).expect("positive"), text);
    let id = AppMessageId::parse(env.app_message_id.clone()).expect("an id");
    model.sent(
        RowId::from_stored(row),
        &Destination::Direct {
            peer: to.clone(),
            endpoint: Some(human()),
        },
        env,
        u64::try_from(row).expect("positive"),
    );
    id
}

fn update(model: &mut UiModel, row: i64, id: &AppMessageId, status: OutboundStatus) {
    model.client_event(ClientEvent::Outbound(OutboundUpdate {
        row: RowId::from_stored(row),
        app_message_id: id.clone(),
        status,
        last_code: None,
    }));
}

fn view() -> View {
    i_slint_backend_testing::init_no_event_loop();
    let view = View::new().expect("a window");
    view.window().show().expect("shown");
    view
}

/// The root's loop: take, apply each draft edit, take again, until empty.
fn intents(view: &mut View, model: &mut UiModel) -> Vec<Intent> {
    let mut out = Vec::new();
    loop {
        let events = view.take_events(model);
        if events.is_empty() {
            return out;
        }
        for event in events {
            match event {
                ViewEvent::Intent(intent) => out.push(intent),
                ViewEvent::DraftChanged { key, draft } => model.draft_changed(key, draft),
            }
        }
    }
}

/// Select `key` and let the view show it.
fn open(view: &mut View, model: &mut UiModel, key: &ConversationKey) -> Vec<Intent> {
    view.select(key.clone());
    let got = intents(view, model);
    view.render(model);
    got
}

fn labelled(view: &View, label: &str) -> Vec<ElementHandle> {
    ElementHandle::find_by_accessible_label(view.window(), label).collect()
}

fn the(view: &View, label: &str) -> ElementHandle {
    let mut found = labelled(view, label);
    assert_eq!(found.len(), 1, "exactly one element labelled {label:?}");
    found.remove(0)
}

fn all(view: &View) -> Vec<ElementHandle> {
    let found = i_slint_backend_testing::ElementQuery::from_root(view.window()).find_all();
    assert!(
        !found.is_empty(),
        "the tree is not empty: built with debug info"
    );
    found
}

fn tab(view: &View) -> String {
    let key: slint::SharedString = Key::Tab.into();
    view.window()
        .window()
        .dispatch_event(WindowEvent::KeyPressed { text: key.clone() });
    view.window()
        .window()
        .dispatch_event(WindowEvent::KeyReleased { text: key });
    view.window().get_focus_id().to_string()
}

fn text(t: UiText) -> &'static str {
    placeholder_en::text(t)
}

/// U3a: lists change by key. Focus held on a message stays there when a
/// newer message arrives and the view re-renders: the next Tab goes to
/// the element after it, not back to the start.
#[test]
fn keyed_updates_keep_focus_on_an_item_when_a_message_arrives() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    model.received(received(1, &alice, "first"));
    model.received(received(2, &alice, "second"));
    open(&mut view, &mut model, &direct(&alice));
    let mut on = String::new();
    for _ in 0..20 {
        on = tab(&view);
        if on.starts_with("item:") {
            break;
        }
    }
    assert!(on.starts_with("item:"), "Tab reaches a message item: {on}");
    let next_without_change = {
        let after = tab(&view);
        // Back to where we were: shift-tab is not needed, re-walk.
        for _ in 0..40 {
            if tab(&view) == on {
                break;
            }
        }
        after
    };
    model.received(received(3, &alice, "third"));
    view.render(&model);
    assert_eq!(
        tab(&view),
        next_without_change,
        "focus stayed on {on} across the render"
    );
}

/// U3b: a conversation viewed while the window lacks focus is not read;
/// focus gained while it is shown reads it.
#[test]
fn viewing_unfocused_reads_nothing_and_focus_gained_reads() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    model.received(received(1, &alice, "hello"));
    assert!(
        open(&mut view, &mut model, &direct(&alice)).is_empty(),
        "unfocused, nothing is read"
    );
    view.set_window_focused(true);
    assert_eq!(
        intents(&mut view, &mut model),
        vec![Intent::MarkRead(RowId::from_stored(1))]
    );
}

/// P1: a press is the action the person saw. Retry pressed, then the row
/// goes terminal before the root takes it: nothing comes out, and above
/// all not Cancel.
#[test]
fn a_press_whose_action_is_gone_yields_nothing() {
    let mut view = view();
    let mut model = UiModel::new();
    let bob = peer();
    let id = sent(&mut model, 7, &bob, "to bob");
    update(
        &mut model,
        7,
        &id,
        OutboundStatus::Sending {
            attempts: 1,
            next_retry_at: Some(10),
            last_problem: None,
        },
    );
    open(&mut view, &mut model, &direct(&bob));
    the(&view, text(UiText::Retry)).invoke_accessible_default_action();
    update(
        &mut model,
        7,
        &id,
        OutboundStatus::Accepted { endpoint: human() },
    );
    assert_eq!(intents(&mut view, &mut model), Vec::<Intent>::new());

    // The control: the same press with the action still offered.
    let id2 = sent(&mut model, 8, &bob, "again");
    update(
        &mut model,
        8,
        &id2,
        OutboundStatus::Sending {
            attempts: 1,
            next_retry_at: Some(10),
            last_problem: None,
        },
    );
    view.render(&model);
    the(&view, text(UiText::Retry)).invoke_accessible_default_action();
    assert_eq!(
        intents(&mut view, &mut model),
        vec![Intent::Retry(RowId::from_stored(8))]
    );
}

/// P2: an edit then a send in one batch sends the edited text.
#[test]
fn an_edit_then_a_send_in_one_batch_sends_the_edited_text() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    model.received(received(1, &alice, "hi"));
    let key = direct(&alice);
    open(&mut view, &mut model, &key);
    view.window().invoke_draft_edited("hello there".into());
    view.window().invoke_send();
    let first = view.take_events(&model);
    assert_eq!(
        first,
        vec![ViewEvent::DraftChanged {
            key: key.clone(),
            draft: "hello there".to_owned()
        }],
        "a take stops at the edit"
    );
    model.draft_changed(key.clone(), "hello there".to_owned());
    assert_eq!(
        intents(&mut view, &mut model),
        vec![Intent::Send {
            key,
            draft: "hello there".to_owned()
        }]
    );
}

/// The coalescing rule (relay seq 10885): edit, send, edit sends the
/// FIRST edit -- the second is typed after the press.
#[test]
fn edit_send_edit_sends_the_first_edit() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    model.received(received(1, &alice, "hi"));
    let key = direct(&alice);
    open(&mut view, &mut model, &key);
    view.window().invoke_draft_edited("first".into());
    view.window().invoke_send();
    view.window().invoke_draft_edited("second".into());
    let sends: Vec<Intent> = intents(&mut view, &mut model)
        .into_iter()
        .filter(|i| matches!(i, Intent::Send { .. }))
        .collect();
    assert_eq!(
        sends,
        vec![Intent::Send {
            key: key.clone(),
            draft: "first".to_owned()
        }]
    );
    assert_eq!(model.composer(&key).draft, "second", "the later edit kept");

    // Consecutive edits with nothing between do coalesce: typing alone
    // never fills the queue.
    for n in 0..(INPUT_CAP * 2) {
        view.window()
            .invoke_draft_edited(format!("typing {n}").into());
    }
    assert_eq!(view.refused_inputs(), 0);
    assert_eq!(
        view.take_events(&model),
        vec![ViewEvent::DraftChanged {
            key,
            draft: format!("typing {}", INPUT_CAP * 2 - 1)
        }]
    );
}

/// The bound (relay seqs 10879, 10882): a full queue refuses the newest
/// PRESS and counts it; an edit queued first survives, and a press after
/// a take works.
#[test]
fn a_full_queue_refuses_the_newest_and_keeps_the_edit() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    model.received(received(1, &alice, "hi"));
    let key = direct(&alice);
    open(&mut view, &mut model, &key);
    view.window().invoke_draft_edited("kept edit".into());
    for _ in 0..INPUT_CAP {
        view.select(key.clone());
    }
    assert_eq!(view.refused_inputs(), 0, "exactly at the bound");
    view.window().invoke_send();
    assert_eq!(
        view.refused_inputs(),
        1,
        "the newest press is refused, counted"
    );
    assert_eq!(
        view.take_events(&model),
        vec![ViewEvent::DraftChanged {
            key: key.clone(),
            draft: "kept edit".to_owned()
        }],
        "the oldest input, the edit, survived"
    );
    model.draft_changed(key.clone(), "kept edit".to_owned());
    assert!(
        intents(&mut view, &mut model).is_empty(),
        "no send was queued"
    );
    view.window().invoke_send();
    assert_eq!(
        intents(&mut view, &mut model),
        vec![Intent::Send {
            key,
            draft: "kept edit".to_owned()
        }],
        "pressed again after a take, it is sent"
    );
}

/// The notice's action resolves only while the notice it was pressed on
/// is the one shown.
#[test]
fn the_notice_action_resolves_only_while_its_notice_is_shown() {
    let mut view = view();
    let mut model = UiModel::new();
    model.client_event(ClientEvent::Session(SessionState::Refused {
        problem: SessionProblem::EndpointInUse,
    }));
    view.render(&model);
    let try_again = the(&view, text(UiText::TryAgain));
    assert_eq!(
        try_again.accessible_role(),
        Some(i_slint_backend_testing::AccessibleRole::Button)
    );
    try_again.invoke_accessible_default_action();
    assert_eq!(intents(&mut view, &mut model), vec![Intent::Reopen]);

    try_again.invoke_accessible_default_action();
    model.client_event(ClientEvent::Session(SessionState::Ready {
        endpoint: Some(human()),
    }));
    assert!(
        intents(&mut view, &mut model).is_empty(),
        "the notice is gone: its press does nothing"
    );
}

/// §13, the accessibility tree: message, route and connectivity controls
/// carry meaningful labels, roles and descriptions; connectivity is a
/// polite live region and an item's status is not; the full `PeerId` is on the
/// author and the header, never in an item's label (U5a).
#[test]
fn the_tree_labels_message_route_and_connectivity_controls() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    model.received(received(1, &alice, "hello alice"));
    model.client_event(ClientEvent::Connectivity(Connectivity::OnlineDirect));
    open(&mut view, &mut model, &direct(&alice));
    let id = alice.as_str();
    let short = interweave_human_ui_model::short_peer(id);
    assert_eq!(
        view.window().get_window_title(),
        text(UiText::AppTitle),
        "the window title is the table's"
    );
    let unread = placeholder_en::label(interweave_human_ui_model::LabelKey::Unread);

    let item = the(&view, &format!("{short}, {unread}: hello alice"));
    assert_eq!(
        item.accessible_role(),
        Some(i_slint_backend_testing::AccessibleRole::ListItem)
    );
    assert!(
        !item.accessible_label().unwrap().contains(id),
        "the full PeerId is not read on every item"
    );

    let authors: Vec<_> = labelled(&view, &short)
        .into_iter()
        .filter(|e| e.accessible_description().as_deref() == Some(id))
        .collect();
    assert_eq!(authors.len(), 1, "the author carries the exact PeerId");

    let route = the(&view, "route: human");
    assert_eq!(
        route.accessible_role(),
        Some(i_slint_backend_testing::AccessibleRole::Text)
    );

    let status = the(&view, unread);
    assert_eq!(
        status.accessible_role(),
        Some(i_slint_backend_testing::AccessibleRole::Text)
    );
    assert_eq!(
        status.accessible_live_region(),
        None,
        "an item's status is not a live region: the window's one \
         announcement says what changed (F4)"
    );

    let online = the(
        &view,
        placeholder_en::connectivity(Connectivity::OnlineDirect),
    );
    assert_eq!(
        online.accessible_live_region(),
        Some(i_slint_backend_testing::AccessibleLiveness::Polite)
    );

    let header = all(&view)
        .into_iter()
        .find(|e| {
            e.accessible_description().as_deref() == Some(id)
                && e.accessible_label().is_some_and(|l| l.contains("human"))
        })
        .expect("the conversation header");
    assert_eq!(
        header.accessible_role(),
        Some(i_slint_backend_testing::AccessibleRole::Text)
    );

    let composer = the(&view, text(UiText::Composer));
    assert_eq!(
        composer.accessible_role(),
        Some(i_slint_backend_testing::AccessibleRole::TextInput)
    );
}

/// §13 bullet 2 at the view: `AcceptedV2` never renders as read or seen.
#[test]
fn accepted_v2_never_reads_as_read_or_seen_in_the_tree() {
    let mut view = view();
    let mut model = UiModel::new();
    let bob = peer();
    let id = sent(&mut model, 3, &bob, "for bob");
    update(
        &mut model,
        3,
        &id,
        OutboundStatus::Accepted { endpoint: human() },
    );
    open(&mut view, &mut model, &direct(&bob));
    let accepted =
        placeholder_en::label(interweave_human_ui_model::LabelKey::AcceptedByRemoteTransport);
    let status: Vec<_> = labelled(&view, accepted);
    assert!(!status.is_empty(), "the status is in the tree");
    for element in all(&view) {
        let label = element
            .accessible_label()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if label.contains("for bob") || label == accepted.to_ascii_lowercase() {
            for forbidden in [" read", "seen", "delivered"] {
                assert!(!label.contains(forbidden), "{label}");
            }
        }
    }
}

/// `Unknown` connectivity is shown as itself, never as offline.
#[test]
fn unknown_connectivity_is_not_shown_as_offline() {
    let mut view = view();
    let model = UiModel::new();
    assert_eq!(model.connectivity(), Connectivity::Unknown);
    view.render(&model);
    let shown = the(&view, placeholder_en::connectivity(Connectivity::Unknown));
    assert!(
        !shown
            .accessible_label()
            .unwrap()
            .to_ascii_lowercase()
            .contains("offline")
    );
    assert!(labelled(&view, placeholder_en::connectivity(Connectivity::Offline)).is_empty());
}

/// U5b: every action element exposes a default action, invoked through
/// the tree, and it gives the expected intent.
#[test]
fn every_action_element_has_a_default_action_giving_its_intent() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    model.received(received(1, &alice, "hi"));
    let key = direct(&alice);
    view.set_window_focused(true);
    intents(&mut view, &mut model);
    view.render(&model);
    let row = the(&view, &model.conversations()[0].title);
    row.invoke_accessible_default_action();
    assert_eq!(
        intents(&mut view, &mut model),
        vec![Intent::MarkRead(RowId::from_stored(1))],
        "the conversation row selects, and focused, reads"
    );
    model.read(RowId::from_stored(1));
    view.render(&model);
    the(&view, text(UiText::Keep)).invoke_accessible_default_action();
    let keep = intents(&mut view, &mut model);
    assert!(matches!(keep.as_slice(), [Intent::Keep { .. }]), "{keep:?}");

    let id = sent(&mut model, 9, &alice, "pending");
    update(
        &mut model,
        9,
        &id,
        OutboundStatus::Sending {
            attempts: 1,
            next_retry_at: Some(5),
            last_problem: None,
        },
    );
    view.render(&model);
    the(&view, text(UiText::Cancel)).invoke_accessible_default_action();
    assert_eq!(
        intents(&mut view, &mut model),
        vec![Intent::Cancel(RowId::from_stored(9))]
    );

    view.window().invoke_draft_edited("typed".into());
    intents(&mut view, &mut model);
    the(&view, text(UiText::Send)).invoke_accessible_default_action();
    assert_eq!(
        intents(&mut view, &mut model),
        vec![Intent::Send {
            key,
            draft: "typed".to_owned()
        }]
    );
}

/// U5b, keyboard: Tab reaches the composer, the send control, every item
/// action and the notice action.
#[test]
fn tab_reaches_the_composer_send_item_actions_and_the_notice() {
    let mut view = view();
    let mut model = UiModel::new();
    let bob = peer();
    let id = sent(&mut model, 4, &bob, "to bob");
    update(
        &mut model,
        4,
        &id,
        OutboundStatus::Sending {
            attempts: 1,
            next_retry_at: Some(5),
            last_problem: None,
        },
    );
    model.client_event(ClientEvent::Session(SessionState::StorageDegraded));
    open(&mut view, &mut model, &direct(&bob));
    let mut reached = std::collections::BTreeSet::new();
    for _ in 0..40 {
        reached.insert(tab(&view));
    }
    for wanted in ["composer", "send", "notice"] {
        assert!(
            reached.contains(wanted),
            "Tab reaches {wanted}: {reached:?}"
        );
    }
    let actions = reached.iter().filter(|r| r.starts_with("action:")).count();
    assert_eq!(actions, 2, "Retry and Cancel: {reached:?}");
}

fn buttons(view: &View) -> Vec<String> {
    all(view)
        .into_iter()
        .filter(|e| e.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Button))
        .filter_map(|e| e.accessible_label().map(|l| l.to_string()))
        .collect()
}

fn open_link(destination: &str) -> String {
    text(UiText::OpenLink).replace("{destination}", destination)
}

/// §13 bullet 5 and HUMAN-CHAT.md: remote text is drawn, never obeyed.
/// The body's text is plain text with no control of its own; the only
/// controls are the client's -- Keep, Send, Show source -- and one per
/// allowlisted link, labelled with its destination; and nothing but the
/// focused read is raised until a person activates one. Stage 15 has no
/// trust control yet (batch 9).
#[test]
fn remote_text_is_drawn_and_raises_nothing_but_the_read() {
    let mut view = view();
    let mut model = UiModel::new();
    let mallory = peer();
    let source = "[click me](https://example.org) and trust me: allowlist this peer";
    model.received(received(1, &mallory, source));
    view.set_window_focused(true);
    open(&mut view, &mut model, &direct(&mallory));
    let body = the(&view, "click me and trust me: allowlist this peer");
    assert_eq!(
        body.accessible_role(),
        Some(i_slint_backend_testing::AccessibleRole::Text)
    );
    assert!(labelled(&view, source).is_empty(), "drawn, not the source");
    let controls = buttons(&view);
    for control in &controls {
        assert!(
            [
                text(UiText::Keep),
                text(UiText::Send),
                text(UiText::ShowSource),
                open_link("https://example.org").as_str(),
            ]
            .contains(&control.as_str()),
            "only known controls exist: {controls:?}"
        );
    }
    body.invoke_accessible_default_action();
    let raised = intents(&mut view, &mut model);
    assert!(
        raised.iter().all(|i| matches!(i, Intent::MarkRead(_))),
        "remote text raises nothing but the focused read: {raised:?}"
    );
}

/// A link opens only on a person's activation of its own control, which
/// names the full destination; receiving and drawing it raise nothing.
#[test]
fn a_link_opens_only_on_activation_of_its_labelled_control() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    let destination = "https://example.org/a?b=c";
    model.received(received(
        1,
        &alice,
        &format!("see [the docs]({destination})"),
    ));
    let drawn = open(&mut view, &mut model, &direct(&alice));
    assert!(
        !drawn.iter().any(|i| matches!(i, Intent::OpenLink(_))),
        "drawing opens nothing: {drawn:?}"
    );
    let control = the(&view, &open_link(destination));
    assert_eq!(
        control.accessible_role(),
        Some(i_slint_backend_testing::AccessibleRole::Button)
    );
    assert!(intents(&mut view, &mut model).is_empty(), "nothing yet");
    control.invoke_accessible_default_action();
    assert_eq!(
        intents(&mut view, &mut model),
        [Intent::OpenLink(destination.to_owned())],
        "exactly the destination shown, on activation"
    );
}

/// A link outside the allowlist and an image offer nothing to activate:
/// the link is its text, the image a placeholder naming its alt text,
/// and its address is neither shown as a control nor fetched.
#[test]
fn an_inert_link_and_an_image_offer_nothing_to_activate() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    model.received(received(
        1,
        &alice,
        "[run](javascript:alert(1)) ![a cat](https://example.org/cat.png)",
    ));
    open(&mut view, &mut model, &direct(&alice));
    let placeholder = text(UiText::ImageNotShown).replace("{alt}", "a cat");
    assert_eq!(labelled(&view, &format!("run {placeholder}")).len(), 1);
    let controls = buttons(&view);
    assert!(
        !controls
            .iter()
            .any(|c| c.contains("javascript") || c.contains("cat.png")),
        "no control for either: {controls:?}"
    );
}

/// The source as received is one activation away, and back again
/// (HUMAN-CHAT.md: always viewable, however it rendered).
#[test]
fn the_source_is_one_activation_away() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    let source = "# Hello\n\n*there*";
    model.received(received(1, &alice, source));
    open(&mut view, &mut model, &direct(&alice));
    assert_eq!(labelled(&view, "Hello").len(), 1, "drawn first");
    assert!(labelled(&view, source).is_empty());
    the(&view, text(UiText::ShowSource)).invoke_accessible_default_action();
    assert_eq!(labelled(&view, source).len(), 1, "the source, as received");
    assert!(labelled(&view, "Hello").is_empty());
    the(&view, text(UiText::ShowFormatted)).invoke_accessible_default_action();
    assert_eq!(labelled(&view, "Hello").len(), 1, "and back");
}

/// Past a bound the body is its source as plain text, and its links are
/// not controls: nothing in it was parsed into anything.
#[test]
fn a_body_past_a_bound_is_its_source_as_text() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    let source = format!("{}[x](https://example.org)", "> ".repeat(17));
    model.received(received(1, &alice, &source));
    open(&mut view, &mut model, &direct(&alice));
    assert_eq!(labelled(&view, &source).len(), 1);
    assert!(
        !buttons(&view).iter().any(|c| c.contains("example.org")),
        "no link control from an unparsed source"
    );
}

/// U2a, U2b, U2c: a conversation says what it is in text; retention is
/// text, an unread copy of a kept message saying both; a reply whose
/// target is not held shows the neutral placeholder.
#[test]
fn rows_say_kind_retention_and_reply_as_text() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    let general = ChannelId::parse("general").expect("a channel");
    model.received(Received {
        row: RowId::from_stored(2),
        origin: Origin::Channel {
            channel: general.clone(),
            publisher: alice.clone(),
        },
        envelope: HumanChatV2 {
            reply_to: Some("f".repeat(32)),
            ..envelope(77, "a reply")
        },
        received_at: 2,
    });
    model.received(received(1, &alice, "direct hi"));
    view.render(&model);
    let direct_row = the(
        &view,
        &model
            .conversations()
            .iter()
            .find(|s| matches!(s.key, ConversationKey::Direct { .. }))
            .unwrap()
            .title,
    );
    assert!(
        direct_row
            .accessible_description()
            .unwrap()
            .contains(text(UiText::DirectConversation))
    );
    let channel_row = the(&view, "#general");
    assert!(
        channel_row
            .accessible_description()
            .unwrap()
            .contains(text(UiText::ChannelConversation))
    );
    open(&mut view, &mut model, &ConversationKey::Channel(general));
    assert_eq!(labelled(&view, text(UiText::ReplyUnavailable)).len(), 1);
    let unread = placeholder_en::label(interweave_human_ui_model::LabelKey::Unread);
    assert!(!labelled(&view, unread).is_empty(), "retention is text");
}

/// U2b: an unread re-sent copy of a message this client kept says both
/// facts, as text; the kept item alone says Kept.
#[test]
fn an_unread_copy_of_a_kept_message_says_both() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    let first = received(1, &alice, "keep this");
    model.kept_listed(vec![interweave_human_ui_model::ListedInbound {
        row: RowId::from_stored(50),
        origin: first.origin.clone(),
        envelope: first.envelope.clone(),
        received_at: 1,
    }]);
    open(&mut view, &mut model, &direct(&alice));
    let kept = placeholder_en::label(interweave_human_ui_model::LabelKey::Kept);
    assert!(!labelled(&view, kept).is_empty(), "the kept item says Kept");
    model.received(Received {
        row: RowId::from_stored(2),
        ..first
    });
    view.render(&model);
    assert!(
        !labelled(&view, text(UiText::UnreadAlsoKept)).is_empty(),
        "the unread copy says it is also kept"
    );
}

/// Review F3: the wake hook may take at once. A root whose wake runs
/// `take_events` straight away gets the press, rather than panicking on
/// state the press still held borrowed.
#[test]
fn a_wake_that_takes_at_once_gets_the_press() {
    use std::cell::RefCell;
    use std::rc::Rc;

    let view = Rc::new(RefCell::new(view()));
    let model = Rc::new(RefCell::new(UiModel::new()));
    let alice = peer();
    model.borrow_mut().received(received(1, &alice, "hi"));
    let key = direct(&alice);
    {
        let mut v = view.borrow_mut();
        open(&mut v, &mut model.borrow_mut(), &key);
    }
    model
        .borrow_mut()
        .draft_changed(key.clone(), "now".to_owned());
    let taken = Rc::new(RefCell::new(Vec::new()));
    {
        let (weak, model, taken) = (Rc::downgrade(&view), Rc::clone(&model), Rc::clone(&taken));
        view.borrow().set_wake(move || {
            let view = weak.upgrade().expect("the view");
            let events = view.borrow_mut().take_events(&model.borrow());
            taken.borrow_mut().extend(events);
        });
    }
    let window = view.borrow().window().clone_strong();
    window.invoke_send();
    assert_eq!(
        *taken.borrow(),
        vec![ViewEvent::Intent(Intent::Send {
            key,
            draft: "now".to_owned()
        })],
        "the wake ran, took, and got the send"
    );
}

/// Review F2: losing focus is never refused by a full queue. The view
/// does not go on believing it has focus, so a selection after a full
/// queue reads nothing.
#[test]
fn losing_focus_is_never_refused_by_a_full_queue() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    let bob = peer();
    model.received(received(1, &alice, "for later"));
    model.received(received(2, &bob, "elsewhere"));
    view.set_window_focused(true);
    open(&mut view, &mut model, &direct(&bob));
    intents(&mut view, &mut model);
    for _ in 0..INPUT_CAP {
        view.select(direct(&bob));
    }
    view.set_window_focused(false);
    view.select(direct(&alice));
    assert_eq!(view.refused_inputs(), 1, "the selection past the cap");
    let reads: Vec<Intent> = intents(&mut view, &mut model)
        .into_iter()
        .filter(|i| matches!(i, Intent::MarkRead(r) if *r == RowId::from_stored(1)))
        .collect();
    assert!(reads.is_empty(), "nothing in alice's conversation is read");
    view.select(direct(&alice));
    assert!(
        intents(&mut view, &mut model).is_empty(),
        "unfocused, selecting it reads nothing either"
    );
}

/// The composer as a person types into it: the text field's text moves,
/// and the edit is reported.
fn type_into(view: &View, text: &str) {
    view.window().set_draft(text.into());
    view.window().invoke_draft_edited(text.into());
}

/// Review F4, rust-ui-dev F2a: a render between a keystroke and its take
/// leaves the typed text alone.
#[test]
fn a_render_before_the_take_keeps_what_was_typed() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    model.received(received(1, &alice, "hi"));
    let key = direct(&alice);
    open(&mut view, &mut model, &key);
    type_into(&view, "a");
    intents(&mut view, &mut model);
    type_into(&view, "ab");
    model.received(received(2, &alice, "an interruption"));
    view.render(&model);
    assert_eq!(
        view.window().get_draft().as_str(),
        "ab",
        "not set back to 'a'"
    );
    intents(&mut view, &mut model);
    assert_eq!(model.composer(&key).draft, "ab");
}

/// rust-ui-dev F2b, review R2-1: an edit is never refused, however full
/// the queue -- what a person typed is never lost -- and a render meanwhile
/// does not erase it.
#[test]
fn an_edit_is_never_refused_by_a_full_queue() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    model.received(received(1, &alice, "hi"));
    let key = direct(&alice);
    open(&mut view, &mut model, &key);
    for _ in 0..INPUT_CAP {
        view.select(key.clone());
    }
    type_into(&view, "typed while full");
    assert_eq!(view.refused_inputs(), 0, "the edit was not refused");
    view.render(&model);
    assert_eq!(view.window().get_draft().as_str(), "typed while full");
    intents(&mut view, &mut model);
    assert_eq!(model.composer(&key).draft, "typed while full");
    view.window().invoke_send();
    assert_eq!(
        intents(&mut view, &mut model),
        vec![Intent::Send {
            key,
            draft: "typed while full".to_owned()
        }],
        "a send then carries what the person typed"
    );
}

/// Review R2-1's trace: the queue is full, the person selects another
/// conversation and types before the root takes. The edit belongs to the
/// conversation it was typed into, and reaches the model there.
#[test]
fn an_edit_typed_before_a_selection_is_taken_reaches_its_conversation() {
    let mut view = view();
    let mut model = UiModel::new();
    let (k, j) = (peer(), peer());
    model.received(received(1, &k, "k"));
    model.received(received(2, &j, "j"));
    let (key_k, key_j) = (direct(&k), direct(&j));
    open(&mut view, &mut model, &key_k);
    for _ in 1..INPUT_CAP {
        view.select(key_k.clone());
    }
    view.select(key_j.clone());
    type_into(&view, "typed into k");
    view.render(&model);
    intents(&mut view, &mut model);
    view.render(&model);
    assert_eq!(model.composer(&key_k).draft, "typed into k");
    assert_eq!(model.composer(&key_j).draft, "", "j's draft is untouched");
}

/// rust-ui-dev F1: a message that arrives in the conversation the person
/// is reading, with the window focused, is marked read; with the window
/// unfocused, it is not.
#[test]
fn a_message_arriving_in_the_shown_conversation_is_read_while_focused() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    model.received(received(1, &alice, "first"));
    view.set_window_focused(true);
    open(&mut view, &mut model, &direct(&alice));
    let first = intents(&mut view, &mut model);
    assert_eq!(first, vec![Intent::MarkRead(RowId::from_stored(1))]);
    model.read(RowId::from_stored(1));
    model.received(received(2, &alice, "arrives while reading"));
    view.render(&model);
    assert_eq!(
        intents(&mut view, &mut model),
        vec![Intent::MarkRead(RowId::from_stored(2))]
    );

    // The control: unfocused, the same arrival stays unread.
    model.read(RowId::from_stored(2));
    view.set_window_focused(false);
    intents(&mut view, &mut model);
    model.received(received(3, &alice, "arrives while away"));
    view.render(&model);
    assert!(intents(&mut view, &mut model).is_empty());
}

/// The queue's bound, at its worst order -- a focus change between every
/// two inputs and an edit beside every press -- is four times the cap
/// plus three: the cap's presses, an edit after each and one before them,
/// and a focus change beside each of those and one more. Anything further
/// coalesces or is refused: an edit folds into the last edit, a focus
/// change into the last focus change, a press past the cap is refused,
/// and a render's "viewed" is not queued. This holds for a root that
/// drains at each take, as `intents` does; the doc says what a root that
/// stops between takes allows.
#[test]
fn a_queue_drained_at_each_take_holds_at_most_four_times_the_cap_plus_three() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    model.received(received(1, &alice, "unread"));
    open(&mut view, &mut model, &direct(&alice));
    for n in 0..INPUT_CAP * 2 {
        view.set_window_focused(true);
        type_into(&view, &format!("draft {n}"));
        view.set_window_focused(false);
        view.window().invoke_send();
        view.render(&model);
    }
    view.set_window_focused(true);
    assert_eq!(view.queued_inputs(), 4 * INPUT_CAP + 3);
    assert_eq!(
        view.refused_inputs(),
        u64::try_from(INPUT_CAP).expect("small"),
        "only presses past the cap are refused"
    );
}

/// Review F1: a render that changes nothing about an item keeps focus on
/// its action button -- the next Tab continues from it.
#[test]
fn a_render_keeps_focus_on_an_items_action() {
    let mut view = view();
    let mut model = UiModel::new();
    let bob = peer();
    let id = sent(&mut model, 5, &bob, "pending");
    update(
        &mut model,
        5,
        &id,
        OutboundStatus::Sending {
            attempts: 1,
            next_retry_at: Some(5),
            last_problem: None,
        },
    );
    open(&mut view, &mut model, &direct(&bob));
    let mut on = String::new();
    for _ in 0..20 {
        on = tab(&view);
        if on.starts_with("action:") {
            break;
        }
    }
    assert!(on.starts_with("action:"), "Tab reaches an action: {on}");
    let next = tab(&view);
    for _ in 0..40 {
        if tab(&view) == on {
            break;
        }
    }
    model.client_event(ClientEvent::Connectivity(Connectivity::OnlineRelay));
    view.render(&model);
    assert_eq!(tab(&view), next, "focus stayed on {on} across the render");
}

/// Review F5: no text in the tree keeps a `{placeholder}` -- every call
/// site fills its template by the names the template holds. The states
/// drawn here render every template the views use.
#[test]
fn no_rendered_text_leaves_a_placeholder_unfilled() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    let general = ChannelId::parse("general").expect("a channel");
    model.received(Received {
        row: RowId::from_stored(2),
        origin: Origin::Channel {
            channel: general,
            publisher: alice.clone(),
        },
        envelope: HumanChatV2 {
            reply_to: Some("f".repeat(32)),
            ..envelope(77, "a reply")
        },
        received_at: 2,
    });
    model.received(received(1, &alice, "direct hi"));
    model.client_event(ClientEvent::Session(SessionState::Refused {
        problem: SessionProblem::EndpointInUse,
    }));
    let key = direct(&alice);
    open(&mut view, &mut model, &key);
    model.send_refused(
        key,
        "too much".to_owned(),
        &interweave_human_client_api::SendError::TooLarge,
    );
    view.render(&model);
    let mut seen = 0;
    for element in all(&view) {
        for text in [element.accessible_label(), element.accessible_description()]
            .into_iter()
            .flatten()
        {
            seen += 1;
            assert!(
                !text.contains('{') && !text.contains('}'),
                "an unfilled placeholder: {text}"
            );
        }
    }
    assert!(seen > 10, "the tree had text to check: {seen}");
    // The other half: each template the sweep must have covered is in the
    // tree, as its filled text -- so a template that rendered nothing
    // cannot pass the sweep by its absence.
    let busy = placeholder_en::error(interweave_human_ui_model::ErrorClass::EndpointInUse);
    let too_large = placeholder_en::error(interweave_human_ui_model::ErrorClass::TooLarge);
    let short = interweave_human_ui_model::short_peer(alice.as_str());
    let unread = placeholder_en::label(interweave_human_ui_model::LabelKey::Unread);
    let unread_count =
        interweave_human_ui_model::fill(text(UiText::UnreadCount), &[("count", "1")]);
    for expected in [
        interweave_human_ui_model::fill(text(UiText::Refused), &[("reason", busy)]),
        interweave_human_ui_model::fill(text(UiText::NotSent), &[("reason", too_large)]),
        interweave_human_ui_model::fill(
            text(UiText::Item),
            &[
                ("author", &short),
                ("status", unread),
                ("body", "direct hi"),
            ],
        ),
        interweave_human_ui_model::fill(
            text(UiText::ConversationDescription),
            &[
                ("kind", text(UiText::DirectConversation)),
                ("unread", &unread_count),
            ],
        ),
        interweave_human_ui_model::fill(text(UiText::Route), &[("route", "human")]),
    ] {
        let shown = all(&view).iter().any(|e| {
            e.accessible_label().as_deref() == Some(expected.as_str())
                || e.accessible_description().as_deref() == Some(expected.as_str())
        });
        assert!(
            shown,
            "the tree shows {expected:?}, so the sweep covered it"
        );
    }
}

/// rust-ui-dev F3: a refused send is a polite live region, like status and
/// connectivity.
#[test]
fn a_refused_send_is_announced() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    model.received(received(1, &alice, "hi"));
    let key = direct(&alice);
    open(&mut view, &mut model, &key);
    model.send_refused(
        key,
        "too much".to_owned(),
        &interweave_human_client_api::SendError::TooLarge,
    );
    view.render(&model);
    let reason = interweave_human_ui_model::fill(
        text(UiText::NotSent),
        &[(
            "reason",
            placeholder_en::error(interweave_human_ui_model::ErrorClass::TooLarge),
        )],
    );
    assert_eq!(
        the(&view, &reason).accessible_live_region(),
        Some(i_slint_backend_testing::AccessibleLiveness::Polite)
    );
}

/// rust-ui-dev R1, review N2: inputs resolve in the order they arrived.
/// Focus lost, B and C selected, focus regained: only C, shown when
/// focus came back, is read -- B never was on screen with focus.
#[test]
fn a_focus_change_never_overtakes_the_selections_after_it() {
    let mut view = view();
    let mut model = UiModel::new();
    let (a, b, c) = (peer(), peer(), peer());
    model.received(received(1, &a, "seen"));
    model.received(received(2, &b, "never shown with focus"));
    model.received(received(3, &c, "shown when focus returns"));
    view.set_window_focused(true);
    open(&mut view, &mut model, &direct(&a));
    model.read(RowId::from_stored(1));
    view.set_window_focused(false);
    view.select(direct(&b));
    view.select(direct(&c));
    view.set_window_focused(true);
    assert_eq!(
        intents(&mut view, &mut model),
        vec![Intent::MarkRead(RowId::from_stored(3))]
    );

    // The second trace: focus lost, B selected, focus lost again.
    let d = peer();
    model.received(received(4, &d, "still unread"));
    view.set_window_focused(false);
    view.select(direct(&d));
    view.set_window_focused(false);
    assert!(intents(&mut view, &mut model).is_empty(), "nothing read");
}

/// Review N1, its variant: a send queued between two edits carries the
/// first; the second reaches the model after it, and an older queued edit
/// never overwrites it.
#[test]
fn an_edit_never_overtakes_a_send_queued_before_it() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    model.received(received(1, &alice, "hi"));
    let key = direct(&alice);
    open(&mut view, &mut model, &key);
    type_into(&view, "before");
    view.window().invoke_send();
    for _ in 2..INPUT_CAP {
        view.select(key.clone());
    }
    type_into(&view, "before and after");
    view.render(&model);
    assert_eq!(view.window().get_draft().as_str(), "before and after");
    let sends: Vec<Intent> = intents(&mut view, &mut model)
        .into_iter()
        .filter(|i| matches!(i, Intent::Send { .. }))
        .collect();
    assert_eq!(
        sends,
        vec![Intent::Send {
            key: key.clone(),
            draft: "before".to_owned()
        }],
        "the send carries the text as it was when pressed"
    );
    assert_eq!(
        model.composer(&key).draft,
        "before and after",
        "then the newer text"
    );
    view.render(&model);
    assert_eq!(view.window().get_draft().as_str(), "before and after");
}

/// Review R2-3, N1's primary trace: the model already holds the draft
/// when Send is pressed, no older edit is queued, and the text typed after
/// the press must not reach the send.
#[test]
fn text_typed_after_a_send_never_reaches_it() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    model.received(received(1, &alice, "hi"));
    let key = direct(&alice);
    open(&mut view, &mut model, &key);
    type_into(&view, "as pressed");
    intents(&mut view, &mut model);
    assert_eq!(
        model.composer(&key).draft,
        "as pressed",
        "the model holds it"
    );
    view.window().invoke_send();
    for _ in 1..INPUT_CAP {
        view.select(key.clone());
    }
    type_into(&view, "as pressed, and more");
    let sends: Vec<Intent> = intents(&mut view, &mut model)
        .into_iter()
        .filter(|i| matches!(i, Intent::Send { .. }))
        .collect();
    assert_eq!(
        sends,
        vec![Intent::Send {
            key: key.clone(),
            draft: "as pressed".to_owned()
        }]
    );
    assert_eq!(model.composer(&key).draft, "as pressed, and more");
}

/// Review N4: the root's own calls do not run the wake hook -- a root
/// holding its view borrowed calls them safely -- and a window callback
/// does.
#[test]
fn only_window_input_runs_the_wake_hook() {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    let view = Rc::new(RefCell::new(view()));
    let model = UiModel::new();
    let alice = peer();
    let woken = Rc::new(Cell::new(0_u32));
    {
        let (weak, woken) = (Rc::downgrade(&view), Rc::clone(&woken));
        view.borrow().set_wake(move || {
            woken.set(woken.get() + 1);
            // A root that takes at once needs the view mutably.
            let view = weak.upgrade().expect("the view");
            drop(view.borrow_mut());
        });
    }
    view.borrow().select(direct(&alice));
    view.borrow().set_window_focused(true);
    view.borrow_mut().render(&model);
    assert_eq!(woken.get(), 0, "the root's own calls do not wake it");
    // The root takes: the selection now names the composer's conversation.
    let _ = view.borrow_mut().take_events(&model);
    let window = view.borrow().window().clone_strong();
    window.invoke_draft_edited("typed".into());
    assert_eq!(woken.get(), 1, "window input does");
}

/// The check asks the font stack's own loader: on a host with the library
/// it passes. NOT PROVED HERE: the failing half. The loader searches the
/// system's library paths, which a test cannot hide, so a host without
/// fontconfig is exercised only where one exists (the README says so).
#[test]
fn the_platform_check_passes_where_fontconfig_loads() {
    assert_eq!(interweave_human_ui_slint::platform_check(), Ok(()));
}

/// The open conversation's identifier is in the tree in exact canonical
/// form, as a read-only field a person selects and copies
/// (human-client-ui.md section 11); with no conversation open there is
/// none.
#[test]
fn the_open_conversations_identifier_is_exact_and_selectable() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    assert!(
        labelled(&view, text(UiText::ConversationId)).is_empty(),
        "no conversation, no identifier"
    );
    model.received(received(1, &alice, "hi"));
    open(&mut view, &mut model, &direct(&alice));
    let field = the(&view, text(UiText::ConversationId));
    assert_eq!(
        field.accessible_value().as_deref(),
        Some(alice.as_str()),
        "the PeerId, exact"
    );
    // Selectable: a text input, so a person can select and copy it.
    assert_eq!(
        field.accessible_role(),
        Some(i_slint_backend_testing::AccessibleRole::TextInput),
        "a field a person can select in"
    );
    // And read-only: an edit would drop the binding and leave an
    // identifier that is not the conversation's.
    assert_eq!(
        field.accessible_read_only(),
        Some(true),
        "the identifier cannot be edited"
    );
}

/// Whether the list item at `index` of `count` -- conversations when
/// `selectable`, messages otherwise -- lies inside the window. An item the
/// tree does not hold at all (a list culls rows outside its viewport) is
/// not in view.
fn item_in_view(view: &View, selectable: bool, index: usize, count: usize) -> bool {
    let window = view.window().window();
    let height = window.size().to_logical(window.scale_factor()).height;
    all(view).into_iter().any(|e| {
        e.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::ListItem)
            && (e.accessible_item_selectable() == Some(true)) == selectable
            && e.accessible_item_index() == Some(index)
            && e.accessible_item_count() == Some(count)
            && e.absolute_position().y >= 0.0
            && e.absolute_position().y + e.size().height <= height
    })
}

/// Tab until `prefix`'s rows have each held focus once: the last of them
/// in the tree holds it now.
fn tab_to_last(view: &View, prefix: &str, count: usize) {
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..(count * 4 + 20) {
        let id = tab(view);
        if id.starts_with(prefix) {
            seen.insert(id);
            if seen.len() == count {
                return;
            }
        }
    }
    panic!("Tab reached {} of {count} {prefix} rows", seen.len());
}

/// More conversations than the window holds: the list scrolls, each row
/// says its place in the list, and the row Tab reaches is brought into
/// view. Not shown here: that the window's minimum height stays put --
/// the testing backend keeps the window's size whatever the layout asks,
/// so a list without a scroller fails only the last assertion.
#[test]
fn a_long_conversation_list_scrolls_and_brings_the_focused_row_into_view() {
    const MANY: usize = 40;
    let mut view = view();
    let mut model = UiModel::new();
    let mut first = None;
    for n in 0..MANY {
        let from = peer();
        model.received(received(i64::try_from(n).expect("small") + 1, &from, "hi"));
        first.get_or_insert(from);
    }
    open(&mut view, &mut model, &direct(&first.expect("one")));
    assert!(
        item_in_view(&view, true, 0, MANY),
        "the first row is shown, as item 1 of {MANY}"
    );
    assert!(
        !item_in_view(&view, true, MANY - 1, MANY),
        "control: the last row starts outside the window"
    );
    tab_to_last(&view, "conversation:", MANY);
    assert!(
        item_in_view(&view, true, MANY - 1, MANY),
        "the focused last row is scrolled into view"
    );
}

/// The same for a long conversation's messages, whose list scrolled
/// already: the toolkit reveals the item that takes focus.
#[test]
fn a_long_conversation_brings_the_focused_message_into_view() {
    const MANY: usize = 40;
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    for n in 0..MANY {
        model.received(received(i64::try_from(n).expect("small") + 1, &alice, "hi"));
    }
    open(&mut view, &mut model, &direct(&alice));
    assert!(
        item_in_view(&view, false, 0, MANY),
        "the first message is shown, as item 1 of {MANY}"
    );
    assert!(
        !item_in_view(&view, false, MANY - 1, MANY),
        "control: the last message starts outside the window"
    );
    tab_to_last(&view, "item:", MANY);
    assert!(
        item_in_view(&view, false, MANY - 1, MANY),
        "the focused last message is scrolled into view"
    );
}

/// The window's two announcement slots' texts, as a screen reader is
/// handed them, each checked to be a polite live region.
fn slots(view: &View) -> [String; 2] {
    ["AppWindow::announce-a", "AppWindow::announce-b"].map(|id| {
        let mut found: Vec<ElementHandle> =
            ElementHandle::find_by_element_id(view.window(), id).collect();
        assert_eq!(found.len(), 1, "one {id}");
        let slot = found.remove(0);
        assert_eq!(
            slot.accessible_live_region(),
            Some(i_slint_backend_testing::AccessibleLiveness::Polite),
            "{id} is a polite live region"
        );
        slot.accessible_label()
            .map(|l| l.to_string())
            .unwrap_or_default()
    })
}

/// What the window's announcement says now: the one slot holding text,
/// or `None` when both are empty.
fn announced(view: &View) -> Option<String> {
    let said: Vec<String> = slots(view).into_iter().filter(|l| !l.is_empty()).collect();
    assert!(said.len() <= 1, "one announcement at a time: {said:?}");
    said.into_iter().next()
}

fn fill(template: UiText, values: &[(&str, &str)]) -> String {
    interweave_human_ui_model::fill(text(template), values)
}

/// F4: a whole conversation's items are not live regions, so opening one
/// says nothing, and a message arriving in it says who sent it -- never
/// what it says -- once.
#[test]
fn opening_a_conversation_announces_nothing_and_an_arrival_its_author() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    for row in 1..=20 {
        model.received(received(row, &alice, "earlier"));
    }
    view.render(&model);
    open(&mut view, &mut model, &direct(&alice));
    assert_eq!(announced(&view), None, "opening a list reads none of it");

    model.received(received(21, &alice, "secret words"));
    view.render(&model);
    let short = interweave_human_ui_model::short_peer(alice.as_str());
    let said = announced(&view).expect("an arrival is announced");
    assert_eq!(said, fill(UiText::AnnounceArrival, &[("author", &short)]));
    assert!(!said.contains("secret"), "never the text: {said}");

    let before = slots(&view);
    view.render(&model);
    assert_eq!(
        slots(&view),
        before,
        "a render with no change says nothing new"
    );
}

/// The same sentence twice is still heard twice: it moves to the other
/// slot, and the slot it leaves is cleared.
#[test]
fn the_same_announcement_twice_is_a_change_both_times() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    model.received(received(1, &alice, "first"));
    view.render(&model);
    open(&mut view, &mut model, &direct(&alice));

    model.received(received(2, &alice, "second"));
    view.render(&model);
    let once = slots(&view);
    model.received(received(3, &alice, "third"));
    view.render(&model);
    let twice = slots(&view);
    let said = announced(&view).expect("announced");
    assert_ne!(once, twice, "the sentence moved slots: {once:?} {twice:?}");
    assert!(once.contains(&said) && twice.contains(&said));
    assert!(
        once.contains(&String::new()) && twice.contains(&String::new()),
        "the slot left is cleared: {once:?} {twice:?}"
    );
}

/// Several arrivals at once are counted, an own message's status change
/// is said with its status, and both together are one announcement.
#[test]
fn arrivals_and_own_status_changes_are_one_announcement() {
    let mut view = view();
    let mut model = UiModel::new();
    let bob = peer();
    let id = sent(&mut model, 7, &bob, "to bob");
    view.render(&model);
    open(&mut view, &mut model, &direct(&bob));
    assert_eq!(announced(&view), None, "just sent: nothing to say");

    update(
        &mut model,
        7,
        &id,
        OutboundStatus::Accepted { endpoint: human() },
    );
    model.received(received(1, &bob, "one"));
    model.received(received(2, &bob, "two"));
    view.render(&model);
    let accepted =
        placeholder_en::label(interweave_human_ui_model::LabelKey::AcceptedByRemoteTransport);
    assert_eq!(
        announced(&view).expect("announced"),
        fill(
            UiText::AnnounceBoth,
            &[
                ("first", &fill(UiText::AnnounceArrivals, &[("count", "2")])),
                (
                    "rest",
                    &fill(UiText::AnnounceOwnStatus, &[("status", accepted)])
                ),
            ]
        )
    );
}

/// A message in a conversation not shown is announced by that
/// conversation's title.
#[test]
fn an_arrival_elsewhere_names_its_conversation() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    let carol = peer();
    model.received(received(1, &alice, "hi"));
    model.received(received(2, &carol, "hi"));
    view.render(&model);
    open(&mut view, &mut model, &direct(&alice));
    assert_eq!(announced(&view), None);

    model.received(received(3, &carol, "again"));
    view.render(&model);
    let title = model
        .conversations()
        .into_iter()
        .find(|c| c.key == direct(&carol))
        .expect("carol's")
        .title;
    assert_eq!(
        announced(&view).expect("announced"),
        fill(UiText::AnnounceElsewhere, &[("conversation", &title)])
    );
}

/// What the rendered window showed: a long unbroken word in the body
/// leaves the list no wider than the window (this fails without the
/// text's zero minimum width), and a long destination's control is taller
/// than a short one's. The overlap the winit window showed while the
/// control's touch area sat beside its layout is NOT reproduced here --
/// the testing backend sized the control either way -- so that rule is
/// held by the comment on `LinkButton` and a look at the window, not by
/// this test.
#[test]
fn long_content_wraps_inside_the_window() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    let long = "a".repeat(300);
    let destination = format!("https://example.org/plan?token={long}");
    model.received(received(
        1,
        &alice,
        &format!("word {long} end [plan]({destination}) [docs](https://example.org/docs)"),
    ));
    open(&mut view, &mut model, &direct(&alice));
    let window = view.window().window();
    let width = window.size().to_logical(window.scale_factor()).width;

    let long_link = the(&view, &open_link(&destination));
    let short_link = the(&view, &open_link("https://example.org/docs"));
    assert!(
        long_link.size().height >= 2.0 * short_link.size().height,
        "the long destination's control grew: {:?} against {:?}",
        long_link.size(),
        short_link.size()
    );
    assert!(
        short_link.absolute_position().y
            >= long_link.absolute_position().y + long_link.size().height,
        "the next control starts below it"
    );
    for element in all(&view) {
        let right = element.absolute_position().x + element.size().width;
        assert!(
            right <= width + 0.5,
            "{:?} reaches {right}, past the window's {width}",
            element.accessible_label()
        );
    }
}

/// A conversation pressed is shown by the take that resolves the press,
/// with no render after it: a root renders and then takes, so a press
/// that yields no intent -- a conversation with nothing unread, or the
/// window without focus -- would otherwise wait on screen for some later
/// event to render it (seen in the shipped client over AT-SPI).
#[test]
fn a_conversation_pressed_is_shown_by_the_take_alone() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    model.received(received(1, &alice, "shown at once"));
    view.render(&model);
    assert_eq!(
        labelled(&view, text(UiText::NoConversation)).len(),
        1,
        "control: nothing shown yet"
    );
    let row = all(&view)
        .into_iter()
        .find(|e| e.accessible_item_selectable() == Some(true))
        .expect("the conversation's row");
    row.invoke_accessible_default_action();
    let _ = intents(&mut view, &mut model);
    assert!(
        labelled(&view, text(UiText::NoConversation)).is_empty(),
        "the header names the conversation"
    );
    let short = interweave_human_ui_model::short_peer(alice.as_str());
    let unread = placeholder_en::label(interweave_human_ui_model::LabelKey::Unread);
    assert_eq!(
        labelled(&view, &format!("{short}, {unread}: shown at once")).len(),
        1,
        "its message is in the list"
    );
}

/// The window is handed the monospaced family code and the source are
/// drawn in: without it they fall back to the proportional default.
#[cfg(feature = "desktop")]
#[test]
fn the_window_is_given_a_family_for_code() {
    let view = view();
    assert!(
        !view.window().get_code_font().is_empty(),
        "the code font reaches the window"
    );
}

/// A destination carrying a right-to-left override is labelled with the
/// override shown as its code point, so the control reads in the order of
/// the string that opens -- and what opens is the destination as sent.
#[test]
fn a_destinations_hidden_characters_are_shown_on_its_control() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    let destination = "https://evil.example/#\u{202E}elpmaxe.knab//:sptth";
    model.received(received(1, &alice, &format!("[docs]({destination})")));
    open(&mut view, &mut model, &direct(&alice));
    let controls = buttons(&view);
    assert!(
        !controls.iter().any(|c| c.contains('\u{202E}')),
        "no control's label carries the override: {controls:?}"
    );
    let control = the(
        &view,
        &open_link("https://evil.example/#<U+202E>elpmaxe.knab//:sptth"),
    );
    control.invoke_accessible_default_action();
    assert_eq!(
        intents(&mut view, &mut model),
        [Intent::OpenLink(destination.to_owned())]
    );
}

/// A render that leaves an item's body unchanged keeps the focus on its
/// link's control: the drawn body is kept per item, as an item's actions
/// are, so the control is not rebuilt under the person's focus.
#[test]
fn a_render_keeps_focus_on_a_links_control() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    model.received(received(
        1,
        &alice,
        "[one](https://example.org/1) and [two](https://example.org/2)",
    ));
    open(&mut view, &mut model, &direct(&alice));
    let mut on = String::new();
    for _ in 0..20 {
        on = tab(&view);
        if on.starts_with("link:") {
            break;
        }
    }
    assert!(on.starts_with("link:"), "Tab reaches a link: {on}");
    let next = tab(&view);
    for _ in 0..40 {
        if tab(&view) == on {
            break;
        }
    }
    model.client_event(ClientEvent::Connectivity(Connectivity::OnlineRelay));
    view.render(&model);
    assert_eq!(tab(&view), next, "focus stayed on {on} across the render");
}

/// Section 7's route indicator: an open direct conversation says how it
/// is connected once the runtime has said, and a path change updates it
/// in place -- no new item, no announcement, no live region -- while a
/// disconnection takes it away.
#[test]
fn the_route_indicator_follows_the_path_in_place() {
    use interweave_transport_api::PeerPath;
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    model.received(received(1, &alice, "hello"));
    view.render(&model);
    open(&mut view, &mut model, &direct(&alice));
    let relayed = placeholder_en::path(PeerPath::Relayed);
    let direct_path = placeholder_en::path(PeerPath::Direct);
    assert!(
        labelled(&view, relayed).is_empty() && labelled(&view, direct_path).is_empty(),
        "nothing until the runtime says"
    );

    model.client_event(ClientEvent::PeerPath {
        peer: alice.clone(),
        path: PeerPath::Relayed,
    });
    view.render(&model);
    let shown = the(&view, relayed);
    assert_eq!(shown.accessible_live_region(), None, "not an interruption");

    let items = all(&view)
        .into_iter()
        .filter(|e| e.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::ListItem))
        .count();
    let said = announced(&view);
    model.client_event(ClientEvent::PeerPath {
        peer: alice.clone(),
        path: PeerPath::Direct,
    });
    view.render(&model);
    assert_eq!(labelled(&view, direct_path).len(), 1, "updated in place");
    assert!(labelled(&view, relayed).is_empty());
    assert_eq!(
        all(&view)
            .into_iter()
            .filter(
                |e| e.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::ListItem)
            )
            .count(),
        items,
        "no new item"
    );
    assert_eq!(announced(&view), said, "nothing announced");

    model.client_event(ClientEvent::PeerDisconnected { peer: alice });
    view.render(&model);
    assert!(labelled(&view, direct_path).is_empty(), "no longer known");
}

/// The source is shown in place of the drawn body, and a link's control is
/// part of the drawn body: while the source is shown it has none, as a
/// body past a bound has none, and it comes back with the drawn body.
#[test]
fn the_source_view_has_no_link_controls() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    let destination = "https://example.org/docs";
    model.received(received(1, &alice, &format!("see [docs]({destination})")));
    open(&mut view, &mut model, &direct(&alice));
    let control = open_link(destination);
    assert_eq!(labelled(&view, &control).len(), 1, "control: drawn first");
    the(&view, text(UiText::ShowSource)).invoke_accessible_default_action();
    assert!(
        labelled(&view, &control).is_empty(),
        "no link control beside the source"
    );
    the(&view, text(UiText::ShowFormatted)).invoke_accessible_default_action();
    assert_eq!(
        labelled(&view, &control).len(),
        1,
        "back with the drawn body"
    );
}
