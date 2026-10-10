// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The UI model's agreed behaviours (relay seqs 10630, 10639, 10642;
//! amended 10707, 10710, 10713),
//! through its public surface.

#![allow(clippy::expect_used, clippy::panic)]

use interweave_human_chat_protocol::{HumanChatV2, MessageKind};
use interweave_human_client_api::{
    ClientEvent, Connectivity, Destination, Origin, OutboundStatus, OutboundUpdate, SendError,
    SessionProblem, SessionState,
};
use interweave_human_core::{AppMessageId, RowId};
use interweave_human_ui_model::{
    ConversationKey, ErrorClass, Intent, LabelKey, ListedInbound, Reply, Retention,
    SESSION_ITEM_CAP, SessionNotice, Table, UiModel,
};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{EndpointId, TransportIdentity};

fn peer() -> TransportIdentity {
    ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id")
}

fn endpoint(name: &str) -> EndpointId {
    EndpointId::parse(name).expect("valid")
}

fn envelope(id: u32, text: &str) -> HumanChatV2 {
    HumanChatV2 {
        v: 2,
        kind: MessageKind::Text,
        app_message_id: format!("{id:032x}"),
        text: text.to_owned(),
        reply_to: None,
        sent_at_ms: None,
        from_endpoint: None,
    }
}

fn listed(row: i64, from: &TransportIdentity, envelope: HumanChatV2, at: u64) -> ListedInbound {
    ListedInbound {
        row: RowId::from_stored(row),
        origin: Origin::Direct {
            peer: from.clone(),
            endpoint: endpoint("human"),
        },
        envelope,
        received_at: at,
    }
}

fn key(from: &TransportIdentity) -> ConversationKey {
    ConversationKey::Direct {
        peer: from.clone(),
        endpoint: Some(endpoint("human")),
    }
}

#[test]
fn messages_are_ordered_by_local_time_never_by_the_senders_clock() {
    let p = peer();
    let mut model = UiModel::new();
    let mut early_claim = envelope(1, "arrived second");
    early_claim.sent_at_ms = Some(1);
    let mut late_claim = envelope(2, "arrived first");
    late_claim.sent_at_ms = Some(9_999_999);
    model.unread_listed(vec![
        listed(1, &p, early_claim, 200),
        listed(2, &p, late_claim, 100),
    ]);
    let order: Vec<_> = model
        .messages(&key(&p))
        .into_iter()
        .map(|m| m.source)
        .collect();
    assert_eq!(order, ["arrived first", "arrived second"]);
}

#[test]
fn a_direct_title_is_the_short_peer_id_and_route_label_never_the_envelopes_from_endpoint() {
    let p = peer();
    let mut model = UiModel::new();
    let mut claim = envelope(1, "I am Alice, your bank");
    claim.from_endpoint = Some(endpoint("alice"));
    model.unread_listed(vec![listed(1, &p, claim, 1)]);
    let [summary] = model.conversations().try_into().expect("one conversation");
    let id = p.as_str();
    assert!(
        summary.title.contains(&id[id.len() - 8..]),
        "{}",
        summary.title
    );
    assert!(summary.title.contains("human"), "the route label");
    assert!(!summary.title.to_ascii_lowercase().contains("alice"));
    assert_eq!(summary.unread, 1);
}

#[test]
fn read_is_raised_only_while_the_conversation_is_viewed_with_focus() {
    let p = peer();
    let mut model = UiModel::new();
    model.unread_listed(vec![listed(7, &p, envelope(1, "x"), 1)]);
    assert!(
        model.conversation_viewed(&key(&p), false).is_empty(),
        "never while unfocused"
    );
    assert_eq!(
        model.conversation_viewed(&key(&p), true),
        [Intent::MarkRead(RowId::from_stored(7))]
    );
}

