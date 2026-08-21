// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Stage 1 exit gate, checked with a real JSON Schema validator.
//!
//! Each contract crate carries a suite comparing **definitions** — that
//! the Rust enum members are the schema's enum members, that a constant
//! equals a `maxLength`. This suite asks the other half of the question,
//! about **instances**:
//!
//! - a value serialized from a Rust type validates against its schema;
//! - a schema-valid instance deserializes into that type.
//!
//! Both directions are needed and neither implies the other. A type can
//! emit conforming JSON while refusing to parse a legal document (an
//! over-strict `deny_unknown_fields`, a missing default), and a type can
//! parse everything while emitting a shape no schema accepts — which is
//! precisely the defect the PR #11 review found in `Payload`, where a
//! derived impl emitted an array of integers.
//!
//! A real validator is used rather than hand-written assertions because
//! hand-written ones check what the author remembered to check. This is a
//! test-only package, so it may take the dependency that a production
//! crate must not.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use interweave_local_client_api::{DataCapability, EndpointLease, Generation, LocalDataSession};
use interweave_transport_api::{
    ConnectivitySummary, DirectInboundState, EndpointId, MediaType, MessageId, PathReadiness,
    Payload, PreferredPathPolicy, TransportError,
};

const TEST_PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("tests/<pkg> is two levels below the root")
        .to_path_buf()
}

fn schema_doc(relative: &str) -> serde_json::Value {
    let path = repo_root()
        .join("architecture/contracts/schemas")
        .join(relative);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

/// Resolve the `$ref`s this repository uses by loading every schema and
/// registering it under its URN.
///
/// The schemas cross-reference by `urn:interweave:...`, which no resolver
/// knows how to fetch. Registering them explicitly is what lets a real
/// validator run at all.
fn validator_for(relative: &str) -> jsonschema::Validator {
    // Loaded once and reused: registry construction reads every schema in
    // the tree, and doing that per test would dominate the runtime.
    static REGISTRY: std::sync::OnceLock<jsonschema::Registry<'_>> = std::sync::OnceLock::new();
    let registry = REGISTRY.get_or_init(|| {
        let base = repo_root().join("architecture/contracts/schemas");
        let pairs: Vec<(String, jsonschema::Resource)> = walk_schemas(&base)
            .into_iter()
            .filter_map(|path| {
                let text = std::fs::read_to_string(&path).ok()?;
                let doc: serde_json::Value = serde_json::from_str(&text).ok()?;
                let id = doc.get("$id")?.as_str()?.to_owned();
                Some((id, jsonschema::Resource::from_contents(doc)))
            })
            .collect();
        assert!(!pairs.is_empty(), "no schemas found to register");
        jsonschema::Registry::new()
            .extend(pairs)
            .expect("schemas register")
            .prepare()
            .expect("registry prepares")
    });

    jsonschema::options()
        .with_registry(registry)
        .build(&schema_doc(relative))
        .expect("schema compiles")
}

fn walk_schemas(dir: &PathBuf) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk_schemas(&p));
        } else if p.extension().is_some_and(|x| x == "json")
            && p.file_name().is_some_and(|n| n != "manifest.json")
        {
            out.push(p);
        }
    }
    out
}

/// Assert an instance validates, printing every error when it does not.
fn assert_valid(validator: &jsonschema::Validator, instance: &serde_json::Value, what: &str) {
    if validator.is_valid(instance) {
        return;
    }
    let errors: Vec<String> = validator
        .iter_errors(instance)
        .map(|e| e.to_string())
        .collect();
    panic!(
        "{what} does not validate against its schema:\n  {}\n  instance: {}",
        errors.join("\n  "),
        serde_json::to_string_pretty(instance).unwrap_or_default()
    );
}

#[test]
fn a_connectivity_summary_validates_and_round_trips() {
    let validator = validator_for("connectivity/connectivity-summary.schema.json");
    let summary = ConnectivitySummary {
        direct_inbound: DirectInboundState::VerifiedPublic,
        relay_inbound: PathReadiness::Ready,
        active_relay_reservations: 2,
        target_relay_reservations: 3,
        active_relayed_peer_paths: 1,
        hole_punch_inflight: 0,
        preferred_path_policy: PreferredPathPolicy::DirectFirst,
        updated_at: 1_700_000_000_000,
    };
    let json = serde_json::to_value(&summary).expect("ser");
    assert_valid(&validator, &json, "ConnectivitySummary");
    assert_eq!(
        serde_json::from_value::<ConnectivitySummary>(json).expect("de"),
        summary
    );

    // Every state combination, since a single happy value would not
    // exercise the enums the review found wrong.
    for direct in [
        DirectInboundState::Unknown,
        DirectInboundState::VerifiedPublic,
        DirectInboundState::NotVerified,
    ] {
        for relay in [
            PathReadiness::Unavailable,
            PathReadiness::Partial,
            PathReadiness::Ready,
        ] {
            let mut s = summary.clone();
            s.direct_inbound = direct;
            s.relay_inbound = relay;
            let json = serde_json::to_value(&s).expect("ser");
            assert_valid(&validator, &json, &format!("{direct:?}/{relay:?}"));
        }
    }
}

