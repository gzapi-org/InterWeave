// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The validator agrees with the frozen configuration vectors.
//!
//! `fixtures/config/config-v2-cross-field.json` states, for sixteen
//! configurations, whether they satisfy the cross-field rules. Those
//! verdicts are recomputed independently by
//! `tools/checks/verify_fixture_vectors.py` from the specification, so
//! they are not this crate's opinion written down — which is exactly what
//! makes them worth testing against.
//!
//! Two agreements are checked, and the second is the one that catches
//! real drift: every vector must **deserialize** into these types, and
//! every verdict must match. A type that could not parse the frozen
//! configurations would be a contract failure even if its rules were
//! right.

// Reads a file and indexes JSON outside `#[test]` functions.
#![allow(clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use interweave_profile_config::ProfileConfig;

fn fixture() -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("crates/config/<crate> is three levels below the root")
        .join("fixtures/config/config-v2-cross-field.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

#[test]
fn every_frozen_vector_deserializes_and_its_verdict_matches() {
    let doc = fixture();
    let vectors = doc["vectors"].as_array().expect("vectors");
    assert!(!vectors.is_empty(), "the fixture is empty");

    let mut checked = 0;
    let mut valid_seen = 0;
    let mut invalid_seen = 0;

    for v in vectors {
        let name = v["name"].as_str().expect("name");
        let expected = v["valid"].as_bool().expect("valid");

        // Deserialization is half the agreement. A frozen configuration
        // these types cannot parse is a contract failure regardless of
        // what the rules would have said about it.
        let config: ProfileConfig = serde_json::from_value(v["config"].clone())
            .unwrap_or_else(|e| panic!("vector '{name}' does not deserialize: {e}"));

        let errors = config.validate();
        let actual = errors.is_empty();
        assert_eq!(
            actual,
            expected,
            "vector '{name}': fixture says valid={expected}, validator says {actual}\n  errors: {}",
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        );

        checked += 1;
        if expected {
            valid_seen += 1;
        } else {
            invalid_seen += 1;
        }
    }

    assert_eq!(checked, 16, "the fixture changed size");
    // Both directions must be exercised. A suite that only ever saw valid
    // configurations would pass with a validator that returns no errors.
    assert!(valid_seen > 0 && invalid_seen > 0, "one-sided fixture");
}

#[test]
fn a_rejected_vector_reports_a_reason_naming_its_cause() {
    // Not merely "invalid": the operator has to be able to act on it.
    let doc = fixture();
    for v in doc["vectors"].as_array().expect("vectors") {
        if v["valid"].as_bool().expect("valid") {
            continue;
        }
        let name = v["name"].as_str().expect("name");
        let config: ProfileConfig =
            serde_json::from_value(v["config"].clone()).expect("deserializes");
        let errors = config.validate();
        assert!(!errors.is_empty(), "vector '{name}' produced no error");
        for e in &errors {
            let msg = e.to_string();
            assert!(!msg.is_empty(), "vector '{name}' has an empty message");
            // Every message names a value, not only a rule.
            assert!(
                msg.chars().any(|c| c.is_ascii_digit()) || msg.contains('\''),
                "vector '{name}' message names no value: {msg}"
            );
        }
    }
}

#[test]
fn the_widening_vectors_are_rejected_for_the_right_reason() {
    // The rule most worth pinning: ADR-0012's narrow-never-widen. A
    // validator that rejected these for some unrelated reason would pass
    // the verdict test while enforcing nothing.
    let doc = fixture();
    for v in doc["vectors"].as_array().expect("vectors") {
        let name = v["name"].as_str().expect("name");
        if !name.contains("widens") && name != "subset-against-empty-profile-trust" {
            continue;
        }
        let config: ProfileConfig =
            serde_json::from_value(v["config"].clone()).expect("deserializes");
        let errors = config.validate();
        assert!(
            errors.iter().any(|e| {
                matches!(
                    e,
                    interweave_profile_config::ConfigError::SubsetWidensTrust { .. }
                )
            }),
            "vector '{name}' was rejected, but not for widening: {errors:?}"
        );
    }
}