#[test]
fn keep_is_offered_only_after_read_and_unkeep_only_when_kept() {
    let p = peer();
    let mut model = UiModel::new();
    model.unread_listed(vec![listed(7, &p, envelope(1, "x"), 1)]);
    let item = model.messages(&key(&p))[0].key;
    assert!(
        model.actions(item).is_empty(),
        "no MarkRead from actions: only a focused view raises it"
    );
    model.read(RowId::from_stored(7));
    assert_eq!(
        model.actions(item),
        [Intent::Keep {
            item,
            from: (Table::Unread, RowId::from_stored(7))
        }],
        "Keep names the read that handed the content back"
    );
    assert_eq!(model.messages(&key(&p))[0].label, LabelKey::ReadNotKept);
    model.kept(item, RowId::from_stored(70));
    assert_eq!(
        model.actions(item),
        [Intent::Unkeep(RowId::from_stored(70))]
    );
    assert_eq!(
        model.messages(&key(&p))[0].key,
        item,
        "the item keeps its key"
    );
    model.unkept(RowId::from_stored(70));
    assert_eq!(
        model.actions(item),
        [Intent::Keep {
            item,
            from: (Table::Kept, RowId::from_stored(70))
        }],
        "after Unkeep, Keep names the unkeep's copy (agreed Q6)"
    );
}

/// An edit is told by when it happened, not by what it says: the same
/// text typed again after the press is the next message and survives the
/// commit, and a composer cleared after the press stays clear through a
/// refusal (#176 review, threads on `sent` and `send_refused`).
#[test]
fn an_answer_to_a_send_tells_an_edit_by_when_it_happened_not_by_its_text() {
    let p = peer();
    let k = key(&p);
    let destination = Destination::Direct {
        peer: p,
        endpoint: Some(endpoint("human")),
    };

    let mut model = UiModel::new();
    model.draft_changed(k.clone(), "yes".to_owned());
    model.send_pressed(&k);
    model.draft_changed(k.clone(), String::new());
    model.draft_changed(k.clone(), "yes".to_owned());
    model.sent(RowId::from_stored(1), &destination, envelope(1, "yes"), 5);
    assert_eq!(
        model.composer(&k).draft,
        "yes",
        "the same text typed again after the press is kept"
    );

    let mut model = UiModel::new();
    model.draft_changed(k.clone(), "yes".to_owned());
    model.send_pressed(&k);
    model.sent(RowId::from_stored(1), &destination, envelope(1, "yes"), 5);
    assert_eq!(
        model.composer(&k).draft,
        "",
        "unedited since the press: cleared"
    );

    let mut model = UiModel::new();
    model.draft_changed(k.clone(), "too long".to_owned());
    model.send_pressed(&k);
    model.draft_changed(k.clone(), String::new());
    model.send_refused(k.clone(), "too long".to_owned(), &SendError::TooLarge);
    let composer = model.composer(&k);
    assert_eq!(
        composer.draft, "",
        "a composer cleared after the press stays clear"
    );
    assert_eq!(composer.refused, Some(ErrorClass::TooLarge), "and says why");
}

/// A press is answered once: after its answer, a stale second answer with
/// no new press and no edit in between touches nothing.
#[test]
fn a_press_is_answered_once() {
    let p = peer();
    let k = key(&p);
    let destination = Destination::Direct {
        peer: p,
        endpoint: Some(endpoint("human")),
    };
    let mut model = UiModel::new();
    model.draft_changed(k.clone(), "words".to_owned());
    model.send_pressed(&k);
    model.send_refused(k.clone(), "words".to_owned(), &SendError::TooLarge);
    assert_eq!(model.composer(&k).draft, "words", "unedited: still there");
    model.sent(RowId::from_stored(2), &destination, envelope(2, "words"), 6);
    assert_eq!(
        model.composer(&k).draft,
        "words",
        "the press was answered by the refusal: no clear"
    );
}

/// With no recorded press, a refusal puts its text back only into an
/// empty composer, never over text the composer holds.
#[test]
fn a_refusal_with_no_press_never_overwrites_a_composer_holding_text() {
    let p = peer();
    let k = key(&p);
    let mut model = UiModel::new();
    model.draft_changed(k.clone(), "mine".to_owned());
    model.send_refused(k.clone(), "theirs".to_owned(), &SendError::TooLarge);
    let composer = model.composer(&k);
    assert_eq!(composer.draft, "mine");
    assert_eq!(composer.refused, Some(ErrorClass::TooLarge), "and says why");
}

#[test]
fn keep_is_not_offered_once_the_root_has_dropped_its_copy() {
    let p = peer();
    let mut model = UiModel::new();
    model.unread_listed(vec![listed(7, &p, envelope(1, "x"), 1)]);
    let item = model.messages(&key(&p))[0].key;
    model.read(RowId::from_stored(7));
    assert_eq!(model.actions(item).len(), 1, "Keep, while the copy is held");
    model.copy_gone(item);
    assert!(model.actions(item).is_empty(), "{:?}", model.actions(item));
    assert_eq!(
        model.messages(&key(&p))[0].label,
        LabelKey::ReadNotKept,
        "still shown, read"
    );
}

