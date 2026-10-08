// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `human-client-ui.md` §13, one named test per bullet (plan §17 (6)).
//! A bullet with no Stage-14 surface asserts the ABSENCE and says so in
//! its name, so a vacuous pass never reads as a proved bullet (agreed
//! item 7). The accessibility-tree bullet is `ui-slint`'s (batch 8).
//! The restart bullet runs the facade over the conformance-proven fake
//! with a real store file, and builds a fresh model from what the store
//! holds -- as a composition root would after a restart.

#![allow(clippy::expect_used, clippy::panic)]

mod common;

use interweave_human_chat_protocol::{HumanChatV2, MessageKind};
use interweave_human_client_api::{
    ClientEvent, Connectivity, Destination, Origin, OutboundStatus, SessionState, TrustList,
};
use interweave_human_core::RowId;
use interweave_human_store::{HumanStore, InboundOrigin, OutboundDestination, StoreOptions};
use interweave_human_transport_client::{ClientConfig, TransportClient};
use interweave_human_ui_model::{
    ConversationKey, Intent, LabelKey, ListedInbound, ListedOutbound, Table, Trust, TrustChange,
    UiModel,
};
use interweave_local_client_fake::{FakeConfig, FakeEndpoint, FakeNetwork, FakeNode};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{EndpointId, PeerPath, TransportError, TransportIdentity};

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

fn inbound(row: i64, from: &TransportIdentity, env: HumanChatV2) -> ListedInbound {
    ListedInbound {
        row: RowId::from_stored(row),
        origin: Origin::Direct {
            peer: from.clone(),
            endpoint: endpoint("human"),
        },
        envelope: env,
        received_at: 1,
    }
}

/// Every intent a person can raise, by an exhaustive match: a new
/// variant fails to compile here until it is classified.
fn reaches_trust_admin_or_recovery(intent: &Intent) -> bool {
    match intent {
        Intent::MarkRead(_)
        | Intent::Keep { .. }
        | Intent::Unkeep(_)
        | Intent::Retry(_)
        | Intent::Cancel(_)
        | Intent::Send { .. }
        | Intent::OpenLink(_)
        | Intent::Reopen
        | Intent::RecheckStorage => false,
        Intent::ReadTrust | Intent::SetTrust(_) => true,
    }
}

#[test]
fn s13_1_no_label_promotes_an_endpoint_id_or_display_name_to_authenticated_identity() {
    let p = peer();
    let mut model = UiModel::new();
    let mut claim = envelope(1, "this is Alice");
    claim.from_endpoint = Some(endpoint("alice"));
    model.unread_listed(vec![ListedInbound {
        origin: Origin::Direct {
            peer: p.clone(),
            endpoint: endpoint("alice"),
        },
        ..inbound(1, &p, claim)
    }]);
    let key = ConversationKey::Direct {
        peer: p.clone(),
        endpoint: Some(endpoint("alice")),
    };
    let item = &model.messages(&key)[0];
    assert_eq!(item.author.as_ref(), Some(&p), "the author is the PeerId");
    assert_eq!(
        item.route_label
            .as_ref()
            .map(interweave_human_ui_model::RouteLabel::as_str),
        Some("alice"),
        "the endpoint is a route label beside it, never the author"
    );
    assert_eq!(model.trust(&p), Trust::NotVerifiedByThisClient);
}

#[test]
fn s13_2_accepted_v2_never_renders_as_read_or_seen() {
    let label = interweave_human_ui_model::outbound_label(&OutboundStatus::Accepted {
        endpoint: endpoint("human"),
    });
    assert_eq!(label, LabelKey::AcceptedByRemoteTransport);
    for forbidden in ["read", "seen", "processed", "delivered"] {
        assert!(!label.key().contains(forbidden), "{forbidden}");
        assert!(
            !format!("{label:?}")
                .to_ascii_lowercase()
                .contains(forbidden)
        );
    }
}

