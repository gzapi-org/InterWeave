// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The headless root over the fake network: a real facade and a real
//! store, a scripted surface in place of a window. What the person does is
//! given as the `ViewEvent`s a view would hand over; what they see is read
//! from the model.
//!
//! What this does NOT prove: a real daemon, a real window, a real
//! platform. Those are the desktop end-to-end suite's.

#![allow(clippy::expect_used, clippy::panic)]

mod common;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use interweave_human_app_core::{
    Command, FacadeSide, Failure, ModelSide, Opener, Problem, Surface, Update,
};
use interweave_human_store::{HumanStore, InboundOrigin, NewInbound, RowId, StoreOptions};
use interweave_human_transport_client::{ClientConfig, TransportClient};
use interweave_human_ui_model::{
    ConversationKey, Intent, LabelKey, Table, TrustChange, TrustInput, TrustOutcome, UiModel,
    ViewEvent,
};
use interweave_local_client_fake::{FakeConfig, FakeEndpoint, FakeNetwork, FakeNode};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{EndpointId, TransportError, TransportIdentity};

const WALL: u64 = 1_786_600_000_000;

fn peer() -> TransportIdentity {
    ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id")
}

fn endpoint(name: &str) -> EndpointId {
    EndpointId::parse(name).expect("valid")
}

fn node() -> FakeConfig {
    FakeConfig {
        peer: peer(),
        endpoints: vec![FakeEndpoint::open(endpoint("human"), false)],
        default_endpoint: Some(endpoint("human")),
        queue_bound: 16,
    }
}

fn facade(node: &FakeNode, store: HumanStore) -> FacadeSide<FakeNode, FakeNode> {
    let client = TransportClient::new(
        node.clone(),
        node.clone(),
        store,
        ClientConfig {
            client_kind: "human-client".to_owned(),
            endpoint: Some(endpoint("human")),
            channels: vec![],
            max_payload_bytes: 49_152,
        },
        Box::new(|| WALL),
        0,
    )
    .expect("facade");
    FacadeSide::new(client, Some(endpoint("human")), || WALL)
}

fn memory() -> HumanStore {
    HumanStore::open_in_memory(StoreOptions::default()).expect("store")
}

/// A view whose person acts from a script: each `take_events` hands over
/// the next batch, and a render is counted.
#[derive(Default)]
struct Scripted {
    batches: VecDeque<Vec<ViewEvent>>,
    renders: usize,
    /// Whether each render was shown a trust change to confirm.
    pending_shown: Vec<bool>,
}

impl Scripted {
    fn will(&mut self, events: Vec<ViewEvent>) {
        self.batches.push_back(events);
    }
}

impl Surface for Scripted {
    fn render(&mut self, model: &UiModel) {
        self.renders += 1;
        self.pending_shown
            .push(model.trust_settings().pending().is_some());
    }

    fn take_events(&mut self, _model: &UiModel) -> Vec<ViewEvent> {
        self.batches.pop_front().unwrap_or_default()
    }
}

/// Records what it was asked to open; a network-access ask as
/// [`NETWORK_ASKED`].
#[derive(Clone, Default)]
struct Opened(Rc<RefCell<Vec<String>>>);

/// What [`Opened`] records for an ask for network access: no link has
/// this shape.
const NETWORK_ASKED: &str = "<network access>";

impl Opener for Opened {
    fn open(&mut self, destination: &str) {
        self.0.borrow_mut().push(destination.to_owned());
    }

    fn ask_network_access(&mut self) {
        self.0.borrow_mut().push(NETWORK_ASKED.to_owned());
    }
}

/// A root: both sides, and the loop between them.
struct Root {
    facade: FacadeSide<FakeNode, FakeNode>,
    model: ModelSide<Scripted, Opened>,
    commands: Vec<Command>,
    updates: Vec<Update>,
}

impl Root {
    fn new(facade: FacadeSide<FakeNode, FakeNode>) -> (Self, Opened) {
        let opened = Opened::default();
        let mut root = Self {
            facade,
            model: ModelSide::new(Scripted::default(), opened.clone()),
            commands: Vec::new(),
            updates: Vec::new(),
        };
        let listing = root.facade.listing().expect("listing");
        root.model.apply(Update::Listed(listing));
        (root, opened)
    }

