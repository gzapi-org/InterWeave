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

// This file's helpers read files and index JSON, and they sit OUTSIDE
// `#[test]` functions, which is where clippy.toml's allow-*-in-tests stops
// reaching. Panicking is the correct behaviour here: a missing or
// malformed schema is a broken checkout, and a helper that returned an
// error would only be unwrapped by every caller anyway.
#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use interweave_transport_api::{
    ChannelId, ConnectivitySummary, DirectInboundState, EndpointId, Health, MAX_MEDIA_TYPE_BYTES,
    MAX_PAYLOAD_BYTES, MediaType, MessageId, PathReadiness, Payload, PreferredPathPolicy,
    TransportError, TransportIdentity,
};

/// A syntactically valid identity for tests. Not a real key.
const TEST_PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

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
    let path = repo_root()
        .join("architecture/contracts/schemas")
        .join(relative);
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
    .map(|e| {
        serde_json::to_value(e)
            .expect("ser")
            .as_str()
            .expect("string")
            .to_owned()
    })
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
        .map(|h| {
            serde_json::to_value(h)
                .expect("ser")
                .as_str()
                .expect("string")
                .to_owned()
        })
        .collect();
    assert_eq!(
        ours, declared_health,
        "Health disagrees with server_state.health"
    );

    // Round-trip rather than only serialize: a rename that broke parsing
    // in one direction would otherwise pass.
    for h in [Health::Healthy, Health::Degraded, Health::Unavailable] {
        let j = serde_json::to_string(&h).expect("ser");
        assert_eq!(serde_json::from_str::<Health>(&j).expect("de"), h);
    }
    for p in [
        PathReadiness::Unavailable,
        PathReadiness::Partial,
        PathReadiness::Ready,
    ] {
        let j = serde_json::to_string(&p).expect("ser");
        assert_eq!(serde_json::from_str::<PathReadiness>(&j).expect("de"), p);
    }
    let j = serde_json::to_string(&PreferredPathPolicy::DirectFirst).expect("ser");
    assert_eq!(
        serde_json::from_str::<PreferredPathPolicy>(&j).expect("de"),
        PreferredPathPolicy::DirectFirst
    );
}

#[test]
fn the_connectivity_summary_agrees_field_by_field() {
    // NOT just `required`. An earlier version of this test checked only
    // which members were mandatory, and every one of the four value
    // constraints below was wrong in the Rust types while it passed:
    // direct_inbound had the relay enum, preferred_path_policy had two
    // invented variants, and the counters were u32 against a u16 bound.
    // Presence is the cheapest half of agreement and the least valuable.
    let doc = schema("connectivity/connectivity-summary.schema.json");
    let props = &doc["properties"];
    let required = string_set(&doc["required"]);
    assert_eq!(
        required.len(),
        8,
        "the schema gained or lost a required member"
    );

    let direct: BTreeSet<String> = string_set(&props["direct_inbound"]["enum"]);
    let ours: BTreeSet<String> = [
        DirectInboundState::Unknown,
        DirectInboundState::VerifiedPublic,
        DirectInboundState::NotVerified,
    ]
    .iter()
    .map(|v| {
        serde_json::to_value(v)
            .expect("ser")
            .as_str()
            .expect("str")
            .to_owned()
    })
    .collect();
    assert_eq!(
        ours, direct,
        "DirectInboundState disagrees with direct_inbound"
    );

    let relay = string_set(&props["relay_inbound"]["enum"]);
    let ours_relay: BTreeSet<String> = [
        PathReadiness::Unavailable,
        PathReadiness::Partial,
        PathReadiness::Ready,
    ]
    .iter()
    .map(|v| {
        serde_json::to_value(v)
            .expect("ser")
            .as_str()
            .expect("str")
            .to_owned()
    })
    .collect();
    assert_eq!(
        ours_relay, relay,
        "PathReadiness disagrees with relay_inbound"
    );

    // A `const`, not an enum: exactly one policy exists in standard v1.
    assert_eq!(
        props["preferred_path_policy"]["const"],
        serde_json::to_value(PreferredPathPolicy::DirectFirst).expect("ser")
    );

    // Counter width. u16::MAX is the schema maximum, and a type wider than
    // the bound would accept values that serialize and are then rejected.
    for counter in [
        "active_relay_reservations",
        "target_relay_reservations",
        "active_relayed_peer_paths",
        "hole_punch_inflight",
    ] {
        assert_eq!(
            props[counter]["maximum"],
            serde_json::json!(u16::MAX),
            "{counter} is not bounded at u16::MAX"
        );
        assert_eq!(props[counter]["minimum"], serde_json::json!(0));
    }

    // And the whole struct round-trips through the shape the schema
    // describes. The counters are written as EXPLICITLY TYPED u16 values,
    // not bare literals: a bare literal infers to whatever the field is,
    // so widening the field to u32 would still compile and this test would
    // still pass. With `u16::MAX` the widening becomes a type error, which
    // is the regression signal — an earlier version of this test used
    // literals and did not catch exactly that.
    let saturated: u16 = u16::MAX;
    let summary = ConnectivitySummary {
        direct_inbound: DirectInboundState::VerifiedPublic,
        relay_inbound: PathReadiness::Ready,
        active_relay_reservations: saturated,
        target_relay_reservations: saturated,
        active_relayed_peer_paths: saturated,
        hole_punch_inflight: saturated,
        preferred_path_policy: PreferredPathPolicy::DirectFirst,
        updated_at: 1_700_000_000_000,
    };
    // Every counter at its type maximum must still be within the schema's.
    for counter in [
        "active_relay_reservations",
        "target_relay_reservations",
        "active_relayed_peer_paths",
        "hole_punch_inflight",
    ] {
        let emitted = serde_json::to_value(&summary).expect("ser")[counter]
            .as_u64()
            .expect("integer");
        let allowed = props[counter]["maximum"].as_u64().expect("maximum");
        assert!(
            emitted <= allowed,
            "{counter} at its type maximum ({emitted}) exceeds the schema maximum ({allowed})"
        );
    }
    let json = serde_json::to_value(&summary).expect("ser");
    for field in required {
        assert!(
            json.get(&field).is_some(),
            "serialized summary omits {field}"
        );
    }
    assert_eq!(
        serde_json::from_value::<ConnectivitySummary>(json).expect("de"),
        summary
    );
}

