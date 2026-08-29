// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! These types agree with the frozen IPC schemas and fixtures.

// Helpers read files and index JSON outside `#[test]` functions, where
// clippy.toml's allow-*-in-tests does not reach.
#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use interweave_ipc_protocol::{
    AuthorityDomain, ClientInfo, Hello, HelloTag, IPC_MAJOR, IpcVersion, MAX_BODY_BYTES,
    MAX_REQUESTED, RequestedCapability, encode_frame,
};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("crates/api/<crate> is three levels below the root")
        .to_path_buf()
}

fn json_at(relative: &str) -> serde_json::Value {
    let path = root().join(relative);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

fn schema(name: &str) -> serde_json::Value {
    json_at(&format!("architecture/contracts/schemas/{name}"))
}

fn string_set(value: &serde_json::Value) -> BTreeSet<String> {
    value
        .as_array()
        .expect("array")
        .iter()
        .map(|v| v.as_str().expect("string").to_owned())
        .collect()
}

#[test]
fn the_requested_capability_vocabulary_matches_the_schema() {
    // `hello.requested_capabilities` references the FULL capability enum,
    // admin included: a client may ask for anything, and refusing is the
    // server's job. That is why this type is separate from the granted
    // DataCapability set rather than an alias for it.
    let declared = string_set(&schema("ipc/capability.schema.json")["enum"]);
    let ours: BTreeSet<String> = [
        RequestedCapability::Events,
        RequestedCapability::Commands,
        RequestedCapability::EndpointsQuery,
        RequestedCapability::AdminEndpoints,
        RequestedCapability::AdminShutdown,
    ]
    .iter()
    .map(|c| {
        serde_json::to_value(c)
            .expect("ser")
            .as_str()
            .expect("string")
            .to_owned()
    })
    .collect();
    assert_eq!(ours, declared);
}

#[test]
fn hello_matches_its_schema_shape() {
    let doc = schema("ipc/hello.schema.json");
    let required = string_set(&doc["required"]);
    for field in ["type", "ipc_version", "client"] {
        assert!(required.contains(field), "{field} should be required");
    }
    // `endpoint` is optional: diagnostics clients omit it, and admin
    // connections must.
    assert!(!required.contains("endpoint"));
    assert_eq!(doc["additionalProperties"], serde_json::json!(false));
    assert_eq!(
        doc["properties"]["ipc_version"]["properties"]["major"]["const"],
        serde_json::json!(IPC_MAJOR)
    );
    assert_eq!(
        doc["properties"]["requested_capabilities"]["maxItems"],
        serde_json::json!(MAX_REQUESTED)
    );
    assert_eq!(
        doc["properties"]["features"]["maxItems"],
        serde_json::json!(MAX_REQUESTED)
    );

    // A minimal schema-shaped hello deserializes into the Rust type.
    let minimal = serde_json::json!({
        "type": "hello",
        "ipc_version": { "major": 2, "minor": 0 },
        "client": { "kind": "human-client" }
    });
    let parsed: Hello = serde_json::from_value(minimal).expect("de");
    assert_eq!(parsed.client.kind, "human-client");
    assert!(parsed.endpoint.is_none());

    // And a Rust-built hello serializes into that shape.
    let built = Hello {
        frame_type: HelloTag::Hello,
        ipc_version: IpcVersion { major: 2, minor: 0 },
        client: ClientInfo {
            kind: "human-client".to_owned(),
            version: None,
        },
        endpoint: None,
        requested_capabilities: BTreeSet::new(),
        features: BTreeSet::new(),
    };
    let json = serde_json::to_value(&built).expect("ser");
    assert_eq!(json["type"], "hello");
    // Empty optional collections are omitted, not emitted empty.
    assert!(json.get("requested_capabilities").is_none());
    assert!(json.get("features").is_none());
}

#[test]
fn the_frame_ceiling_matches_the_contract_and_the_fixture() {
    // The number lives in three places and must be one number: this
    // crate, the prose contract, and the frozen payload-fit vectors.
    assert_eq!(MAX_BODY_BYTES, 131_072);

    let fixture = json_at("fixtures/ipc-v2/ipc-v2-payload-fit.json");
    let vectors = fixture["vectors"].as_array().expect("vectors");
    assert!(!vectors.is_empty(), "the payload-fit fixture is empty");

    for v in vectors {
        let body = v["body_bytes"].as_u64().expect("body_bytes") as usize;
        let headroom = v["envelope_headroom_bytes"]
            .as_u64()
            .expect("envelope_headroom_bytes") as usize;
        // The invariant the fixture exists to prove.
        assert!(
            body <= MAX_BODY_BYTES,
            "{} exceeds the frame ceiling",
            v["name"]
        );
        assert_eq!(body + headroom, MAX_BODY_BYTES, "{} headroom", v["name"]);

        // And the codec agrees: a body of that size frames successfully,
        // with the prefix the fixture recorded.
        let prefix = v["frame_length_prefix_hex"].as_str().expect("prefix");
        // A real object of exactly that size: `encode_frame` enforces
        // every rule the decoder does, so a body of raw padding is no
        // longer a frame this process will produce.
        let padded = format!(r#"{{"pad":"{}"}}"#, "x".repeat(body - 10));
        assert_eq!(padded.len(), body);
        let framed = encode_frame(&padded).expect("encodes");
        assert_eq!(hex(&framed[..4]), prefix, "{} prefix", v["name"]);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn the_authority_domain_is_not_a_frame_field() {
    // The structural claim behind ADR-0037, asserted rather than described:
    // the same bytes yield different authority depending only on the
    // socket they arrived on.
    let json = serde_json::json!({
        "type": "hello",
        "ipc_version": { "major": 2, "minor": 0 },
        "client": { "kind": "admin" },
        "requested_capabilities": ["admin.shutdown"]
    });
    let hello: Hello = serde_json::from_value(json).expect("de");

    assert!(hello.evaluate(AuthorityDomain::Data, false).is_err());
    let granted = hello
        .evaluate(AuthorityDomain::Admin, false)
        .expect("admin socket grants");
    assert!(!granted.granted_admin.is_empty());
}