    /// One pass of the loop: the facade's turn, the model side's turn,
    /// and every command it asked for.
    async fn pump(&mut self, now: u64) {
        for update in self.facade.turn(now).await {
            self.updates.push(update.clone());
            self.model.apply(update);
        }
        let commands = self.model.turn();
        self.commands.extend(commands.iter().cloned());
        for command in commands {
            for update in self.facade.execute(command, now).await {
                self.updates.push(update.clone());
                self.model.apply(update);
            }
        }
    }

    fn will(&mut self, events: Vec<ViewEvent>) {
        self.model.surface_mut().will(events);
    }

    fn model(&self) -> &UiModel {
        self.model.model()
    }
}

fn direct(to: &TransportIdentity) -> ConversationKey {
    ConversationKey::Direct {
        peer: to.clone(),
        endpoint: None,
    }
}

fn send(key: &ConversationKey, text: &str) -> ViewEvent {
    ViewEvent::Intent(Intent::Send {
        key: key.clone(),
        draft: text.to_owned(),
    })
}

#[tokio::test]
async fn a_send_goes_out_and_reads_as_accepted_never_as_read() {
    let (a, b) = FakeNetwork::pair(node(), node());
    let (mut alice, _) = Root::new(facade(&a, memory()));
    let (mut bob, _) = Root::new(facade(&b, memory()));
    alice.pump(0).await;
    bob.pump(0).await;

    let to_bob = direct(b.peer());
    alice.will(vec![send(&to_bob, "hello bob")]);
    alice.pump(1).await;
    alice.pump(2).await;

    let items = alice.model().messages(&to_bob);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].source, "hello bob");
    assert_eq!(items[0].label, LabelKey::AcceptedByRemoteTransport);

    bob.pump(3).await;
    let from_alice: Vec<String> = bob
        .model()
        .conversations()
        .iter()
        .flat_map(|c| bob.model().messages(&c.key))
        .map(|m| m.source)
        .collect();
    assert_eq!(from_alice, ["hello bob"]);
}

#[tokio::test]
async fn a_second_send_while_one_is_in_flight_is_not_sent_twice() {
    let (a, b) = FakeNetwork::pair(node(), node());
    let (mut alice, _) = Root::new(facade(&a, memory()));
    alice.pump(0).await;
    let to_bob = direct(b.peer());
    // Two presses of Send in one take, before either is answered.
    alice.will(vec![send(&to_bob, "once"), send(&to_bob, "once")]);
    alice.pump(1).await;
    let sends = alice
        .commands
        .iter()
        .filter(|c| matches!(c, Command::Send { .. }))
        .count();
    assert_eq!(sends, 1, "{:?}", alice.commands);
    // Answered, the same action can be taken again.
    alice.will(vec![send(&to_bob, "twice")]);
    alice.pump(2).await;
    let sends = alice
        .commands
        .iter()
        .filter(|c| matches!(c, Command::Send { .. }))
        .count();
    assert_eq!(sends, 2, "{:?}", alice.commands);
}

#[tokio::test]
async fn read_then_keep_then_unkeep_then_keep_again_reaches_the_store() {
    let (a, b) = FakeNetwork::pair(node(), node());
    let dir = common::private_tempdir();
    let path = dir.path().join("state").join("human.sqlite3");
    let (mut alice, _) = Root::new(facade(
        &a,
        HumanStore::open(&path, StoreOptions::default()).expect("store"),
    ));
    let (mut bob, _) = Root::new(facade(&b, memory()));
    alice.pump(0).await;
    bob.pump(0).await;
    bob.will(vec![send(&direct(a.peer()), "keep me")]);
    bob.pump(1).await;
    alice.pump(2).await;

    let from_bob = alice.model().conversations()[0].key.clone();
    let item = alice.model().messages(&from_bob)[0].key;
    let unread_row = match alice
        .model()
        .conversation_viewed(&from_bob, true)
        .as_slice()
    {
        [Intent::MarkRead(row)] => *row,
        other => panic!("one unread row: {other:?}"),
    };

    alice.will(vec![ViewEvent::Intent(Intent::MarkRead(unread_row))]);
    alice.pump(3).await;
    assert_eq!(
        alice.model().actions(item),
        [Intent::Keep {
            item,
            from: (Table::Unread, unread_row)
        }]
    );

    let keep = alice.model().actions(item)[0].clone();
    alice.will(vec![ViewEvent::Intent(keep)]);
    alice.pump(4).await;
    let kept_row = match alice.model().actions(item).as_slice() {
        [Intent::Unkeep(row)] => *row,
        other => panic!("kept: {other:?}"),
    };

    alice.will(vec![ViewEvent::Intent(Intent::Unkeep(kept_row))]);
    alice.pump(5).await;
    let keep_again = alice.model().actions(item);
    assert_eq!(
        keep_again,
        [Intent::Keep {
            item,
            from: (Table::Kept, kept_row)
        }],
        "Keep is offered again in the session, from the unkeep's copy"
    );
    alice.will(vec![ViewEvent::Intent(keep_again[0].clone())]);
    alice.pump(6).await;
    assert!(
        matches!(alice.model().actions(item).as_slice(), [Intent::Unkeep(_)]),
        "kept again"
    );
    assert!(
        !alice
            .updates
            .iter()
            .any(|u| matches!(u, Update::Failed { .. })),
        "{:?}",
        alice.updates
    );
    alice.facade.close().await;
    drop(alice);

    let store = HumanStore::open(&path, StoreOptions::default()).expect("reopen");
    let kept = store.kept_inbound().expect("kept");
    assert_eq!(kept.len(), 1, "durable after the second Keep");
    assert!(store.unread_inbound().expect("unread").is_empty());
}