#[test]
fn a_refused_send_keeps_the_draft_and_says_why_until_it_is_edited_or_sent() {
    let p = peer();
    let k = key(&p);
    let mut model = UiModel::new();
    model.send_refused(k.clone(), "my words".to_owned(), &SendError::TooLarge);
    let composer = model.composer(&k);
    assert_eq!(composer.draft, "my words");
    assert_eq!(composer.refused, Some(ErrorClass::TooLarge));
    model.draft_changed(k.clone(), "shorter".to_owned());
    assert_eq!(model.composer(&k).refused, None);
    assert_eq!(
        model.send_draft(&k),
        Some(Intent::Send {
            key: k.clone(),
            draft: "shorter".to_owned()
        })
    );
    model.send_pressed(&k);
    model.sent(
        RowId::from_stored(1),
        &Destination::Direct {
            peer: p,
            endpoint: Some(endpoint("human")),
        },
        envelope(1, "shorter"),
        5,
    );
    assert_eq!(model.composer(&k).draft, "", "cleared once sent");
}

/// The facade answers a send after the press. Text typed in between is
/// the person's next message: a commit does not clear it, and a refusal
/// does not put the older text back over it.
#[test]
fn an_answer_to_a_send_never_replaces_text_typed_after_the_press() {
    let p = peer();
    let k = key(&p);
    let destination = Destination::Direct {
        peer: p,
        endpoint: Some(endpoint("human")),
    };

    let mut model = UiModel::new();
    model.draft_changed(k.clone(), "first".to_owned());
    model.send_pressed(&k);
    // Pressed with "first"; the person types on before the commit lands.
    model.draft_changed(k.clone(), "second".to_owned());
    model.sent(RowId::from_stored(1), &destination, envelope(1, "first"), 5);
    assert_eq!(
        model.composer(&k).draft,
        "second",
        "a commit keeps newer text"
    );

    let mut model = UiModel::new();
    model.draft_changed(k.clone(), "too long".to_owned());
    model.send_pressed(&k);
    model.draft_changed(k.clone(), "short".to_owned());
    model.send_refused(k.clone(), "too long".to_owned(), &SendError::TooLarge);
    let composer = model.composer(&k);
    assert_eq!(composer.draft, "short", "a refusal keeps newer text");
    assert_eq!(
        composer.refused,
        Some(ErrorClass::TooLarge),
        "and still says why"
    );
}

#[test]
fn each_session_notice_carries_the_intent_that_resolves_it() {
    let mut model = UiModel::new();
    model.client_event(ClientEvent::Session(SessionState::StorageDegraded));
    assert_eq!(model.session_notice(), Some(SessionNotice::StorageDegraded));
    assert_eq!(
        SessionNotice::StorageDegraded.resolution(),
        Some(Intent::RecheckStorage)
    );
    model.client_event(ClientEvent::Session(SessionState::Refused {
        problem: SessionProblem::EndpointInUse,
    }));
    assert_eq!(
        model.session_notice(),
        Some(SessionNotice::Refused(ErrorClass::EndpointInUse))
    );
    assert_eq!(
        SessionNotice::Refused(ErrorClass::EndpointInUse).resolution(),
        Some(Intent::Reopen)
    );
    model.client_event(ClientEvent::Session(SessionState::Ready { endpoint: None }));
    assert_eq!(model.session_notice(), None);
    // Connectivity is its own value, and storage trouble is never hidden
    // behind "online".
    model.client_event(ClientEvent::Connectivity(Connectivity::OnlineDirect));
    model.client_event(ClientEvent::Session(SessionState::StorageDegraded));
    assert_eq!(model.connectivity(), Connectivity::OnlineDirect);
    assert_eq!(model.session_notice(), Some(SessionNotice::StorageDegraded));
}