#[test]
fn the_transport_identity_grammar_matches_the_peer_id_schema() {
    let doc = schema("common/peer-id.schema.json");
    let pattern = doc["pattern"].as_str().expect("pattern");
    assert!(
        pattern.contains("12D3KooW") && pattern.contains("Qm"),
        "{pattern}"
    );

    assert!(TransportIdentity::parse(TEST_PEER).is_ok());
    // The values that used to pass the neutral boundary and fail later in
    // a backend parser.
    for bad in ["garbage", "12D3KooW", "", "not-a-peer-id"] {
        assert!(
            TransportIdentity::parse(bad).is_err(),
            "{bad:?} should not parse"
        );
    }
}

#[test]
fn a_payload_serializes_as_the_schemas_describe_it() {
    // base64url string, not an array of integers: a derived impl emitted
    // the latter, which no schema in this repository accepts.
    let p = Payload::at_ceiling(
        Some(MediaType::parse("text/plain").expect("valid")),
        b"hello".to_vec(),
    )
    .expect("valid");
    let json = serde_json::to_value(&p).expect("ser");
    assert_eq!(json["bytes"], serde_json::json!("aGVsbG8"));
    assert_eq!(json["media_type"], serde_json::json!("text/plain"));
    assert_eq!(serde_json::from_value::<Payload>(json).expect("de"), p);

    // An absent media type is absent from the JSON, not present-and-empty.
    let bare = Payload::at_ceiling(None, Vec::new()).expect("valid");
    let json = serde_json::to_value(&bare).expect("ser");
    assert!(json.get("media_type").is_none());
    assert_eq!(json["bytes"], serde_json::json!(""));

    // The pattern the schema asserts must accept what we emit.
    let doc = schema("ipc/send-params.schema.json");
    let pattern = doc["properties"]["payload"]["properties"]["bytes"]["pattern"]
        .as_str()
        .expect("pattern");
    assert!(
        pattern.contains("A-Za-z0-9_-"),
        "unexpected alphabet: {pattern}"
    );
}

#[test]
fn deserialization_cannot_bypass_the_payload_ceiling() {
    // The bound has to hold on the path untrusted input actually takes.
    let over = "A".repeat(MAX_PAYLOAD_BYTES.div_ceil(3) * 4 + 4);
    let json = serde_json::json!({ "bytes": over });
    assert!(
        serde_json::from_value::<Payload>(json).is_err(),
        "an over-ceiling payload deserialized"
    );

    // Exactly at the ceiling still works.
    let at = crate_encode(&vec![0u8; MAX_PAYLOAD_BYTES]);
    let json = serde_json::json!({ "bytes": at });
    let p = serde_json::from_value::<Payload>(json).expect("at the ceiling");
    assert_eq!(p.len(), MAX_PAYLOAD_BYTES);

    // A non-canonical encoding is refused rather than normalized.
    assert!(serde_json::from_value::<Payload>(serde_json::json!({ "bytes": "AA==" })).is_err());
    assert!(serde_json::from_value::<Payload>(serde_json::json!({ "bytes": "A" })).is_err());
}

fn crate_encode(bytes: &[u8]) -> String {
    interweave_transport_api::base64url::encode(bytes)
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
        assert!(
            EndpointId::parse(s).is_ok(),
            "schema example {s:?} does not parse"
        );
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
    assert!(
        pattern.starts_with("^[A-Za-z0-9]"),
        "unexpected pattern: {pattern}"
    );
}
