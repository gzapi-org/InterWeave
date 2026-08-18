// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The parser agrees with the frozen HumanChatV2 envelope vectors.
//!
//! The 23 verdicts in `fixtures/human-chat-v2/` are recomputed
//! independently by `verify_fixture_vectors.py` from the specification, so
//! they are not this crate's opinion written down. Two implementations
//! agreeing is the point.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use interweave_human_chat_protocol::HumanChatV2;

fn fixture() -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("crates/human/<crate> is three levels below the root")
        .join("fixtures/human-chat-v2/human-chat-v2-envelope.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

#[test]
fn every_frozen_verdict_matches() {
    let doc = fixture();
    let vectors = doc["vectors"].as_array().expect("vectors");
    let (mut valid, mut invalid) = (0, 0);

    for v in vectors {
        let name = v["name"].as_str().expect("name");
        let expected = v["valid"].as_bool().expect("valid");
        // The fixture holds the envelope as a JSON value; the parser takes
        // text, which is what actually arrives over the wire.
        let text = serde_json::to_string(&v["envelope"]).expect("re-serialises");

        let parsed = HumanChatV2::parse(&text);
        assert_eq!(
            parsed.is_ok(),
            expected,
            "vector '{name}': fixture says valid={expected}, parser says {}\n  {text}\n  {:?}",
            parsed.is_ok(),
            parsed.err()
        );
        if expected {
            valid += 1;
        } else {
            invalid += 1;
        }
    }

    assert_eq!(vectors.len(), 23, "the fixture changed size");
    // A one-sided fixture would pass with a parser that accepts anything.
    assert!(valid > 0 && invalid > 0, "one-sided fixture");
}

#[test]
fn the_markdown_body_vector_keeps_its_source_verbatim() {
    // The subset is a RENDERING contract; the envelope carries the source
    // unchanged, and the receiver must always be able to see what was
    // actually sent.
    let doc = fixture();
    let vector = doc["vectors"]
        .as_array()
        .expect("vectors")
        .iter()
        .find(|v| v["name"] == "markdown-body")
        .expect("the markdown-body vector exists");
    let text = serde_json::to_string(&vector["envelope"]).expect("re-serialises");
    let parsed = HumanChatV2::parse(&text).expect("parses");
    let original = vector["envelope"]["text"].as_str().expect("text");
    assert_eq!(parsed.text, original);
    assert!(parsed.text.contains("##"), "markdown should survive intact");
}