#[tokio::test]
async fn a_keep_with_no_copy_held_fails_and_changes_nothing() {
    let (a, b) = FakeNetwork::pair(node(), node());
    let dir = common::private_tempdir();
    let path = dir.path().join("state").join("human.sqlite3");
    let (mut alice, _) = Root::new(facade(
        &a,
        HumanStore::open(&path, StoreOptions::default()).expect("store"),
    ));
    let (mut bob, _) = Root::new(facade(&b, memory()));
    alice.pump(0).await;
    bob.pump(0).await;
    bob.will(vec![send(&direct(a.peer()), "unread")]);
    bob.pump(1).await;
    alice.pump(2).await;
    let from_bob = alice.model().conversations()[0].key.clone();
    let item = alice.model().messages(&from_bob)[0].key;
    let unread_row = match alice
        .model()
        .conversation_viewed(&from_bob, true)
        .as_slice()
    {
        [Intent::MarkRead(row)] => *row,
        other => panic!("one unread row: {other:?}"),
    };
    // The state a dropped copy leaves: the model has the message read and
    // offers Keep, while the facade side holds no copy of it.
    alice.model.apply(Update::Read(unread_row));
    let keep = Command::Keep {
        item,
        from: (Table::Unread, unread_row),
    };
    assert_eq!(
        alice.model().actions(item),
        [Intent::Keep {
            item,
            from: (Table::Unread, unread_row)
        }],
        "the control: Keep is offered before the failure"
    );
    let updates = alice.facade.execute(keep.clone(), 3).await;
    assert_eq!(
        updates,
        [Update::Failed {
            command: keep.clone(),
            why: Failure::CopyGone
        }]
    );
    for update in updates {
        alice.model.apply(update);
    }
    assert!(
        alice.model().actions(item).is_empty(),
        "Keep is no longer offered: {:?}",
        alice.model().actions(item)
    );
    assert_eq!(
        alice.model.take_problems(),
        [Problem::Command {
            command: keep,
            why: Failure::CopyGone
        }],
        "and the root is told"
    );
    alice.facade.close().await;
    drop(alice);
    let store = HumanStore::open(&path, StoreOptions::default()).expect("reopen");
    assert!(
        store.kept_inbound().expect("kept").is_empty(),
        "nothing kept"
    );
    assert_eq!(
        store.unread_inbound().expect("unread").len(),
        1,
        "still unread"
    );
}

#[tokio::test]
async fn a_link_is_opened_only_when_allowlisted_and_never_reaches_the_facade() {
    let (a, _b) = FakeNetwork::pair(node(), node());
    let (mut alice, opened) = Root::new(facade(&a, memory()));
    alice.will(vec![
        ViewEvent::Intent(Intent::OpenLink("https://example.org/a".to_owned())),
        ViewEvent::Intent(Intent::OpenLink("javascript:alert(1)".to_owned())),
    ]);
    alice.pump(0).await;
    assert_eq!(*opened.0.borrow(), ["https://example.org/a"]);
    assert!(alice.commands.is_empty(), "{:?}", alice.commands);
}