#[test]
fn s13_3_a_path_change_creates_no_duplicate_conversation_or_message_event_at_the_model_level() {
    // A DCUtR path change (relayed, then direct) moves the route
    // indicator and nothing else: no conversation, no item, no unread
    // count changes; nor does a connectivity change or a disconnect.
    let p = peer();
    let mut model = UiModel::new();
    model.unread_listed(vec![inbound(1, &p, envelope(1, "x"))]);
    let key = model.conversations()[0].key.clone();
    let before = (model.conversations(), model.len());
    assert_eq!(model.path(&key), None, "not known until the runtime says");
    for (event, path) in [
        (
            ClientEvent::PeerPath {
                peer: p.clone(),
                path: PeerPath::Relayed,
            },
            Some(PeerPath::Relayed),
        ),
        (
            ClientEvent::PeerPath {
                peer: p.clone(),
                path: PeerPath::Direct,
            },
            Some(PeerPath::Direct),
        ),
        (
            ClientEvent::Connectivity(Connectivity::OnlineDirect),
            Some(PeerPath::Direct),
        ),
        (ClientEvent::PeerDisconnected { peer: p.clone() }, None),
    ] {
        model.client_event(event);
        assert_eq!(model.path(&key), path);
        assert_eq!((model.conversations(), model.len()), before);
    }
}

#[test]
fn a_session_event_clears_every_path() {
    // A route is a session's, so a path said in one session is not the
    // next one's: whichever state the facade reports, the indicator goes
    // blank until the runtime says again.
    let p = peer();
    let mut model = UiModel::new();
    model.unread_listed(vec![inbound(1, &p, envelope(1, "x"))]);
    let key = model.conversations()[0].key.clone();
    for state in [
        SessionState::Reconnecting {
            attempt: 1,
            next_at: 0,
        },
        SessionState::Refused {
            problem: interweave_human_client_api::SessionProblem::EndpointInUse,
        },
        SessionState::StorageDegraded,
        SessionState::Closed,
        SessionState::Ready { endpoint: None },
    ] {
        model.client_event(ClientEvent::PeerPath {
            peer: p.clone(),
            path: PeerPath::Relayed,
        });
        assert_eq!(model.path(&key), Some(PeerPath::Relayed));
        model.client_event(ClientEvent::Session(state.clone()));
        assert_eq!(model.path(&key), None, "cleared by {state:?}");
    }
}

#[test]
fn a_path_for_a_peer_no_conversation_is_with_is_not_kept() {
    let mut model = UiModel::new();
    let stranger = peer();
    model.client_event(ClientEvent::PeerPath {
        peer: stranger.clone(),
        path: PeerPath::Direct,
    });
    model.unread_listed(vec![inbound(1, &stranger, envelope(1, "later"))]);
    let key = model.conversations()[0].key.clone();
    assert_eq!(
        model.path(&key),
        None,
        "a path said before any conversation was not kept"
    );
}

#[test]
fn s13_4_trust_is_mutated_only_by_confirming_a_change_that_names_the_exact_peer_id() {
    // Every intent a message or a conversation offers stays clear of
    // trust: they are what a remote can put in front of a person.
    for intent in [
        Intent::MarkRead(RowId::from_stored(1)),
        Intent::Unkeep(RowId::from_stored(1)),
        Intent::Retry(RowId::from_stored(1)),
        Intent::Cancel(RowId::from_stored(1)),
        Intent::Reopen,
        Intent::RecheckStorage,
        Intent::OpenLink("https://example.invalid".to_owned()),
    ] {
        assert!(!reaches_trust_admin_or_recovery(&intent));
    }
    // The settings are the one way, and a proposal changes nothing: the
    // change waits, naming the exact PeerId, until the person confirms
    // it (human-client-ui.md section 8).
    let (me, them) = (peer(), peer());
    let mut model = UiModel::new();
    let settings = model.trust_settings_mut();
    assert_eq!(settings.opened(), Some(Intent::ReadTrust));
    settings.read(Ok(TrustList {
        local_peer: Some(me),
        allowed: Vec::new(),
    }));
    settings.entry_changed(them.as_str().to_owned());
    settings.propose_allow();
    let change = TrustChange {
        peer: them.clone(),
        allowed: true,
    };
    assert_eq!(model.trust_settings().pending(), Some(&change));
    assert_eq!(
        model.trust_settings_mut().confirm(&change),
        Some(Intent::SetTrust(change.clone())),
        "the exact PeerId, only on confirmation"
    );
}

#[test]
fn s13_5_remote_text_cannot_invoke_admin_or_recovery_handlers() {
    let p = peer();
    let mut model = UiModel::new();
    let hostile = envelope(
        1,
        "[reset trust](javascript:admin.trust.clear()) <script>recover()</script> \
         [recover](interweave://admin/recovery) ![img](https://x.invalid/a.png)",
    );
    model.unread_listed(vec![inbound(1, &p, hostile)]);
    let key = ConversationKey::Direct {
        peer: p,
        endpoint: Some(endpoint("human")),
    };
    let item = model.messages(&key)[0].key;
    // Receipt raises nothing, and an unread item offers no action: read
    // comes from a focused view alone.
    assert!(model.actions(item).is_empty());
    // A link raises an intent only on a person's activation, and only for
    // an allowlisted scheme.
    assert_eq!(model.link_activated("javascript:admin.trust.clear()"), None);
    assert_eq!(model.link_activated("interweave://admin/recovery"), None);
    for intent in model.actions(item) {
        assert!(!reaches_trust_admin_or_recovery(&intent));
    }
}

