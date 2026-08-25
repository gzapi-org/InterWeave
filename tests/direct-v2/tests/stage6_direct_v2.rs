// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Stage 6 exit gate: `/interweave/direct/2.0.0` between real peers.
//!
//! The network scenarios land with the backend that makes them
//! possible. What is here at the opening commit is the one claim already
//! true: the frozen vectors this stage is built AGAINST exist and hold
//! the shape the fixture README promises. A stage that opened against
//! missing fixtures would build a codec with nothing to be byte-identical
//! to, so this fails first.

use interweave_test_support::fixtures;

/// The six framing vectors `fixtures/direct-v2/README.md` describes.
#[test]
fn the_frame_vectors_this_stage_is_built_against_exist() {
    let file = fixtures::load("direct-v2/direct-message-v2-frame.json");
    let vectors = file["vectors"].as_array().expect("a vectors array");
    assert_eq!(
        vectors.len(),
        6,
        "six framing vectors, per the fixture README"
    );
    assert_eq!(file["status"], "frozen");
    for vector in vectors {
        for field in ["message_id", "frame_hex", "frame_len"] {
            assert!(
                vector.get(field).is_some(),
                "vector {:?} lacks `{field}`",
                vector["name"]
            );
        }
    }
}

/// The seven fingerprint vectors, including the ADR-0047 golden.
#[test]
fn the_fingerprint_vectors_this_stage_is_built_against_exist() {
    let file = fixtures::load("direct-v2/direct-content-fingerprint-v1.json");
    let vectors = file["vectors"].as_array().expect("a vectors array");
    assert_eq!(
        vectors.len(),
        7,
        "seven fingerprint vectors, per the fixture README"
    );
    assert!(
        vectors
            .iter()
            .any(|v| v["name"] == "golden-text-plain-hello"),
        "the ADR-0047 golden is present"
    );
}
