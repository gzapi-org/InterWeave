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

use interweave_human_chat_protocol::{HumanChatV2, MessageKind};
use interweave_human_client_api::{
    ClientEvent, Connectivity, Destination, Origin, OutboundStatus, SessionState,
};
use interweave_human_core::RowId;
use interweave_human_store::{HumanStore, InboundOrigin, OutboundDestination, StoreOptions};
use interweave_human_transport_client::{ClientConfig, TransportClient};
use interweave_human_ui_model::{
    ConversationKey, Intent, LabelKey, ListedInbound, ListedOutbound, Trust, UiModel,
};
use interweave_local_client_fake::{FakeConfig, FakeEndpoint, FakeNetwork, FakeNode};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{EndpointId, TransportError, TransportIdentity};

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
        | Intent::Keep(_)
        | Intent::Unkeep(_)
        | Intent::Retry(_)
        | Intent::Cancel(_)
        | Intent::Send { .. }
        | Intent::OpenLink(_)
        | Intent::Reopen
        | Intent::RecheckStorage => false,
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
    // Stage 14 has no per-peer path event (plan §17 (5)); at the model a
    // connectivity change or a disconnect adds no conversation or message.
    let p = peer();
    let mut model = UiModel::new();
    model.unread_listed(vec![inbound(1, &p, envelope(1, "x"))]);
    let before = (model.conversations(), model.len());
    for event in [
        ClientEvent::Connectivity(Connectivity::OnlineRelay),
        ClientEvent::Connectivity(Connectivity::OnlineDirect),
        ClientEvent::PeerDisconnected { peer: p.clone() },
        ClientEvent::Session(SessionState::Ready { endpoint: None }),
    ] {
        model.client_event(event);
    }
    assert_eq!((model.conversations(), model.len()), before);
}

#[test]
fn s13_4_trust_mutation_has_no_stage_14_surface_and_no_intent_mutates_trust() {
    // Absence: ADR-0032's trust administration is Stage 15's. Asserted by
    // the exhaustive match above, over every intent the model can raise.
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
    // Receipt raises nothing: the only intents are what a person's own
    // actions on the item allow.
    assert_eq!(
        model.actions(item),
        [Intent::MarkRead(RowId::from_stored(1))]
    );
    // A link raises an intent only on a person's activation, and only for
    // an allowlisted scheme.
    assert_eq!(model.link_activated("javascript:admin.trust.clear()"), None);
    assert_eq!(model.link_activated("interweave://admin/recovery"), None);
    for intent in model.actions(item) {
        assert!(!reaches_trust_admin_or_recovery(&intent));
    }
}

#[test]
fn s13_6_the_same_fixture_renders_the_same_on_every_platform_at_the_model_level() {
    // Desktop and Android share this model and its render: the same
    // envelope gives the same items in two independent models.
    let p = peer();
    let fixture = envelope(
        1,
        "# Title\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n~~gone~~ **kept**",
    );
    let render_with = |model: &mut UiModel| {
        model.unread_listed(vec![inbound(1, &p, fixture.clone())]);
        model.messages(&ConversationKey::Direct {
            peer: p.clone(),
            endpoint: Some(endpoint("human")),
        })
    };
    assert_eq!(
        render_with(&mut UiModel::new()),
        render_with(&mut UiModel::new())
    );
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
        !model.actions(item).contains(&Intent::Keep(item)),
        "not before read, whatever the text says"
    );
    model.read(RowId::from_stored(1));
    assert_eq!(model.actions(item), [Intent::Keep(item)]);
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
    let dir = tempfile::tempdir().expect("tempdir");
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
    let dir = tempfile::tempdir().expect("tempdir");
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
    // The window is not focused: nothing to act on.
    assert!(
        model.conversation_viewed(&key, false).is_empty(),
        "no intent while unfocused"
    );
    a_side.close().await;
    drop(a_side);
    let store = HumanStore::open(&path, StoreOptions::default()).expect("reopen");
    assert_eq!(store.unread_inbound().expect("unread").len(), 1);
}