/// No daemon replaces "reconnecting" only: storage trouble is still said
/// as it is, and a ready session shows no notice.
#[test]
fn no_daemon_is_said_in_place_of_reconnecting_only() {
    let mut model = UiModel::new();
    model.client_event(ClientEvent::Session(SessionState::Reconnecting {
        attempt: 1,
        next_at: 0,
    }));
    assert_eq!(model.session_notice(), Some(SessionNotice::Reconnecting));
    model.daemon_seen(Some(false));
    assert_eq!(model.session_notice(), Some(SessionNotice::NoDaemon));
    assert_eq!(SessionNotice::NoDaemon.resolution(), None, "guidance only");
    model.client_event(ClientEvent::Session(SessionState::StorageDegraded));
    assert_eq!(model.session_notice(), Some(SessionNotice::StorageDegraded));
    model.client_event(ClientEvent::Session(SessionState::Ready { endpoint: None }));
    assert_eq!(model.session_notice(), None);
    model.daemon_seen(Some(false));
    model.client_event(ClientEvent::Session(SessionState::Reconnecting {
        attempt: 2,
        next_at: 0,
    }));
    assert_eq!(model.session_notice(), Some(SessionNotice::NoDaemon));
    model.daemon_seen(None);
    assert_eq!(
        model.session_notice(),
        Some(SessionNotice::Reconnecting),
        "cannot tell clears the no-daemon guidance"
    );
    model.daemon_seen(Some(true));
    model.client_event(ClientEvent::Session(SessionState::Reconnecting {
        attempt: 2,
        next_at: 0,
    }));
    assert_eq!(model.session_notice(), Some(SessionNotice::Reconnecting));
}

/// With no network access the window says so whatever the facade
/// reports, offers the person the way to allow it, and offers no send;
/// reading and keeping stay local and go on (human-client-android.md,
/// "Runtime permissions and the network-denied state").
#[test]
fn network_denied_is_said_first_and_offers_no_send() {
    let mut model = UiModel::new();
    let k = key(&peer());
    model.draft_changed(k.clone(), "yes".to_owned());
    assert!(model.sending_offered(), "the control: access by default");
    assert!(model.send_draft(&k).is_some(), "the control: a draft sends");

    model.network_access(false);
    assert_eq!(model.session_notice(), Some(SessionNotice::NetworkDenied));
    assert_eq!(
        SessionNotice::NetworkDenied.resolution(),
        Some(Intent::AllowNetwork)
    );
    assert!(!model.sending_offered());
    assert_eq!(model.send_draft(&k), None, "no send while denied");
    assert_eq!(model.composer(&k).draft, "yes", "the draft is kept");
    for state in [
        SessionState::Ready { endpoint: None },
        SessionState::StorageDegraded,
        SessionState::Reconnecting {
            attempt: 1,
            next_at: 0,
        },
    ] {
        model.client_event(ClientEvent::Session(state.clone()));
        assert_eq!(
            model.session_notice(),
            Some(SessionNotice::NetworkDenied),
            "said over {state:?}"
        );
    }
    model.daemon_seen(Some(false));
    assert_eq!(model.session_notice(), Some(SessionNotice::NetworkDenied));

    model.network_access(true);
    model.daemon_seen(Some(true));
    assert_eq!(model.session_notice(), Some(SessionNotice::Reconnecting));
    assert!(model.send_draft(&k).is_some(), "allowed again, it sends");
}

#[test]
fn unknown_connectivity_stays_unknown() {
    let mut model = UiModel::new();
    assert_eq!(model.connectivity(), Connectivity::Unknown);
    model.client_event(ClientEvent::Connectivity(Connectivity::Unknown));
    assert_ne!(model.connectivity(), Connectivity::Offline);
}

#[test]
fn a_re_sent_copy_after_read_is_one_item_and_its_row_is_still_read_when_viewed() {
    // The store commits a re-send after a read as a new unread row: it is
    // attached, never hidden (agreed U1).
    let p = peer();
    let mut model = UiModel::new();
    model.unread_listed(vec![listed(1, &p, envelope(5, "once"), 1)]);
    model.read(RowId::from_stored(1));
    model.unread_listed(vec![listed(2, &p, envelope(5, "once"), 2)]);
    let items = model.messages(&key(&p));
    assert_eq!(items.len(), 1, "one message");
    assert_eq!(items[0].label, LabelKey::Unread, "its new row is unread");
    assert_eq!(model.conversations()[0].unread, 1);
    assert_eq!(
        model.conversation_viewed(&key(&p), true),
        [Intent::MarkRead(RowId::from_stored(2))]
    );
}