#[test]
fn a_payload_validates_inside_ipc_send_params() {
    // The direction the PR #11 review caught: a derived impl emitted an
    // array of integers here, which the schema rejects.
    let validator = validator_for("ipc/send-params.schema.json");
    for (name, payload) in [
        (
            "with media type",
            Payload::at_ceiling(
                Some(MediaType::parse("text/plain").expect("valid")),
                b"hello".to_vec(),
            )
            .expect("valid"),
        ),
        (
            "absent media type",
            Payload::at_ceiling(None, b"hello".to_vec()).expect("valid"),
        ),
        (
            "empty payload",
            Payload::at_ceiling(None, Vec::new()).expect("valid"),
        ),
        (
            "non-utf8 bytes",
            Payload::at_ceiling(None, vec![0xff, 0xfe, 0x00]).expect("valid"),
        ),
    ] {
        let instance = serde_json::json!({
            "peer": TEST_PEER,
            "endpoint": "human",
            "payload": serde_json::to_value(&payload).expect("ser"),
            "message_id": MessageId::from_bytes([0x0a; 16]).to_hex(),
        });
        assert_valid(&validator, &instance, name);

        // And back: the schema-valid instance parses into the type.
        let parsed: Payload =
            serde_json::from_value(instance["payload"].clone()).expect("payload de");
        assert_eq!(parsed, payload, "{name} did not round-trip");
    }
}

#[test]
fn a_direct_message_received_event_validates() {
    let validator = validator_for("endpoints/message-received.schema.json");
    let payload = Payload::at_ceiling(
        Some(MediaType::parse("application/vnd.interweave-human-chat+json;v=2").expect("valid")),
        b"{}".to_vec(),
    )
    .expect("valid");
    let instance = serde_json::json!({
        "message_id": MessageId::from_bytes([0x01; 16]).to_hex(),
        "mode": "direct",
        "source_peer": TEST_PEER,
        "source_endpoint": "claude",
        "destination_endpoint": "human",
        "payload": serde_json::to_value(&payload).expect("ser"),
        "received_at": 1_700_000_000_000_u64,
    });
    assert_valid(&validator, &instance, "MessageReceived");
}

#[test]
fn every_error_code_validates_individually() {
    // The whole vocabulary, one instance each: a set comparison proves the
    // members match, and this proves each one is actually emitted in the
    // spelling the schema accepts.
    let validator = validator_for("ipc/error-code.schema.json");
    for e in [
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
    ] {
        let json = serde_json::to_value(e).expect("ser");
        assert_valid(&validator, &json, &format!("{e:?}"));
    }
}

#[test]
fn every_direct_reject_reason_validates() {
    let validator = validator_for("direct/reject-reason.schema.json");
    // Driven through the mapping rather than listed, so a local category
    // that mapped to something the wire does not define would fail here.
    for e in [
        TransportError::EndpointUnknown,
        TransportError::UnauthorizedPeer,
        TransportError::PayloadTooLarge,
        TransportError::Overloaded,
        TransportError::ShuttingDown,
        TransportError::ProtocolUnsupported,
        TransportError::ProtocolViolation,
        TransportError::Internal,
    ] {
        let json = serde_json::to_value(e.to_wire()).expect("ser");
        assert_valid(&validator, &json, &format!("{e:?} -> wire"));
    }
}

#[test]
fn identifiers_validate_against_their_common_schemas() {
    let peer = validator_for("common/peer-id.schema.json");
    assert_valid(&peer, &serde_json::json!(TEST_PEER), "peer id");

    let message = validator_for("common/message-id.schema.json");
    let id = MessageId::from_bytes([0xfe; 16]);
    assert_valid(&message, &serde_json::json!(id.to_hex()), "message id");

    let endpoint = validator_for("endpoints/endpoint-id.schema.json");
    for name in ["human", "claude", "automation.build", "a"] {
        let parsed = EndpointId::parse(name).expect("valid");
        assert_valid(
            &endpoint,
            &serde_json::to_value(&parsed).expect("ser"),
            name,
        );
    }

    let channel = validator_for("common/channel-id.schema.json");
    for name in ["general", "General", "team.eu:builds/nightly-1"] {
        let parsed = interweave_transport_api::ChannelId::parse(name).expect("valid");
        assert_valid(&channel, &serde_json::to_value(&parsed).expect("ser"), name);
    }
}

