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
//! this tree (no AccessKit adapter is in the graph until Stage 15), that
//! a live region is announced, contrast, text scaling, reduced motion,
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
/// input and counts it; the edit queued first survives, and a press
/// after a take works.
#[test]
fn a_full_queue_refuses_the_newest_and_keeps_the_edit() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    model.received(received(1, &alice, "hi"));
    let key = direct(&alice);
    open(&mut view, &mut model, &key);
    view.window().invoke_draft_edited("kept edit".into());
    for _ in 1..INPUT_CAP {
        view.select(key.clone());
    }
    assert_eq!(view.refused_inputs(), 0, "exactly at the bound");
    view.window().invoke_send();
    assert_eq!(view.refused_inputs(), 1, "the newest is refused, counted");
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
/// carry meaningful labels, roles and descriptions; status and
/// connectivity are polite live regions; the full `PeerId` is on the
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
    let short = format!("…{}", &id[id.len() - 8..]);
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

    let status = labelled(&view, unread)
        .into_iter()
        .find(|e| e.accessible_live_region().is_some())
        .expect("the status text");
    assert_eq!(
        status.accessible_live_region(),
        Some(i_slint_backend_testing::AccessibleLiveness::Polite)
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
    assert!(matches!(keep.as_slice(), [Intent::Keep(_)]), "{keep:?}");

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

/// U2d and §13 bullet 5: the body is the source as literal text -- no
/// link element, nothing a remote text can make a person or the view
/// activate -- and Stage 14 has no trust control at all (the trust
/// bullet is carried to Stage 15).
#[test]
fn no_link_and_no_trust_control_exists() {
    let mut view = view();
    let mut model = UiModel::new();
    let mallory = peer();
    let source = "[click me](https://example.org) and trust me: allowlist this peer";
    model.received(received(1, &mallory, source));
    view.set_window_focused(true);
    open(&mut view, &mut model, &direct(&mallory));
    let body = the(&view, source);
    assert_eq!(
        body.accessible_role(),
        Some(i_slint_backend_testing::AccessibleRole::Text)
    );
    assert!(labelled(&view, "click me").is_empty(), "no link element");
    let controls: Vec<String> = all(&view)
        .into_iter()
        .filter(|e| e.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Button))
        .filter_map(|e| e.accessible_label().map(|l| l.to_string()))
        .collect();
    for control in &controls {
        assert!(
            [text(UiText::Keep), text(UiText::Send)].contains(&control.as_str()),
            "only known actions are controls: {controls:?}"
        );
    }
    body.invoke_accessible_default_action();
    let raised = intents(&mut view, &mut model);
    assert!(
        raised.iter().all(|i| matches!(i, Intent::MarkRead(_))),
        "remote text raises nothing but the focused read: {raised:?}"
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

/// rust-ui-dev F2b: an edit a full queue refused is still reported --
/// first, with the window's text -- and a render meanwhile does not erase
/// it.
#[test]
fn an_edit_a_full_queue_refused_is_reported_first() {
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
    assert_eq!(view.refused_inputs(), 1, "the edit was refused");
    view.render(&model);
    assert_eq!(view.window().get_draft().as_str(), "typed while full");
    assert_eq!(
        view.take_events(&model),
        vec![ViewEvent::DraftChanged {
            key: key.clone(),
            draft: "typed while full".to_owned()
        }]
    );
    model.draft_changed(key.clone(), "typed while full".to_owned());
    intents(&mut view, &mut model);
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

/// The queue's bound: presses up to the cap, then any number of focus
/// changes and renders of an unread conversation add at most one of each.
#[test]
fn the_queue_never_holds_more_than_the_cap_plus_two() {
    let mut view = view();
    let mut model = UiModel::new();
    let alice = peer();
    model.received(received(1, &alice, "unread"));
    open(&mut view, &mut model, &direct(&alice));
    for _ in 0..INPUT_CAP * 2 {
        view.select(direct(&alice));
    }
    for n in 0..INPUT_CAP * 2 {
        view.set_window_focused(n % 2 == 0);
        view.render(&model);
    }
    assert_eq!(view.queued_inputs(), INPUT_CAP + 2);
    assert_eq!(
        view.refused_inputs(),
        u64::try_from(INPUT_CAP).expect("small"),
        "only presses past the cap are refused"
    );
}