/// The ask for network access is the platform's, as a link is: it reaches
/// the opener and never the facade, and while access is withheld a draft
/// is not sent.
#[tokio::test]
async fn an_ask_for_network_access_reaches_the_platform_and_never_the_facade() {
    let (a, b) = FakeNetwork::pair(node(), node());
    let (mut alice, opened) = Root::new(facade(&a, memory()));
    alice.pump(0).await;
    let to_bob = direct(b.peer());
    alice.model.network_access(false);
    alice.will(vec![
        ViewEvent::DraftChanged {
            key: to_bob.clone(),
            draft: "typed".to_owned(),
        },
        ViewEvent::Intent(Intent::AllowNetwork),
    ]);
    alice.pump(1).await;
    assert_eq!(*opened.0.borrow(), [NETWORK_ASKED]);
    assert!(alice.commands.is_empty(), "{:?}", alice.commands);
    assert_eq!(
        alice.model().send_draft(&to_bob),
        None,
        "nothing to send while access is withheld"
    );
}

#[tokio::test]
async fn a_draft_edit_reaches_the_model_before_a_send_in_the_same_take() {
    let (a, b) = FakeNetwork::pair(node(), node());
    let (mut alice, _) = Root::new(facade(&a, memory()));
    alice.pump(0).await;
    let to_bob = direct(b.peer());
    alice.will(vec![ViewEvent::DraftChanged {
        key: to_bob.clone(),
        draft: "typed".to_owned(),
    }]);
    alice.pump(1).await;
    assert_eq!(alice.model().composer(&to_bob).draft, "typed");
}

#[tokio::test]
async fn the_store_is_listed_at_start() {
    let dir = common::private_tempdir();
    let path = dir.path().join("state").join("human.sqlite3");
    let (a, b) = FakeNetwork::pair(node(), node());
    // A message that stays pending: the remote is busy.
    a.inject_send(TransportError::Overloaded);
    let (mut alice, _) = Root::new(facade(
        &a,
        HumanStore::open(&path, StoreOptions::default()).expect("store"),
    ));
    let (mut bob, _) = Root::new(facade(&b, memory()));
    alice.pump(0).await;
    bob.pump(0).await;
    alice.will(vec![send(&direct(b.peer()), "still pending")]);
    alice.pump(1).await;
    bob.will(vec![send(&direct(a.peer()), "left unread")]);
    bob.pump(2).await;
    alice.pump(3).await;
    alice.facade.close().await;
    drop(alice);

    let (a2, _b2) = FakeNetwork::pair(node(), node());
    let (restarted, _) = Root::new(facade(
        &a2,
        HumanStore::open(&path, StoreOptions::default()).expect("reopen"),
    ));
    let mut texts: Vec<String> = restarted
        .model()
        .conversations()
        .iter()
        .flat_map(|c| restarted.model().messages(&c.key))
        .map(|m| m.source)
        .collect();
    texts.sort();
    assert_eq!(texts, ["left unread", "still pending"]);
}

/// A store failure behind a command changes nothing in the model and is
/// reported with its class: a read of a row that is not there, and an
/// unkeep of a kept row the build cannot decode.
#[tokio::test]
async fn a_store_failure_changes_nothing_in_the_model_and_says_why() {
    let (a, b) = FakeNetwork::pair(node(), node());
    let dir = common::private_tempdir();
    let path = dir.path().join("state").join("human.sqlite3");
    let (mut alice, _) = Root::new(facade(
        &a,
        HumanStore::open(&path, StoreOptions::default()).expect("store"),
    ));
    let (mut bob, _) = Root::new(facade(&b, memory()));
    alice.pump(0).await;
    bob.pump(0).await;
    bob.will(vec![send(&direct(a.peer()), "one")]);
    bob.pump(1).await;
    alice.pump(2).await;
    let from_bob = alice.model().conversations()[0].key.clone();
    let item = alice.model().messages(&from_bob)[0].key;

    let missing = Command::MarkRead(RowId::from_stored(999));
    let updates = alice.facade.execute(missing.clone(), 3).await;
    assert_eq!(
        updates,
        [Update::Failed {
            command: missing,
            why: Failure::NoSuchRow
        }],
        "no Read reaches the model"
    );

    // Read and keep it, then make the kept row undecodable.
    let unread_row = match alice
        .model()
        .conversation_viewed(&from_bob, true)
        .as_slice()
    {
        [Intent::MarkRead(row)] => *row,
        other => panic!("one unread row: {other:?}"),
    };
    alice.will(vec![ViewEvent::Intent(Intent::MarkRead(unread_row))]);
    alice.pump(4).await;
    let keep = alice.model().actions(item)[0].clone();
    alice.will(vec![ViewEvent::Intent(keep)]);
    alice.pump(5).await;
    let kept_row = match alice.model().actions(item).as_slice() {
        [Intent::Unkeep(row)] => *row,
        other => panic!("kept: {other:?}"),
    };
    let conn = rusqlite::Connection::open(&path).expect("open");
    conn.execute("UPDATE kept_inbound SET source_peer = 'not-a-peer'", [])
        .expect("corrupt");
    drop(conn);

    let unkeep = Command::Unkeep(kept_row);
    let updates = alice.facade.execute(unkeep.clone(), 6).await;
    assert_eq!(
        updates,
        [Update::Failed {
            command: unkeep,
            why: Failure::Corrupt
        }]
    );
    for update in updates {
        alice.model.apply(update);
    }
    assert_eq!(
        alice.model().actions(item),
        [Intent::Unkeep(kept_row)],
        "the model still shows it kept: no Unkept was applied"
    );
}