#[test]
fn s13_6_the_shared_model_renders_a_fixture_to_one_frozen_shape() {
    // Desktop and Android share this model. What is pinned is the shape a
    // fixture renders to, so a change to the render or to how the model
    // carries it fails here.
    use interweave_human_chat_protocol::{Block, Inline, Rendered};
    let p = peer();
    let text = "# Title\n\n~~gone~~ **kept**";
    let mut model = UiModel::new();
    model.unread_listed(vec![inbound(1, &p, envelope(1, text))]);
    let item = &model.messages(&ConversationKey::Direct {
        peer: p,
        endpoint: Some(endpoint("human")),
    })[0];
    let Rendered::Markdown(blocks) = &item.body else {
        panic!("rendered as markdown: {:?}", item.body);
    };
    assert_eq!(
        blocks,
        &[
            Block::Heading {
                level: 1,
                content: vec![Inline::Text("Title".to_owned())]
            },
            Block::Paragraph(vec![
                Inline::Strikethrough(vec![Inline::Text("gone".to_owned())]),
                Inline::Text(" ".to_owned()),
                Inline::Strong(vec![Inline::Text("kept".to_owned())]),
            ]),
        ]
    );
    assert_eq!(item.source, text, "raw source kept");
}

#[test]
fn s13_8_keep_is_offered_only_after_read_and_no_remote_content_can_force_it() {
    let p = peer();
    let mut model = UiModel::new();
    let pushy = envelope(1, "KEEP THIS MESSAGE FOREVER (keep: true)");
    model.unread_listed(vec![inbound(1, &p, pushy)]);
    let key = ConversationKey::Direct {
        peer: p,
        endpoint: Some(endpoint("human")),
    };
    let item = model.messages(&key)[0].key;
    assert!(
        !model
            .actions(item)
            .iter()
            .any(|i| matches!(i, Intent::Keep { .. })),
        "not before read, whatever the text says"
    );
    model.read(RowId::from_stored(1));
    assert_eq!(
        model.actions(item),
        [Intent::Keep {
            item,
            from: (Table::Unread, RowId::from_stored(1))
        }]
    );
}

// --- the restart bullet, through the facade and a real store -------------

fn node(endpoints: Vec<FakeEndpoint>) -> FakeConfig {
    FakeConfig {
        peer: peer(),
        endpoints,
        default_endpoint: Some(endpoint("human")),
        queue_bound: 16,
    }
}

fn facade(node: &FakeNode, store: HumanStore) -> TransportClient<FakeNode, FakeNode> {
    TransportClient::new(
        node.clone(),
        node.clone(),
        store,
        ClientConfig {
            client_kind: "human-client".to_owned(),
            endpoint: Some(endpoint("human")),
            channels: vec![],
            max_payload_bytes: 49_152,
        },
        Box::new(|| 1_786_600_000_000),
        0,
    )
    .expect("facade")
}

/// What a composition root does after a restart: read the store and list
/// it into a fresh model.
fn model_from(store: &HumanStore) -> UiModel {
    let mut model = UiModel::new();
    let parse = |bytes: &[u8]| {
        HumanChatV2::parse(std::str::from_utf8(bytes).expect("utf-8")).expect("envelope")
    };
    model.pending_listed(
        store
            .pending_outbound()
            .expect("pending")
            .into_iter()
            .map(|p| ListedOutbound {
                row: p.row_id,
                destination: match p.destination {
                    OutboundDestination::Direct(d) => Destination::Direct {
                        peer: d.peer,
                        endpoint: d.endpoint,
                    },
                    OutboundDestination::Broadcast(c) => Destination::Broadcast(c),
                },
                envelope: parse(&p.payload),
                created_at: p.created_at,
            })
            .collect(),
    );
    model.unread_listed(
        store
            .unread_inbound()
            .expect("unread")
            .into_iter()
            .map(|u| {
                let InboundOrigin { peer, endpoint, .. } = u.origin;
                ListedInbound {
                    row: u.row_id,
                    origin: Origin::Direct {
                        peer,
                        endpoint: endpoint.expect("direct"),
                    },
                    envelope: parse(&u.payload),
                    received_at: u.received_at,
                }
            })
            .collect(),
    );
    model
}