#[test]
fn a_re_sent_copy_of_a_kept_message_shows_both_in_either_listing_order() {
    for kept_first in [true, false] {
        let p = peer();
        let mut model = UiModel::new();
        let kept = vec![listed(10, &p, envelope(5, "kept text"), 1)];
        let unread = vec![listed(20, &p, envelope(5, "kept text"), 2)];
        if kept_first {
            model.kept_listed(kept);
            model.unread_listed(unread);
        } else {
            model.unread_listed(unread);
            model.kept_listed(kept);
        }
        let items = model.messages(&key(&p));
        assert_eq!(items.len(), 1, "kept first: {kept_first}");
        assert_eq!(items[0].label, LabelKey::Unread);
        assert!(items[0].kept, "and still kept");
        assert_eq!(
            model.conversation_viewed(&key(&p), true),
            [Intent::MarkRead(RowId::from_stored(20))]
        );
        model.read(RowId::from_stored(20));
        let after = &model.messages(&key(&p))[0];
        assert_eq!(after.label, LabelKey::Kept, "reading leaves the kept row");
        assert_eq!(
            model.actions(after.key),
            [Intent::Unkeep(RowId::from_stored(10))],
            "Unkeep reachable"
        );
    }
}

#[test]
fn the_same_id_with_different_text_is_a_new_item_never_dropped() {
    // The id is the peer's to choose: new text under an old id is shown
    // (agreed U1a).
    let p = peer();
    let mut model = UiModel::new();
    model.unread_listed(vec![
        listed(1, &p, envelope(5, "first text"), 1),
        listed(2, &p, envelope(5, "other text"), 2),
    ]);
    let texts: Vec<_> = model
        .messages(&key(&p))
        .into_iter()
        .map(|m| m.source)
        .collect();
    assert_eq!(texts, ["first text", "other text"]);
}

#[test]
fn the_same_row_listed_again_is_the_same_item() {
    let p = peer();
    let mut model = UiModel::new();
    model.unread_listed(vec![listed(1, &p, envelope(5, "x"), 1)]);
    model.unread_listed(vec![listed(1, &p, envelope(5, "x"), 1)]);
    assert_eq!(model.len(), 1);
}

#[test]
fn session_only_items_are_capped_oldest_first_and_unread_is_never_evicted() {
    let p = peer();
    let mut model = UiModel::new();
    let total = SESSION_ITEM_CAP + 1;
    let rows: Vec<ListedInbound> = (0..total)
        .map(|n| {
            let n32 = u32::try_from(n).expect("small");
            listed(
                i64::from(n32),
                &p,
                envelope(n32, &format!("m{n}")),
                u64::from(n32),
            )
        })
        .collect();
    model.unread_listed(rows);
    let keep_unread = listed(1_000_000, &p, envelope(999_999, "still unread"), 0);
    model.unread_listed(vec![keep_unread]);
    for n in 0..SESSION_ITEM_CAP {
        model.read(RowId::from_stored(i64::try_from(n).expect("small")));
    }
    assert_eq!(model.len(), total + 1, "at the cap, nothing evicted");
    model.read(RowId::from_stored(
        i64::try_from(SESSION_ITEM_CAP).expect("small"),
    ));
    assert_eq!(model.len(), total, "one past the cap, one evicted");
    let sources: Vec<_> = model
        .messages(&key(&p))
        .into_iter()
        .map(|m| m.source)
        .collect();
    assert!(!sources.contains(&"m0".to_owned()), "the oldest went");
    assert!(sources.contains(&"still unread".to_owned()), "unread stays");
}

#[test]
fn a_reply_is_linked_when_held_and_neutral_when_not() {
    let p = peer();
    let mut model = UiModel::new();
    model.unread_listed(vec![listed(1, &p, envelope(1, "original"), 1)]);
    let mut held = envelope(2, "re: held");
    held.reply_to = Some(format!("{:032x}", 1));
    let mut missing = envelope(3, "re: gone");
    missing.reply_to = Some("11111111111111111111111111111111".to_owned());
    model.unread_listed(vec![listed(2, &p, held, 2), listed(3, &p, missing, 3)]);
    let items = model.messages(&key(&p));
    assert!(matches!(items[1].reply, Some(Reply::Present(k)) if k == items[0].key));
    // Resolved within the conversation: the same id elsewhere is not it.
    let other = peer();
    let mut stray = envelope(4, "re: from elsewhere");
    stray.reply_to = Some(format!("{:032x}", 1));
    model.unread_listed(vec![listed(4, &other, stray, 4)]);
    assert_eq!(
        model.messages(&key(&other))[0].reply,
        Some(Reply::Unavailable(format!("{:032x}", 1)))
    );
    assert_eq!(
        items[2].reply,
        Some(Reply::Unavailable(
            "11111111111111111111111111111111".to_owned()
        ))
    );
}

