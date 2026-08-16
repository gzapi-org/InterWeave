// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! Stage 0 exit gate: the frozen vectors load from Rust, with no product
//! networking and no product crate in the graph.
//!
//! These tests deliberately do NOT recompute a vector.
//! `tools/checks/verify_fixture_vectors.py` does that, from the specification
//! rather than from the fixture, and a second implementation of the same
//! algorithm here would only give the repository two answers that agree until
//! the day one of them is edited. The question left over — can an
//! implementation read what was frozen — is the one a Python checker cannot
//! answer, and is what these cover.

use interweave_test_support::{fixtures, hex};

#[test]
fn every_vector_file_loads_and_declares_its_algorithm() {
    let files = fixtures::vector_files();
    assert!(
        !files.is_empty(),
        "no vector files found under fixtures/ — the loader is looking in the wrong place"
    );

    for file in &files {
        assert!(
            file.algorithm_id().is_some(),
            "{}: no algorithm.id — nothing can recompute this file",
            file.relative_path
        );
        assert!(
            !file.vectors().is_empty(),
            "{}: declares an algorithm but freezes no vectors",
            file.relative_path
        );
        for (i, vector) in file.vectors().iter().enumerate() {
            assert!(
                vector.get("name").and_then(|n| n.as_str()).is_some(),
                "{}: vector {i} has no name to report a drift against",
                file.relative_path
            );
        }
    }
}

#[test]
fn hex_inputs_decode() {
    // Every `*_hex` field in every vector is canonical lower-case hex of even
    // length. The Python checker decodes these too, but only for the
    // algorithms it implements; this covers the fields no algorithm consumes
    // yet, which are exactly the ones a typo survives in.
    for file in fixtures::vector_files() {
        for vector in file.vectors() {
            let Some(object) = vector.as_object() else {
                continue;
            };
            for (key, value) in object {
                if !key.ends_with("_hex") {
                    continue;
                }
                let Some(text) = value.as_str() else { continue };
                assert!(
                    hex::decode(text).is_ok(),
                    "{}: field {key} is not canonical lower-case hex: {text}",
                    file.relative_path
                );
            }
        }
    }
}

#[test]
fn a_named_vector_is_reachable_by_name() {
    // The loader is only useful to a conformance suite if it can pick out one
    // case. This golden is re-frozen by ADR-0047 and quoted in the ADR itself,
    // so a rename would be visible in review rather than silent.
    let file = fixtures::vector_files()
        .into_iter()
        .find(|f| f.relative_path == "fixtures/direct-v2/direct-content-fingerprint-v1.json")
        .expect("the direct content fingerprint fixture is present");

    let golden = file
        .vector("golden-text-plain-hello")
        .expect("the ADR-0047 golden case is present");

    assert_eq!(
        golden.get("media_type").and_then(|v| v.as_str()),
        Some("text/plain")
    );
    assert_eq!(
        hex::decode(
            golden
                .get("payload_hex")
                .and_then(|v| v.as_str())
                .expect("the golden carries its payload as hex")
        ),
        Ok(b"hello".to_vec())
    );
}