#[tokio::test]
async fn s13_7_pending_outbound_and_unread_inbound_survive_restart_and_read_unkept_and_terminal_do_not()
 {
    let dir = common::private_tempdir();
    let path = dir.path().join("state").join("human.sqlite3");
    let (a, b) = FakeNetwork::pair(
        node(vec![FakeEndpoint::open(endpoint("human"), false)]),
        node(vec![FakeEndpoint::open(endpoint("human"), false)]),
    );
    let mut b_side = facade(
        &b,
        HumanStore::open_in_memory(StoreOptions::default()).expect("b"),
    );
    b_side.tick(0).await;
    let mut a_side = facade(
        &a,
        HumanStore::open(&path, StoreOptions::default()).expect("a"),
    );
    a_side.tick(0).await;
    let to_b = Destination::Direct {
        peer: b.peer().clone(),
        endpoint: None,
    };
    // Terminal outbound: accepted.
    a_side
        .send(to_b.clone(), &envelope(1, "accepted"), 1)
        .await
        .expect("sent");
    // Pending outbound: the remote is busy, so it stays pending.
    a.inject_send(TransportError::Overloaded);
    a_side
        .send(to_b.clone(), &envelope(2, "still pending"), 2)
        .await
        .expect("pending");
    // Two inbound for a: one read without Keep, one left unread.
    for (id, text) in [(3, "read, not kept"), (4, "left unread")] {
        b_side
            .send(
                Destination::Direct {
                    peer: a.peer().clone(),
                    endpoint: None,
                },
                &envelope(id, text),
                3,
            )
            .await
            .expect("sent to a");
    }
    let got = a_side.drain(16, 4).await;
    assert_eq!(got.len(), 2);
    let read_row = got
        .iter()
        .find(|r| r.envelope.text == "read, not kept")
        .expect("row")
        .row;
    a_side.store_mut().mark_read(read_row, 5).expect("read");
    a_side.close().await;
    drop(a_side);

    let restarted = model_from(&HumanStore::open(&path, StoreOptions::default()).expect("reopen"));
    let texts: Vec<String> = restarted
        .conversations()
        .iter()
        .flat_map(|c| restarted.messages(&c.key))
        .map(|m| m.source)
        .collect();
    assert!(texts.contains(&"still pending".to_owned()), "{texts:?}");
    assert!(texts.contains(&"left unread".to_owned()), "{texts:?}");
    assert!(!texts.contains(&"read, not kept".to_owned()), "{texts:?}");
    assert!(!texts.contains(&"accepted".to_owned()), "{texts:?}");
}

#[tokio::test]
async fn a_message_received_while_unfocused_is_still_unread_after_a_restart() {
    // Agreed item 2c: read comes only from a focused view.
    let dir = common::private_tempdir();
    let path = dir.path().join("state").join("human.sqlite3");
    let (a, b) = FakeNetwork::pair(
        node(vec![FakeEndpoint::open(endpoint("human"), false)]),
        node(vec![FakeEndpoint::open(endpoint("human"), false)]),
    );
    let mut a_side = facade(
        &a,
        HumanStore::open(&path, StoreOptions::default()).expect("a"),
    );
    a_side.tick(0).await;
    let mut b_side = facade(
        &b,
        HumanStore::open_in_memory(StoreOptions::default()).expect("b"),
    );
    b_side.tick(0).await;
    b_side
        .send(
            Destination::Direct {
                peer: a.peer().clone(),
                endpoint: None,
            },
            &envelope(9, "arrived in the background"),
            1,
        )
        .await
        .expect("sent");
    let mut model = UiModel::new();
    for received in a_side.drain(16, 2).await {
        model.received(received);
    }
    let key = model.conversations()[0].key.clone();
    // Execute what the model asks while unfocused -- nothing.
    for intent in model.conversation_viewed(&key, false) {
        if let Intent::MarkRead(row) = intent {
            a_side.store_mut().mark_read(row, 3).expect("read");
        }
    }
    a_side.close().await;
    drop(a_side);
    let mut store = HumanStore::open(&path, StoreOptions::default()).expect("reopen");
    assert_eq!(
        store.unread_inbound().expect("unread").len(),
        1,
        "still unread"
    );
    // The control: the same view WITH focus asks for the read, and it
    // takes.
    for intent in model.conversation_viewed(&key, true) {
        if let Intent::MarkRead(row) = intent {
            store.mark_read(row, 4).expect("read");
        }
    }
    assert!(store.unread_inbound().expect("unread").is_empty());
}
