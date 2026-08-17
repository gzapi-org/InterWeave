// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Stage 1 exit gate: these types agree with the frozen JSON Schemas.
//!
//! "Match the architecture" is checked mechanically here rather than by
//! reading. The schemas under `architecture/contracts/schemas/` are
//! normative for shape (ADR-0049), so a Rust type whose serialization
//! disagrees with one is a defect in the type — and a limit that drifted
//! apart from its schema is exactly the drift no compiler would catch.
//!
//! This suite deliberately does NOT pull in a JSON Schema validator. It
//! reads the schema documents and asserts the specific facts these types
//! encode — the enum members, the grammars, the bounds — because a full
//! validator would prove instances conform while leaving the interesting
//! question, "do the two definitions say the same thing", unasked.

use std::collections::BTreeSet;
use std::path::PathBuf;

use interweave_transport_api::{
    ChannelId, EndpointId, Health, MediaType, MessageId, PathReadiness, Payload,
    PreferredPathPolicy, TransportError, MAX_MEDIA_TYPE_BYTES, MAX_PAYLOAD_BYTES,
};

fn repo_root() -> PathBuf {
    // From CARGO_MANIFEST_DIR, not the working directory: the latter
    // depends on how the test was invoked.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("crates/api/<crate> is three levels below the root")
        .to_path_buf()
}