#[test]
fn a_hello_frame_validates_and_a_schema_valid_hello_parses() {
    use interweave_ipc_protocol::{ClientInfo, Hello, HelloTag, IpcVersion, RequestedCapability};

    let validator = validator_for("ipc/hello.schema.json");
    let hello = Hello {
        frame_type: HelloTag::Hello,
        ipc_version: IpcVersion { major: 2, minor: 0 },
        client: ClientInfo {
            kind: "human-client".to_owned(),
            version: Some("0.1".to_owned()),
        },
        endpoint: Some(interweave_ipc_protocol::EndpointClaim {
            id: EndpointId::parse("human").expect("valid"),
        }),
        requested_capabilities: [RequestedCapability::Events, RequestedCapability::Commands]
            .into_iter()
            .collect(),
        features: ["keepalive".to_owned()].into_iter().collect(),
    };
    let json = serde_json::to_value(&hello).expect("ser");
    assert_valid(&validator, &json, "Hello");
    assert_eq!(serde_json::from_value::<Hello>(json).expect("de"), hello);

    // The other direction, from a hand-written schema-valid document.
    let minimal = serde_json::json!({
        "type": "hello",
        "ipc_version": { "major": 2, "minor": 0 },
        "client": { "kind": "diagnostics" }
    });
    assert_valid(&validator, &minimal, "minimal hello");
    let parsed: Hello = serde_json::from_value(minimal).expect("minimal de");
    assert!(parsed.endpoint.is_none());
}

#[test]
fn an_endpoint_trust_policy_validates_in_both_shapes() {
    use interweave_trust_api::EndpointTrustPolicy;

    // The shape the PR #11 review corrected, and the reason the config
    // fixture had to be corrected too — checked here against the schema
    // itself rather than against either of them.
    let validator = validator_for("endpoints/endpoint-config.schema.json");
    let peer = interweave_transport_api::TransportIdentity::parse(TEST_PEER).expect("valid");

    for (name, policy) in [
        ("inherit", EndpointTrustPolicy::InheritProfileTrust),
        (
            "subset",
            EndpointTrustPolicy::StaticSubset {
                allowed_peers: [peer].into_iter().collect(),
            },
        ),
    ] {
        let instance = serde_json::json!({
            "id": "human",
            "enabled": true,
            "advertise": false,
            "inbound": serde_json::to_value(&policy).expect("ser"),
            "outbound": serde_json::to_value(&policy).expect("ser"),
        });
        assert_valid(&validator, &instance, name);
    }
}

#[test]
fn a_discovery_candidate_validates() {
    use interweave_discovery_api::{CandidatePeer, ProtocolId, ProtocolObservation};

    let validator = validator_for("discovery/candidate-peer.schema.json");
    let candidate = CandidatePeer {
        peer_id: interweave_transport_api::TransportIdentity::parse(TEST_PEER).expect("valid"),
        addresses: ["/ip4/192.0.2.1/tcp/4001".to_owned()].into_iter().collect(),
        source: "mdns".to_owned(),
        observed_at: 1_700_000_000_000,
        expires_at: Some(1_700_000_060_000),
        protocol_observations: [ProtocolObservation {
            protocol_id: ProtocolId::parse("/interweave/direct/2.0.0").expect("valid"),
            supported: true,
            observed_at: 1_700_000_000_000,
        }]
        .into_iter()
        .collect(),
    };
    assert!(candidate.validate().is_ok());
    let json = serde_json::to_value(&candidate).expect("ser");
    assert_valid(&validator, &json, "CandidatePeer");
    assert_eq!(
        serde_json::from_value::<CandidatePeer>(json).expect("de"),
        candidate
    );
}

#[test]
fn a_data_session_never_carries_an_admin_capability() {
    // Cross-crate, and the reason this suite exists rather than only the
    // per-crate ones: the IPC capability schema is a single closed
    // vocabulary containing admin.*, while a data session's granted set
    // must never contain one. Both facts are true at once, and only a
    // test that sees both crates can say so.
    let validator = validator_for("ipc/capability.schema.json");
    let session = LocalDataSession::new(
        Generation::parse("session_generation").expect("valid"),
        "human-client",
        Some(EndpointLease {
            endpoint: EndpointId::parse("human").expect("valid"),
            epoch: Generation::parse("lease_generation_1").expect("valid"),
        }),
        [DataCapability::Events, DataCapability::Commands],
        256,
    )
    .expect("valid session");

    for c in session.capabilities() {
        let json = serde_json::to_value(c).expect("ser");
        // Each granted capability is a legal member of the vocabulary...
        assert_valid(&validator, &json, "granted capability");
        // ...and is never one of the administrative ones.
        let s = json.as_str().expect("string");
        assert!(
            !s.starts_with("admin."),
            "a data session granted {s}, which is administrative"
        );
    }
}