/// A stored row that cannot be decoded is held, never shown, and counted.
#[tokio::test]
async fn an_undecodable_stored_row_is_not_shown_and_is_counted() {
    let (a, _b) = FakeNetwork::pair(node(), node());
    let mut store = memory();
    let origin = InboundOrigin {
        peer: peer(),
        endpoint: Some(endpoint("human")),
        channel: None,
    };
    let media = interweave_transport_api::MediaType::parse(
        "application/vnd.interweave-human-chat+json;v=2",
    )
    .expect("media type");
    for (id, payload) in [
        ("0000000000000000000000000000000a", b"not json".to_vec()),
        (
            "0000000000000000000000000000000b",
            br#"{"v":2,"kind":"text","app_message_id":"0000000000000000000000000000000b","text":"shown"}"#
                .to_vec(),
        ),
    ] {
        store
            .commit_unread_inbound(&NewInbound {
                app_message_id: interweave_human_store::AppMessageId::parse(id.to_owned())
                    .expect("id"),
                origin: origin.clone(),
                media_type: Some(media.clone()),
                payload,
                received_at: 1,
            })
            .expect("committed");
    }
    let mut side = facade(&a, store);
    let listing = side.listing().expect("listing");
    assert_eq!(listing.undecodable_unread, 1);
    assert_eq!(listing.unread.len(), 1);
    let (root, _) = Root::new(side);
    assert_eq!(root.model.hidden_rows(), 1);
    let texts: Vec<String> = root
        .model()
        .conversations()
        .iter()
        .flat_map(|c| root.model().messages(&c.key))
        .map(|m| m.source)
        .collect();
    assert_eq!(texts, ["shown"]);
}

/// The model side records the press when it issues a send, so the
/// facade's answer clears an unedited composer and keeps one edited since.
#[tokio::test]
async fn a_sent_message_clears_the_composer_only_if_it_was_not_edited_since() {
    let (a, b) = FakeNetwork::pair(node(), node());
    let (mut alice, _) = Root::new(facade(&a, memory()));
    alice.pump(0).await;
    let to_bob = direct(b.peer());
    alice.will(vec![
        ViewEvent::DraftChanged {
            key: to_bob.clone(),
            draft: "hi".to_owned(),
        },
        send(&to_bob, "hi"),
    ]);
    alice.pump(1).await;
    assert_eq!(
        alice.model().composer(&to_bob).draft,
        "",
        "unedited: cleared"
    );
}

fn trust(input: TrustInput) -> Vec<ViewEvent> {
    vec![ViewEvent::Trust(input)]
}

