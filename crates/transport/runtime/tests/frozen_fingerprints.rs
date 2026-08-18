// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Rust fingerprint reproduces every frozen vector.
//!
//! `verify_fixture_vectors.py` recomputes these from the specification, so
//! the file is an independent statement of what the algorithm produces —
//! not a transcript of this implementation. Until now nothing in Rust
//! consumed it, which meant two implementations could disagree with only
//! the Python one being checked.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use interweave_transport_runtime::direct_content_fingerprint_v1;

fn fixture() -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("crates/transport/<crate> is three levels below the root")
        .join("fixtures/direct-v2/direct-content-fingerprint-v1.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

fn from_hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex pair"))
        .collect()
}

#[test]
fn every_frozen_vector_reproduces() {
    let doc = fixture();
    let vectors = doc["vectors"].as_array().expect("vectors");
    assert!(!vectors.is_empty(), "the fixture is empty");

    let mut absent = 0;
    let mut present = 0;

    for v in vectors {
        let name = v["name"].as_str().expect("name");
        let media = v.get("media_type").and_then(|m| m.as_str());
        let payload = v
            .get("payload_hex")
            .and_then(|p| p.as_str())
            .map(from_hex)
            .unwrap_or_default();
        let expected = v["sha256"].as_str().expect("sha256");

        let computed = direct_content_fingerprint_v1(media, &payload)
            .unwrap_or_else(|e| panic!("vector '{name}' does not compute: {e}"));
        assert_eq!(
            computed.to_hex(),
            expected,
            "vector '{name}' drifted\n  media: {media:?}\n  payload: {} bytes",
            payload.len()
        );

        if media.is_some() {
            present += 1;
        } else {
            absent += 1;
        }
    }

    assert_eq!(vectors.len(), 7, "the fixture changed size");
    // Both media cases must be covered, since the present/absent
    // distinction is the one the domain framing exists to preserve.
    assert!(absent > 0 && present > 0, "one-sided media coverage");
}

#[test]
fn the_frozen_vectors_do_not_collide() {
    // The fixture's own edge cases are only useful if they distinguish
    // something. The Python verifier asserts this too; asserting it here
    // means a Rust-side change that collapsed two cases would be caught
    // by the Rust suite rather than only by the tree checks.
    let doc = fixture();
    let mut seen: Vec<(String, String)> = Vec::new();
    for v in doc["vectors"].as_array().expect("vectors") {
        let name = v["name"].as_str().expect("name").to_owned();
        let media = v.get("media_type").and_then(|m| m.as_str());
        let payload = v
            .get("payload_hex")
            .and_then(|p| p.as_str())
            .map(from_hex)
            .unwrap_or_default();
        let hex = direct_content_fingerprint_v1(media, &payload)
            .expect("computes")
            .to_hex();
        if let Some((other, _)) = seen.iter().find(|(_, h)| h == &hex) {
            panic!("'{name}' collides with '{other}'");
        }
        seen.push((name, hex));
    }
}
