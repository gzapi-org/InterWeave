// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `human-chat/envelope.schema.json` and the Rust envelope agree, in both
//! directions (plan §17 (3)):
//! - every frozen envelope vector gets the same verdict from the schema
//!   and from `HumanChatV2::parse`;
//! - what this crate emits validates: the envelopes it serializes, and
//!   the raw bytes `encode_outbound` sends.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use interweave_human_chat_protocol::{HumanChatV2, MessageKind, encode_outbound};
use interweave_transport_api::EndpointId;
use serde_json::Value;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every schema under `architecture/contracts/schemas`, by `$id`, so the
/// envelope's `urn:` references resolve.
fn schema_docs(dir: &std::path::Path, out: &mut Vec<Value>) {
    for entry in std::fs::read_dir(dir)
        .expect("a schema directory")
        .flatten()
    {
        let path = entry.path();
        if path.is_dir() {
            schema_docs(&path, out);
        } else if path.extension().is_some_and(|x| x == "json")
            && path.file_name().is_some_and(|n| n != "manifest.json")
        {
            let text = std::fs::read_to_string(&path).expect("read");
            out.push(serde_json::from_str(&text).expect("json"));
        }
    }
}

fn validator() -> jsonschema::Validator {
    let dir = root().join("architecture/contracts/schemas");
    let mut docs = Vec::new();
    schema_docs(&dir, &mut docs);
    let pairs: Vec<(String, jsonschema::Resource)> = docs
        .into_iter()
        .filter_map(|doc| {
            let id = doc.get("$id").and_then(Value::as_str)?.to_owned();
            Some((id, jsonschema::Resource::from_contents(doc)))
        })
        .collect();
    let registry = jsonschema::Registry::new()
        .extend(pairs)
        .expect("register")
        .prepare()
        .expect("prepare");
    let schema: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("human-chat/envelope.schema.json")).expect("the schema"),
    )
    .expect("json");
    jsonschema::options()
        .with_registry(&registry)
        .build(&schema)
        .expect("compiles")
}

#[test]
fn the_schema_and_the_parser_give_every_frozen_vector_the_same_verdict() {
    let doc: Value = serde_json::from_str(
        &std::fs::read_to_string(root().join("fixtures/human-chat-v2/human-chat-v2-envelope.json"))
            .expect("the fixture"),
    )
    .expect("json");
    let schema = validator();
    let vectors = doc["vectors"].as_array().expect("vectors");
    assert_eq!(vectors.len(), 23, "the fixtures README's count");
    for v in vectors {
        let name = v["name"].as_str().expect("name");
        let valid = v["valid"].as_bool().expect("a verdict");
        let envelope = &v["envelope"];
        assert_eq!(schema.is_valid(envelope), valid, "the schema on {name}");
        assert_eq!(
            HumanChatV2::parse(&envelope.to_string()).is_ok(),
            valid,
            "the parser on {name}"
        );
    }
}

#[test]
fn what_this_crate_emits_validates_against_the_schema() {
    let schema = validator();
    let full = HumanChatV2 {
        v: 2,
        kind: MessageKind::Text,
        app_message_id: "0123456789abcdef0123456789abcdef".to_owned(),
        text: "**hello** | world".to_owned(),
        reply_to: Some("11111111111111111111111111111111".to_owned()),
        sent_at_ms: Some(1_786_600_000_000),
        from_endpoint: Some(EndpointId::parse("human").expect("valid")),
    };
    let minimal = HumanChatV2 {
        reply_to: None,
        sent_at_ms: None,
        from_endpoint: None,
        text: String::new(),
        ..full.clone()
    };
    for envelope in [&full, &minimal] {
        let value = serde_json::to_value(envelope).expect("serializes");
        assert!(schema.is_valid(&value), "{value}");
        // And the bytes the send side puts on the wire, read back as JSON.
        let encoded = encode_outbound(envelope, 49_152).expect("fits");
        let sent: Value = serde_json::from_slice(&encoded.bytes).expect("raw JSON");
        assert!(schema.is_valid(&sent), "{sent}");
    }
}
