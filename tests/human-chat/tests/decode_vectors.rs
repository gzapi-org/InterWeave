// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The frozen decode-direction compression vectors
//! (`fixtures/human-chat-v2/human-chat-v2-brotli-decode.json`, ADR-0050)
//! through the shared decoder every consumer uses: a stream that decodes
//! gives exactly its frozen bytes, and the cap-abort case is refused as
//! over the ceiling -- by the incremental decoder, which stops reading at
//! the first byte past it.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use interweave_human_chat_protocol::{
    ContentEncoding, DecodeError, HumanChatV2, MAX_DECOMPRESSED_BYTES, decode_envelope_bytes,
};
use serde_json::Value;

fn fixture() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/human-chat-v2/human-chat-v2-brotli-decode.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("the fixture")).expect("json")
}

/// Standard base64, as the fixture stores the streams.
fn base64(text: &str) -> Vec<u8> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut bits = 0_u32;
    let mut count = 0;
    for byte in text.bytes().filter(|b| *b != b'=') {
        let value = ALPHABET
            .iter()
            .position(|a| *a == byte)
            .unwrap_or_else(|| panic!("not base64: {byte}"));
        bits = (bits << 6) | u32::try_from(value).expect("six bits");
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push(u8::try_from((bits >> count) & 0xff).expect("a byte"));
        }
    }
    out
}

fn vector<'a>(doc: &'a Value, name: &str) -> &'a Value {
    doc["vectors"]
        .as_array()
        .expect("vectors")
        .iter()
        .find(|v| v["name"] == name)
        .unwrap_or_else(|| panic!("no vector {name}"))
}

#[test]
fn the_fixture_declares_the_ceiling_the_decoder_enforces() {
    assert_eq!(
        fixture()["algorithm"]["ceiling"],
        serde_json::json!(MAX_DECOMPRESSED_BYTES)
    );
}

#[test]
fn a_frozen_stream_decodes_to_its_raw_envelope() {
    let doc = fixture();
    let v = vector(&doc, "an-envelope-decodes-to-its-raw-form");
    let decoded = decode_envelope_bytes(
        &base64(v["compressed_base64"].as_str().expect("stream")),
        ContentEncoding::Brotli,
    )
    .expect("decodes");
    assert_eq!(decoded, v["raw"].as_str().expect("raw"), "byte for byte");
    HumanChatV2::parse(&decoded).expect("and it is a valid envelope");
}

#[test]
fn exactly_the_ceiling_decodes_and_one_past_it_aborts() {
    let doc = fixture();
    let at = vector(&doc, "exactly-the-ceiling-decodes");
    let decoded = decode_envelope_bytes(
        &base64(at["compressed_base64"].as_str().expect("stream")),
        ContentEncoding::Brotli,
    )
    .expect("the ceiling itself decodes");
    assert_eq!(decoded.len(), MAX_DECOMPRESSED_BYTES);

    let past = vector(&doc, "one-past-the-ceiling-aborts");
    assert_eq!(past["result"], "aborts");
    assert_eq!(
        decode_envelope_bytes(
            &base64(past["compressed_base64"].as_str().expect("stream")),
            ContentEncoding::Brotli,
        ),
        Err(DecodeError::DecompressedTooLarge {
            limit: MAX_DECOMPRESSED_BYTES
        })
    );
}
