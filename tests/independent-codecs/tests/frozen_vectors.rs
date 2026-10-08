// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! Every frozen vector of the four shapes decodes under the independent
//! codec, its fields match the vector's declared ones, and it re-encodes
//! byte-equal. A vector added to a fixture file is picked up here without
//! an edit: each file is read whole and a file with no vectors fails.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use interweave_independent_codecs::broadcast_v1::BroadcastMessageV1;
use interweave_independent_codecs::direct_v2::DirectMessageV2;
use interweave_independent_codecs::fingerprint;
use interweave_independent_codecs::ipc_v2::{self, Class};
use serde_json::Value;

fn vectors(rel: &str) -> Vec<Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(rel);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let doc: Value = serde_json::from_str(&text).unwrap();
    let v = doc["vectors"].as_array().cloned().unwrap_or_default();
    assert!(!v.is_empty(), "{rel} holds no vectors");
    v
}

fn hex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd hex");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn id16(s: &str) -> [u8; 16] {
    hex(s).try_into().unwrap()
}

fn opt_str(v: &Value) -> Option<String> {
    v.as_str().map(str::to_owned)
}

#[test]
fn direct_v2_frames_decode_to_their_fields_and_reencode_byte_equal() {
    for v in vectors("direct-v2/direct-message-v2-frame.json") {
        let name = v["name"].as_str().unwrap();
        let frame = hex(v["frame_hex"].as_str().unwrap());
        assert_eq!(
            frame.len() as u64,
            v["frame_len"].as_u64().unwrap(),
            "{name}"
        );
        let got = DirectMessageV2::decode(&frame).unwrap_or_else(|e| panic!("{name}: {e}"));
        let want = DirectMessageV2 {
            message_id: id16(v["message_id"].as_str().unwrap()),
            sent_at_ms: v["sent_at_ms"].as_u64().unwrap(),
            source_endpoint: v["source_endpoint"].as_str().unwrap().to_owned(),
            destination_endpoint: opt_str(&v["destination_endpoint"]),
            media_type: opt_str(&v["media_type"]),
            payload: hex(v["payload_hex"].as_str().unwrap()),
        };
        assert_eq!(got, want, "{name}");
        assert_eq!(got.encode().unwrap(), frame, "{name}");
    }
}

#[test]
fn broadcast_v1_frames_decode_to_their_fields_and_reencode_byte_equal() {
    for v in vectors("gossipsub/broadcast-message-v1-frame.json") {
        let name = v["name"].as_str().unwrap();
        let frame = hex(v["frame_hex"].as_str().unwrap());
        assert_eq!(
            frame.len() as u64,
            v["frame_len"].as_u64().unwrap(),
            "{name}"
        );
        assert_eq!(v["version"].as_u64(), Some(1), "{name}");
        let got = BroadcastMessageV1::decode(&frame).unwrap_or_else(|e| panic!("{name}: {e}"));
        let want = BroadcastMessageV1 {
            message_id: id16(v["message_id"].as_str().unwrap()),
            sent_at_ms: v["sent_at_ms"].as_u64().unwrap(),
            media_type: opt_str(&v["media_type"]),
            payload: hex(v["payload_hex"].as_str().unwrap()),
        };
        assert_eq!(got, want, "{name}");
        assert_eq!(got.encode().unwrap(), frame, "{name}");
    }
}

#[test]
fn fingerprints_match_every_vector() {
    for v in vectors("direct-v2/direct-content-fingerprint-v1.json") {
        let name = v["name"].as_str().unwrap();
        let media = v["media_type"].as_str();
        let payload = hex(v["payload_hex"].as_str().unwrap());
        let got =
            fingerprint::fingerprint(media, &payload).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(got.to_vec(), hex(v["sha256"].as_str().unwrap()), "{name}");
    }
}

#[test]
fn the_fingerprint_domain_is_the_fixtures() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/direct-v2/direct-content-fingerprint-v1.json");
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(
        fingerprint::DOMAIN,
        hex(doc["algorithm"]["domain_hex"].as_str().unwrap()).as_slice()
    );
}

#[test]
fn ipc_v2_frames_decode_to_their_bodies_and_reencode_byte_equal() {
    for v in vectors("ipc-v2/ipc-v2-frame-golden.json") {
        let name = v["name"].as_str().unwrap();
        let frame = hex(v["frame_hex"].as_str().unwrap());
        let env = ipc_v2::decode_frame(&frame).unwrap_or_else(|e| panic!("{name}: {e}"));
        // The body is read by two parsers that share nothing; equal values
        // is the field-level check, byte equality the encoding one.
        let theirs: Value = serde_json::from_slice(&env.encode_body()).unwrap();
        assert_eq!(theirs, v["body"], "{name}");
        assert_eq!(
            Some(env.class),
            class_of(v["body"]["type"].as_str().unwrap()),
            "{name}"
        );
        assert_eq!(
            ipc_v2::encode_frame(&env.encode_body()).unwrap(),
            frame,
            "{name}"
        );
    }
}

fn class_of(t: &str) -> Option<Class> {
    Some(match t {
        "hello" => Class::Hello,
        "hello_response" => Class::HelloResponse,
        "close" => Class::Close,
        "request" => Class::Request,
        "response" => Class::Response,
        "cancel" => Class::Cancel,
        "event" => Class::Event,
        "server_state" => Class::ServerState,
        "ping" => Class::Ping,
        "pong" => Class::Pong,
        _ => return None,
    })
}

/// `LOCAL-IPC.md` §Framing: N is 1..=131,072 and the prefix is outside
/// it. A body of exactly the ceiling frames and decodes; one byte more and
/// a zero length are refused at the prefix, before any body is read.
#[test]
fn the_frame_ceiling_is_exact_at_both_ends() {
    let pad = |n: usize| {
        let head = r#"{"type":"ping","nonce":""#;
        let tail = r#""}"#;
        format!("{head}{}{tail}", "A".repeat(n - head.len() - tail.len()))
    };
    let at = pad(ipc_v2::MAX_BODY_BYTES);
    let frame = ipc_v2::encode_frame(at.as_bytes()).unwrap();
    assert_eq!(frame[..4], [0x00, 0x02, 0x00, 0x00]);
    assert_eq!(ipc_v2::decode_frame(&frame).unwrap().class, Class::Ping);

    let over = pad(ipc_v2::MAX_BODY_BYTES + 1);
    assert!(ipc_v2::encode_frame(over.as_bytes()).is_err());
    let mut raw = u32::try_from(over.len()).unwrap().to_be_bytes().to_vec();
    raw.extend_from_slice(over.as_bytes());
    let err = ipc_v2::decode_frame(&raw).unwrap_err();
    assert!(err.0.contains("above"), "{err}");

    // A prefix declaring the ceiling plus one with NO body behind it is
    // refused for its length, not for the missing bytes: the bound comes
    // before the read.
    let err = ipc_v2::decode_frame(&[0x00, 0x02, 0x00, 0x01]).unwrap_err();
    assert!(err.0.contains("above"), "{err}");
    assert!(
        ipc_v2::decode_frame(&[0, 0, 0, 0])
            .unwrap_err()
            .0
            .contains("zero")
    );
}