#[test]
fn an_accepted_outbound_is_terminal_with_no_actions_and_its_own_label() {
    let p = peer();
    let mut model = UiModel::new();
    let row = RowId::from_stored(1);
    model.sent(
        row,
        &Destination::Direct {
            peer: p.clone(),
            endpoint: Some(endpoint("human")),
        },
        envelope(1, "x"),
        1,
    );
    let item = model.messages(&key(&p))[0].key;
    assert_eq!(
        model.actions(item),
        [Intent::Retry(row), Intent::Cancel(row)]
    );
    model.client_event(ClientEvent::Outbound(OutboundUpdate {
        row,
        app_message_id: AppMessageId::parse(format!("{:032x}", 1)).expect("id"),
        status: OutboundStatus::Accepted {
            endpoint: endpoint("human"),
        },
        last_code: None,
    }));
    assert!(model.actions(item).is_empty());
    assert_eq!(
        model.messages(&key(&p))[0].label,
        LabelKey::AcceptedByRemoteTransport
    );
}

#[test]
fn an_inbound_item_carries_its_retention_state_and_raw_source() {
    let p = peer();
    let mut model = UiModel::new();
    model.unread_listed(vec![listed(1, &p, envelope(1, "**bold**"), 1)]);
    let item = &model.messages(&key(&p))[0];
    assert_eq!(item.source, "**bold**", "raw source always viewable");
    assert!(matches!(
        item.status,
        interweave_human_ui_model::ItemStatus::Inbound(Retention::Unread)
    ));
    assert_eq!(item.author.as_ref(), Some(&p), "the authenticated sender");
}

#[test]
fn an_outbound_rows_raw_code_is_on_the_diagnostics_surface_not_the_item() {
    let p = peer();
    let mut model = UiModel::new();
    let row = RowId::from_stored(1);
    model.sent(
        row,
        &Destination::Direct {
            peer: p.clone(),
            endpoint: Some(endpoint("human")),
        },
        envelope(1, "x"),
        1,
    );
    model.client_event(ClientEvent::Outbound(OutboundUpdate {
        row,
        app_message_id: AppMessageId::parse(format!("{:032x}", 1)).expect("id"),
        status: OutboundStatus::NeedsAttention {
            problem: interweave_human_client_api::SendProblem::Internal,
            may_have_reached: true,
        },
        last_code: Some(interweave_transport_api::TransportError::Internal),
    }));
    let item = model.messages(&key(&p))[0].key;
    assert_eq!(
        model.item_diagnostics(item).and_then(|d| d.last_code),
        Some(interweave_transport_api::TransportError::Internal)
    );
}

fn update(row: i64, status: OutboundStatus) -> ClientEvent {
    ClientEvent::Outbound(OutboundUpdate {
        row: RowId::from_stored(row),
        app_message_id: AppMessageId::parse(format!("{row:032x}")).expect("id"),
        status,
        last_code: None,
    })
}

#[test]
fn an_update_before_its_row_is_listed_is_held_and_applied_when_listed() {
    // Agreed U3: no order is required of the root.
    let p = peer();
    let mut model = UiModel::new();
    model.client_event(update(
        3,
        OutboundStatus::Unconfirmed {
            next_retry_at: None,
            last_problem: None,
        },
    ));
    assert_eq!(model.held_updates(), 1);
    model.pending_listed(vec![interweave_human_ui_model::ListedOutbound {
        row: RowId::from_stored(3),
        destination: Destination::Direct {
            peer: p.clone(),
            endpoint: Some(endpoint("human")),
        },
        envelope: envelope(3, "x"),
        created_at: 1,
    }]);
    assert_eq!(model.messages(&key(&p))[0].label, LabelKey::NotConfirmed);
    assert_eq!(model.held_updates(), 0);
}

#[test]
fn listing_discards_held_updates_for_rows_it_does_not_list() {
    let mut model = UiModel::new();
    model.client_event(update(8, OutboundStatus::Published));
    model.pending_listed(vec![]);
    assert_eq!(model.held_updates(), 0, "authoritative (agreed U3a)");
}

