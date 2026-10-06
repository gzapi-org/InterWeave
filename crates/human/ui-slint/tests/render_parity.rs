// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The render-parity golden (`human-client-ui.md` §13: desktop and
//! Android render the same `HumanChatV2` fixture consistently; plan §20
//! carries the Android half). Each case's text is received, its
//! conversation shown, and the drawn body the window's model then holds
//! -- every line's kind, level, depth, quote, marker and text, and every
//! link control's label -- must equal the golden in
//! `test-data/human-chat/render-parity.json`. Both platforms draw with
//! this crate, so the Android instrumented test compares the same file;
//! a difference between them is then a platform's, not the drawing's.
//!
//! What this does not prove: glyphs, wrapping and layout, which are the
//! platform's text stack (the rendered-window inspection's, and Android's
//! own). `RENDER_PARITY_WRITE=1` rewrites the golden from this build; a
//! rewrite is reviewed line by line before it is committed, since a
//! golden written from the code under test agrees with it for free.

#![allow(clippy::expect_used, clippy::panic)]

use interweave_human_chat_protocol::{HumanChatV2, MessageKind};
use interweave_human_client_api::{Origin, Received};
use interweave_human_core::RowId;
use interweave_human_ui_model::{ConversationKey, UiModel};
use interweave_human_ui_slint::View;
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::EndpointId;
use serde_json::{Value, json};
use slint::Model as _;

fn golden_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../test-data/human-chat/render-parity.json")
}

/// The drawn body of the one message `text` makes, as the window's model
/// holds it.
fn drawn(text: &str) -> Value {
    let mut view = View::new().expect("a window");
    let mut model = UiModel::new();
    let peer = ProfileIdentity::generate()
        .transport_identity()
        .expect("a peer");
    let human = EndpointId::parse("human").expect("an endpoint");
    model.received(Received {
        row: RowId::from_stored(1),
        origin: Origin::Direct {
            peer: peer.clone(),
            endpoint: human.clone(),
        },
        envelope: HumanChatV2 {
            v: 2,
            kind: MessageKind::Text,
            app_message_id: format!("{:032x}", 1),
            text: text.to_owned(),
            reply_to: None,
            sent_at_ms: None,
            from_endpoint: None,
        },
        received_at: 1,
    });
    view.select(ConversationKey::Direct {
        peer,
        endpoint: Some(human),
    });
    let _ = view.take_events(&model);
    view.render(&model);
    let rows = view.window().get_messages();
    assert_eq!(rows.row_count(), 1, "one message drawn");
    let row = rows.row_data(0).expect("the message");
    let drawn_lines: Vec<Value> = row
        .lines
        .iter()
        .map(|l| {
            json!({
                "kind": format!("{:?}", l.kind),
                "level": l.level,
                "depth": l.depth,
                "quoted": l.quoted,
                "marker": l.marker.as_str(),
                "text": l.text.as_str(),
            })
        })
        .collect();
    let link_labels: Vec<Value> = row.links.iter().map(|l| json!(l.label.as_str())).collect();
    json!({ "lines": drawn_lines, "links": link_labels })
}

#[test]
fn every_case_draws_as_the_golden_says() {
    // Once per thread: the testing backend refuses a second init.
    i_slint_backend_testing::init_no_event_loop();
    let path = golden_path();
    let golden: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("the golden")).expect("JSON");
    let cases = golden["cases"].as_array().expect("cases");
    assert!(!cases.is_empty(), "a golden with no case proves nothing");
    let write = std::env::var_os("RENDER_PARITY_WRITE").is_some();
    let mut rewritten = golden.clone();
    let mut differ = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let name = case["name"].as_str().expect("a name");
        let text = case["text"].as_str().expect("a text");
        let got = drawn(text);
        if write {
            rewritten["cases"][i]["drawn"] = got;
        } else if case["drawn"] != got {
            differ.push(format!(
                "{name}:\n  golden {}\n  drawn  {got}",
                case["drawn"]
            ));
        }
    }
    if write {
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&rewritten).expect("JSON") + "\n",
        )
        .expect("the golden written");
        return;
    }
    assert!(
        differ.is_empty(),
        "drawn differently:\n{}",
        differ.join("\n")
    );
}