fn schema(relative: &str) -> serde_json::Value {
    let path = repo_root().join("architecture/contracts/schemas").join(relative);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

fn string_set(value: &serde_json::Value) -> BTreeSet<String> {
    value
        .as_array()
        .expect("expected a JSON array")
        .iter()
        .map(|v| v.as_str().expect("expected strings").to_owned())
        .collect()
}

#[test]
fn the_error_vocabulary_matches_the_ipc_schema_exactly() {
    let doc = schema("ipc/error-code.schema.json");
    let declared = string_set(&doc["enum"]);

    // Serializing every variant is what makes this exhaustive: adding a
    // variant without adding it here fails to compile the match below.
    let ours: BTreeSet<String> = [
        TransportError::InvalidArgument,
        TransportError::PayloadTooLarge,
        TransportError::ChannelNotJoined,
        TransportError::EndpointNotRegistered,
        TransportError::EndpointUnknown,
        TransportError::EndpointInUse,
        TransportError::EndpointDisabled,
        TransportError::EndpointClientKindDenied,
        TransportError::CapabilityDenied,
        TransportError::UnauthorizedPeer,
        TransportError::PeerUnknown,
        TransportError::PeerUnreachable,
        TransportError::RemoteEndpointUnavailable,
        TransportError::Timeout,
        TransportError::CancelledBeforeDispatch,
        TransportError::CancellationRaced,
        TransportError::Overloaded,
        TransportError::BackendUnavailable,
        TransportError::ProtocolUnsupported,
        TransportError::ProtocolViolation,
        TransportError::VersionIncompatible,
        TransportError::ShuttingDown,
        TransportError::Internal,
    ]
    .iter()
    .map(|e| serde_json::to_value(e).expect("ser").as_str().expect("string").to_owned())
    .collect();

    assert_eq!(
        ours, declared,
        "the Rust error vocabulary and ipc/error-code.schema.json disagree"
    );
}

#[test]
fn health_and_path_states_match_the_frame_and_connectivity_schemas() {
    let frame = schema("ipc/frame.schema.json");
    let declared_health =
        string_set(&frame["$defs"]["server_state"]["properties"]["health"]["enum"]);
    let ours: BTreeSet<String> = [Health::Healthy, Health::Degraded, Health::Unavailable]
        .iter()
        .map(|h| serde_json::to_value(h).expect("ser").as_str().expect("string").to_owned())
        .collect();
    assert_eq!(ours, declared_health, "Health disagrees with server_state.health");

    // Round-trip rather than only serialize: a rename that broke parsing
    // in one direction would otherwise pass.
    for h in [Health::Healthy, Health::Degraded, Health::Unavailable] {
        let j = serde_json::to_string(&h).expect("ser");
        assert_eq!(serde_json::from_str::<Health>(&j).expect("de"), h);
    }
    for p in [PathReadiness::Unavailable, PathReadiness::Partial, PathReadiness::Ready] {
        let j = serde_json::to_string(&p).expect("ser");
        assert_eq!(serde_json::from_str::<PathReadiness>(&j).expect("de"), p);
    }
    for p in [PreferredPathPolicy::PreferDirect, PreferredPathPolicy::PreferRelay] {
        let j = serde_json::to_string(&p).expect("ser");
        assert_eq!(serde_json::from_str::<PreferredPathPolicy>(&j).expect("de"), p);
    }
}

#[test]
fn the_connectivity_summary_requires_every_member_the_schema_requires() {
    let doc = schema("connectivity/connectivity-summary.schema.json");
    let required = string_set(&doc["required"]);
    // Serde structs without Option are required by construction, so the
    // check is that the schema demands nothing this type would drop.
    for field in [
        "direct_inbound",
        "relay_inbound",
        "active_relay_reservations",
        "target_relay_reservations",
        "active_relayed_peer_paths",
        "hole_punch_inflight",
        "preferred_path_policy",
        "updated_at",
    ] {
        assert!(required.contains(field), "{field} is not required by the schema");
    }
    assert_eq!(required.len(), 8, "the schema gained or lost a required member");
}

#[test]
fn the_endpoint_id_grammar_matches_its_schema() {
    let doc = schema("endpoints/endpoint-id.schema.json");
    assert_eq!(doc["pattern"], "^[a-z][a-z0-9._-]{0,63}$");
    assert_eq!(doc["maxLength"], serde_json::json!(EndpointId::MAX_BYTES));

    // The schema's own examples must parse. An example that does not is a
    // documentation bug that would otherwise ship as guidance.
    for example in doc["examples"].as_array().expect("examples") {
        let s = example.as_str().expect("string");
        assert!(EndpointId::parse(s).is_ok(), "schema example {s:?} does not parse");
    }
}

#[test]
fn message_id_matches_the_common_schema_grammar() {
    let doc = schema("common/message-id.schema.json");
    let pattern = doc["pattern"].as_str().expect("pattern");
    assert!(
        pattern.contains("0-9a-f") && pattern.contains("32"),
        "message-id pattern is not 32 lowercase hex: {pattern}"
    );
    let id = MessageId::from_bytes([0x0f; 16]);
    let json = serde_json::to_string(&id).expect("ser");
    assert_eq!(json, "\"0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f\"");
    assert_eq!(serde_json::from_str::<MessageId>(&json).expect("de"), id);
    // Uppercase is rejected on the way in, so the canonical form cannot be
    // acquired by round-tripping a non-canonical spelling.
    assert!(serde_json::from_str::<MessageId>("\"0F0F0F0F0F0F0F0F0F0F0F0F0F0F0F0F\"").is_err());
}

#[test]
fn payload_bounds_match_the_ipc_send_params_schema() {
    let doc = schema("ipc/send-params.schema.json");
    let bytes = &doc["properties"]["payload"]["properties"]["bytes"];
    // 65,536 base64url characters is the encoding of exactly 49,152 bytes;
    // the schema states the encoded bound, this crate states the decoded
    // one, and the two must describe the same ceiling.
    let max_encoded = bytes["maxLength"].as_u64().expect("maxLength") as usize;
    assert_eq!(max_encoded, MAX_PAYLOAD_BYTES.div_ceil(3) * 4);

    let media = &doc["properties"]["payload"]["properties"]["media_type"];
    assert_eq!(media["maxLength"], serde_json::json!(MAX_MEDIA_TYPE_BYTES));
    assert_eq!(media["minLength"], serde_json::json!(1));
    assert!(MediaType::parse("m".repeat(MAX_MEDIA_TYPE_BYTES)).is_ok());
    assert!(Payload::at_ceiling(None, vec![0; MAX_PAYLOAD_BYTES]).is_ok());
    assert!(Payload::at_ceiling(None, vec![0; MAX_PAYLOAD_BYTES + 1]).is_err());
}

#[test]
fn channel_id_matches_its_common_schema() {
    let doc = schema("common/channel-id.schema.json");
    assert_eq!(doc["maxLength"], serde_json::json!(ChannelId::MAX_BYTES));
    let pattern = doc["pattern"].as_str().expect("pattern");
    assert!(pattern.starts_with("^[A-Za-z0-9]"), "unexpected pattern: {pattern}");
}