#[test]
fn held_updates_are_capped_latest_per_row_oldest_dropped() {
    let mut model = UiModel::new();
    let cap = interweave_human_ui_model::HELD_UPDATE_CAP;
    for row in 0..cap {
        model.client_event(update(
            i64::try_from(row).expect("small"),
            OutboundStatus::Published,
        ));
    }
    assert_eq!(model.held_updates(), cap, "at the cap");
    model.client_event(update(0, OutboundStatus::Published));
    assert_eq!(model.held_updates(), cap, "the same row again replaces");
    model.client_event(update(
        i64::try_from(cap).expect("small"),
        OutboundStatus::Published,
    ));
    assert_eq!(
        model.held_updates(),
        cap,
        "one past the cap drops the oldest"
    );
}

#[test]
fn a_reply_to_an_id_two_messages_carry_is_unavailable_not_a_guess() {
    // The id is the peer's to choose, so two messages in one conversation
    // can carry it (new text under an old id); a reply is not linked to
    // either.
    let p = peer();
    let mut model = UiModel::new();
    model.unread_listed(vec![
        listed(1, &p, envelope(5, "first text"), 1),
        listed(2, &p, envelope(5, "second text"), 2),
    ]);
    let mut reply = envelope(6, "re: which?");
    reply.reply_to = Some(format!("{:032x}", 5));
    model.unread_listed(vec![listed(3, &p, reply, 3)]);
    assert_eq!(
        model.messages(&key(&p))[2].reply,
        Some(Reply::Unavailable(format!("{:032x}", 5)))
    );
}

#[test]
fn a_held_update_keeps_the_latest_per_row_and_the_cap_drops_the_oldest() {
    let p = peer();
    let listed_out = |row: i64| interweave_human_ui_model::ListedOutbound {
        row: RowId::from_stored(row),
        destination: Destination::Direct {
            peer: p.clone(),
            endpoint: Some(endpoint("human")),
        },
        envelope: envelope(u32::try_from(row).expect("small"), "x"),
        created_at: 1,
    };
    // Latest wins per row.
    let mut model = UiModel::new();
    model.client_event(update(
        1,
        OutboundStatus::Sending {
            attempts: 1,
            next_retry_at: None,
            last_problem: None,
        },
    ));
    model.client_event(update(1, OutboundStatus::Published));
    model.pending_listed(vec![listed_out(1)]);
    assert_eq!(
        model.messages(&key(&p))[0].label,
        LabelKey::PublishedLocally
    );
    // Past the cap, the OLDEST row's update goes and the newest stays.
    let cap = interweave_human_ui_model::HELD_UPDATE_CAP;
    let mut model = UiModel::new();
    for row in 0..=cap {
        model.client_event(update(
            i64::try_from(row).expect("small"),
            OutboundStatus::Published,
        ));
    }
    let newest = i64::try_from(cap).expect("small");
    model.sent(
        RowId::from_stored(0),
        &Destination::Direct {
            peer: p.clone(),
            endpoint: Some(endpoint("human")),
        },
        envelope(0, "oldest"),
        1,
    );
    model.sent(
        RowId::from_stored(newest),
        &Destination::Direct {
            peer: p.clone(),
            endpoint: Some(endpoint("human")),
        },
        envelope(u32::try_from(newest).expect("small"), "newest"),
        2,
    );
    let labels: Vec<_> = model
        .messages(&key(&p))
        .into_iter()
        .map(|m| (m.source, m.label))
        .collect();
    assert_eq!(
        labels,
        [
            ("oldest".to_owned(), LabelKey::Sending),
            ("newest".to_owned(), LabelKey::PublishedLocally),
        ]
    );
}

#[test]
fn a_copy_attached_to_a_read_item_keeps_it_from_eviction() {
    // The copy's unread row must stay reachable however many items pass.
    let p = peer();
    let mut model = UiModel::new();
    model.unread_listed(vec![listed(1, &p, envelope(5, "copied"), 0)]);
    model.read(RowId::from_stored(1));
    model.unread_listed(vec![listed(2, &p, envelope(5, "copied"), 1)]);
    let filler = interweave_human_ui_model::SESSION_ITEM_CAP + 1;
    for n in 0..filler {
        let n32 = u32::try_from(n).expect("small") + 100;
        let row = i64::from(n32) + 1_000;
        model.unread_listed(vec![listed(row, &p, envelope(n32, &format!("f{n}")), 2)]);
        model.read(RowId::from_stored(row));
    }
    assert!(
        model
            .conversation_viewed(&key(&p), true)
            .contains(&Intent::MarkRead(RowId::from_stored(2))),
        "the copy's unread row is still offered"
    );
}