/// The trust settings through the root: opening reads the daemon's
/// allowlist, a proposal reaches nothing, and only a confirmation sends
/// the change -- which the daemon then holds, read back.
#[tokio::test]
async fn a_trust_change_reaches_the_daemon_only_once_confirmed() {
    let (a, b) = FakeNetwork::pair(node(), node());
    let (mut root, _) = Root::new(facade(&a, memory()));
    root.pump(0).await;

    root.will(trust(TrustInput::Opened));
    root.pump(1).await;
    let list = root.model().trust_settings().list().cloned().expect("read");
    assert_eq!(list.local_peer.as_ref(), Some(a.peer()));
    assert_eq!(
        list.peers().cloned().collect::<Vec<_>>(),
        vec![b.peer().clone()]
    );

    let stranger = peer();
    root.will(trust(TrustInput::EntryChanged(
        stranger.as_str().to_owned(),
    )));
    root.will(trust(TrustInput::ProposeAllow));
    root.pump(2).await;
    let set = |c: &Command| matches!(c, Command::SetTrust(_));
    assert!(!root.commands.iter().any(set), "a proposal sends nothing");
    assert!(root.model().trust_settings().pending().is_some());

    let change = TrustChange {
        peer: stranger.clone(),
        allowed: true,
    };
    root.will(trust(TrustInput::Confirm(change.clone())));
    root.pump(3).await;
    assert!(root.commands.contains(&Command::SetTrust(change.clone())));
    let settings = root.model().trust_settings();
    assert_eq!(settings.outcome().0, Some(&TrustOutcome::Changed(change)));
    assert!(
        settings.list().is_some_and(|l| l.allows(&stranger)),
        "the daemon holds it, read back"
    );

    root.will(trust(TrustInput::ProposeRevoke(b.peer().clone())));
    root.will(trust(TrustInput::Confirm(TrustChange {
        peer: b.peer().clone(),
        allowed: false,
    })));
    root.pump(4).await;
    assert!(
        root.model()
            .trust_settings()
            .list()
            .is_some_and(|l| !l.allows(b.peer())),
        "revoked, read back"
    );
}

/// A trust input that asks the facade nothing still changes what the
/// settings show, so the same turn renders it: a proposal is on screen
/// to confirm without waiting for an unrelated event.
#[tokio::test]
async fn a_trust_proposal_is_rendered_in_the_turn_that_took_it() {
    let (a, b) = FakeNetwork::pair(node(), node());
    let (mut root, _) = Root::new(facade(&a, memory()));
    root.will(trust(TrustInput::Opened));
    root.pump(0).await;
    root.pump(1).await;
    root.will(trust(TrustInput::ProposeRevoke(b.peer().clone())));
    root.model.turn();
    assert_eq!(
        root.model.surface().pending_shown.last(),
        Some(&true),
        "the turn's last render showed the proposal"
    );
}

/// A trust intent handed over bare -- by a surface that built one rather
/// than confirming the change shown -- reaches nothing: trust changes only
/// through the settings' inputs.
#[tokio::test]
async fn a_trust_intent_handed_over_bare_reaches_nothing() {
    let (a, b) = FakeNetwork::pair(node(), node());
    let (mut root, _) = Root::new(facade(&a, memory()));
    root.will(vec![
        ViewEvent::Intent(Intent::ReadTrust),
        ViewEvent::Intent(Intent::SetTrust(TrustChange {
            peer: b.peer().clone(),
            allowed: false,
        })),
    ]);
    root.pump(0).await;
    assert!(
        !root
            .commands
            .iter()
            .any(|c| matches!(c, Command::ReadTrust | Command::SetTrust(_))),
        "{:?}",
        root.commands
    );
}

/// A change made whose list was not read back -- or one the daemon did not
/// confirm -- leaves the list shown possibly stale: the next turn reads it
/// again, unasked.
#[tokio::test]
async fn a_change_not_read_back_is_read_again_by_the_next_turn() {
    use interweave_human_client_api::{TrustProblem, TrustSetFailure};
    let (a, b) = FakeNetwork::pair(node(), node());
    let (mut root, _) = Root::new(facade(&a, memory()));
    root.will(trust(TrustInput::Opened));
    root.pump(0).await;
    let change = TrustChange {
        peer: b.peer().clone(),
        allowed: false,
    };
    root.will(trust(TrustInput::ProposeRevoke(b.peer().clone())));
    root.will(trust(TrustInput::Confirm(change.clone())));
    let commands = root.model.turn();
    assert_eq!(commands, vec![Command::SetTrust(change.clone())]);
    root.model.apply(Update::TrustSet {
        change,
        answer: Err(TrustSetFailure::MadeNotReadBack(TrustProblem::Unavailable)),
    });
    root.model.apply(Update::Done(commands[0].clone()));
    assert_eq!(root.model.turn(), vec![Command::ReadTrust], "read again");
}
