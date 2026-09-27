// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The runtime's `PeerPath` vocabulary agrees with
//! `contracts/schemas/connectivity/peer-path.schema.json` (plan §15's
//! exit gate, carried from Stage 11's close).
//!
//! The schema has three values and the runtime two: its `none` is the
//! absence of a path, which the runtime spells `Option::None` rather than
//! as a variant. The mapping lives here, in the test, as the plan asks --
//! so the assertion is that the two vocabularies differ by exactly that
//! one value, in both directions.

// Helpers outside `#[test]` read a file and index JSON; a missing or
// malformed schema is a broken checkout, so panicking is correct there.
#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use interweave_transport_libp2p::PeerPath;

fn schema_values() -> BTreeSet<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("crates/transport/<crate> is three levels below the root")
        .join("architecture/contracts/schemas/connectivity/peer-path.schema.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let doc: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON: {e}"));
    doc["enum"]
        .as_array()
        .expect("peer-path is an enum schema")
        .iter()
        .map(|v| v.as_str().expect("string members").to_owned())
        .collect()
}

/// Every runtime path. The match makes it exhaustive: a new variant fails
/// to compile here before it can be missing from the comparison.
fn every_path() -> [PeerPath; 2] {
    let all = [PeerPath::Relayed, PeerPath::Direct];
    for path in all {
        match path {
            PeerPath::Relayed | PeerPath::Direct => {}
        }
    }
    all
}

/// The schema's spelling of a peer's path, absence included.
fn schema_value(path: Option<PeerPath>) -> String {
    match path {
        Some(path) => serde_json::to_value(path)
            .expect("a path serializes")
            .as_str()
            .expect("a path serializes as a string")
            .to_owned(),
        None => "none".to_owned(),
    }
}

#[test]
fn the_runtime_vocabulary_with_absence_is_the_schema_enum_exactly() {
    let ours: BTreeSet<String> = every_path()
        .into_iter()
        .map(Some)
        .chain([None])
        .map(schema_value)
        .collect();
    assert_eq!(ours, schema_values());
}

#[test]
fn every_path_serializes_as_its_label_and_round_trips() {
    for path in every_path() {
        let json = serde_json::to_value(path).expect("serializes");
        assert_eq!(json, serde_json::Value::from(path.label()), "{path:?}");
        let back: PeerPath = serde_json::from_value(json).expect("deserializes");
        assert_eq!(back, path);
    }
}

#[test]
fn none_is_the_absence_of_a_path_not_a_path() {
    // A `none` arriving where a path is expected is refused: a connected
    // peer always has one, and a variant for "not reachable" is the
    // shape the schema's mapping rules out.
    assert!(serde_json::from_value::<PeerPath>(serde_json::Value::from("none")).is_err());
    let absent: Option<PeerPath> = None;
    assert_eq!(schema_value(absent), "none");
}

#[test]
fn every_schema_value_names_a_path_or_its_absence() {
    for value in schema_values() {
        let parsed = serde_json::from_value::<PeerPath>(serde_json::Value::from(value.as_str()));
        match (value.as_str(), parsed) {
            ("none", Err(_)) => {}
            (_, Ok(path)) => assert_eq!(path.label(), value),
            (other, Err(e)) => panic!("schema value {other:?} names no runtime path: {e}"),
        }
    }
}