#[test]
fn a_first_text_seen_again_after_another_attaches_to_its_own_item() {
    let p = peer();
    let mut model = UiModel::new();
    model.unread_listed(vec![
        listed(1, &p, envelope(5, "A"), 1),
        listed(2, &p, envelope(5, "B"), 2),
        listed(3, &p, envelope(5, "A"), 3),
    ]);
    let texts: Vec<_> = model
        .messages(&key(&p))
        .into_iter()
        .map(|m| m.source)
        .collect();
    assert_eq!(texts, ["A", "B"], "A, B, A is two messages");
}

#[test]
fn a_stale_listing_does_not_bring_back_a_row_already_read() {
    let p = peer();
    let mut model = UiModel::new();
    let snapshot = vec![listed(1, &p, envelope(5, "x"), 1)];
    model.unread_listed(snapshot.clone());
    model.read(RowId::from_stored(1));
    model.unread_listed(snapshot);
    assert_eq!(model.messages(&key(&p))[0].label, LabelKey::ReadNotKept);
    assert!(model.conversation_viewed(&key(&p), true).is_empty());
}

#[test]
fn eviction_leaves_no_index_entry_behind() {
    let p = peer();
    let mut model = UiModel::new();
    let total = interweave_human_ui_model::SESSION_ITEM_CAP * 2;
    for n in 0..total {
        let n32 = u32::try_from(n).expect("small");
        model.unread_listed(vec![listed(i64::from(n32), &p, envelope(n32, "x"), 1)]);
        model.read(RowId::from_stored(i64::from(n32)));
    }
    assert_eq!(model.len(), interweave_human_ui_model::SESSION_ITEM_CAP);
    assert_eq!(
        model.indexed_ids(),
        interweave_human_ui_model::SESSION_ITEM_CAP,
        "one id entry per item held, none for the evicted"
    );
}

#[test]
fn a_stale_pending_listing_does_not_revive_a_terminal_row() {
    let p = peer();
    let mut model = UiModel::new();
    let out = interweave_human_ui_model::ListedOutbound {
        row: RowId::from_stored(7),
        destination: Destination::Direct {
            peer: p.clone(),
            endpoint: Some(endpoint("human")),
        },
        envelope: envelope(7, "sent"),
        created_at: 1,
    };
    model.pending_listed(vec![out.clone()]);
    model.client_event(update(7, OutboundStatus::Published));
    model.pending_listed(vec![out]);
    let item = &model.messages(&key(&p))[0];
    assert_eq!(item.label, LabelKey::PublishedLocally, "still terminal");
    assert!(
        model.actions(item.key).is_empty(),
        "no Retry or Cancel on a deleted row"
    );
}

#[test]
fn one_id_reused_with_ever_new_text_keeps_the_copy_index_bounded() {
    let p = peer();
    let mut model = UiModel::new();
    let cap = interweave_human_ui_model::SESSION_ITEM_CAP;
    for n in 0..2 * cap {
        let row = i64::try_from(n).expect("small");
        model.unread_listed(vec![listed(row, &p, envelope(5, &format!("text {n}")), 1)]);
        model.read(RowId::from_stored(row));
    }
    assert!(
        model.copy_index_keys() <= cap + 1,
        "the live items and the newest, not every text ever: {}",
        model.copy_index_keys()
    );
}

#[test]
fn released_rows_are_remembered_up_to_the_cap_forgetting_the_oldest() {
    let p = peer();
    let mut model = UiModel::new();
    let cap = interweave_human_ui_model::DEDUP_CAP;
    let row = |n: usize| {
        let n32 = u32::try_from(n).expect("small");
        listed(i64::from(n32), &p, envelope(n32, &format!("m{n}")), 1)
    };
    for n in 0..=cap {
        model.unread_listed(vec![row(n)]);
        model.read(RowId::from_stored(i64::try_from(n).expect("small")));
    }
    assert_eq!(model.released_rows(), cap, "at most the cap");
    // The NEWEST release is still remembered: a stale snapshot taken just
    // before it cannot bring it back -- the likeliest stale listing.
    let before = model.conversation_viewed(&key(&p), true).len();
    model.unread_listed(vec![row(cap)]);
    assert_eq!(model.conversation_viewed(&key(&p), true).len(), before);
}
