// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! These types agree with the frozen discovery schemas.
//!
//! The transport-api version of this suite initially compared only which
//! members were required, and six value-level disagreements passed it.
//! This one compares constraints from the start: enum membership, caps,
//! and the set semantics.

// Helpers read files and index JSON outside `#[test]` functions, where
// clippy.toml's allow-*-in-tests does not reach. A missing schema is a
// broken checkout and panicking is the right answer.
#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use interweave_discovery_api::{
    MAX_ADDRESS_BYTES, MAX_ADDRESSES, MAX_PROTOCOL_OBSERVATIONS, MAX_PROVIDER_NAME_BYTES,
    ProviderHealth, ProviderMode, ProviderScope,
};

fn schema(relative: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("crates/api/<crate> is three levels below the root")
        .join("architecture/contracts/schemas")
        .join(relative);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

fn string_set(value: &serde_json::Value) -> BTreeSet<String> {
    value
        .as_array()
        .expect("array")
        .iter()
        .map(|v| v.as_str().expect("string").to_owned())
        .collect()
}

fn serialized<T: serde::Serialize>(values: &[T]) -> BTreeSet<String> {
    values
        .iter()
        .map(|v| {
            serde_json::to_value(v)
                .expect("ser")
                .as_str()
                .expect("string")
                .to_owned()
        })
        .collect()
}

#[test]
fn provider_health_matches_its_schema() {
    let declared = string_set(&schema("discovery/provider-health.schema.json")["enum"]);
    let ours = serialized(&[
        ProviderHealth::Healthy,
        ProviderHealth::Degraded,
        ProviderHealth::Unavailable,
    ]);
    assert_eq!(ours, declared);
}

#[test]
fn provider_scope_and_mode_match_the_descriptor_schema() {
    let doc = schema("discovery/provider-descriptor.schema.json");
    assert_eq!(
        serialized(&[
            ProviderScope::Local,
            ProviderScope::Configured,
            ProviderScope::Network
        ]),
        string_set(&doc["properties"]["scope"]["enum"])
    );
    assert_eq!(
        serialized(&[
            ProviderMode::Passive,
            ProviderMode::Active,
            ProviderMode::Mixed
        ]),
        string_set(&doc["properties"]["mode"]["enum"])
    );
    assert_eq!(
        doc["properties"]["name"]["maxLength"],
        serde_json::json!(MAX_PROVIDER_NAME_BYTES)
    );
}

#[test]
fn candidate_caps_and_set_semantics_match_the_schema() {
    let doc = schema("discovery/candidate-peer.schema.json");
    let props = &doc["properties"];

    assert_eq!(
        props["addresses"]["maxItems"],
        serde_json::json!(MAX_ADDRESSES)
    );
    assert_eq!(
        props["addresses"]["items"]["maxLength"],
        serde_json::json!(MAX_ADDRESS_BYTES)
    );
    assert_eq!(
        props["protocol_observations"]["maxItems"],
        serde_json::json!(MAX_PROTOCOL_OBSERVATIONS)
    );

    // Both collections are SETS in the schema, which is why the Rust type
    // uses BTreeSet rather than Vec — a Vec would let duplicates consume
    // the cap while adding no information.
    assert_eq!(props["addresses"]["uniqueItems"], serde_json::json!(true));
    assert_eq!(
        props["protocol_observations"]["uniqueItems"],
        serde_json::json!(true)
    );

    // Required members. `expires_at` is deliberately NOT among them: a
    // provider that cannot express expiry omits it, and the manager
    // bounds the candidate itself.
    let required = string_set(&doc["required"]);
    for field in ["peer_id", "addresses", "source", "observed_at"] {
        assert!(required.contains(field), "{field} should be required");
    }
    assert!(!required.contains("expires_at"));
    assert!(!required.contains("protocol_observations"));
}

#[test]
fn a_protocol_observation_matches_its_declared_shape() {
    let doc = schema("discovery/candidate-peer.schema.json");
    let items = &doc["properties"]["protocol_observations"]["items"];
    let required = string_set(&items["required"]);
    for field in ["protocol_id", "supported", "observed_at"] {
        assert!(required.contains(field), "{field} should be required");
    }
    assert_eq!(items["additionalProperties"], serde_json::json!(false));
}
