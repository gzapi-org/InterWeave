// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! These types agree with the frozen IPC schemas and fixtures.

// Helpers read files and index JSON outside `#[test]` functions, where
// clippy.toml's allow-*-in-tests does not reach.
#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use interweave_ipc_protocol::{
    AuthorityDomain, ClientInfo, EventType, Hello, HelloTag, IPC_MAJOR, IpcVersion, MAX_BODY_BYTES,
    MAX_REQUESTED, Method, RequestedCapability, encode_frame,
};
use interweave_transport_api::TransportError;

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

/// The serialized name of every variant `T`'s serde derive knows, read
/// from the derive itself: `Deserialize` hands its full variant list to
/// `deserialize_enum`, and this deserializer keeps it and fails. A list
/// typed into a test escapes a new variant (#143's supply review); this
/// one cannot, because the compiler writes it.
fn variants<T: serde::de::DeserializeOwned>() -> BTreeSet<String> {
    use serde::de::{Error as _, Visitor};

    struct Probe<'a>(&'a mut Option<&'static [&'static str]>);

    impl<'de> serde::Deserializer<'de> for Probe<'_> {
        type Error = serde::de::value::Error;

        fn deserialize_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Self::Error> {
            Err(Self::Error::custom("not an enum"))
        }

        fn deserialize_enum<V: Visitor<'de>>(
            self,
            _: &'static str,
            variants: &'static [&'static str],
            _: V,
        ) -> Result<V::Value, Self::Error> {
            *self.0 = Some(variants);
            Err(Self::Error::custom("probed"))
        }

        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
            bytes byte_buf option unit unit_struct newtype_struct seq tuple
            tuple_struct map struct identifier ignored_any
        }
    }

    let mut seen = None;
    let _ = T::deserialize(Probe(&mut seen));
    seen.expect("T derives Deserialize for an enum")
        .iter()
        .map(|&name| name.to_owned())
        .collect()
}

fn names<T: serde::Serialize>(items: impl IntoIterator<Item = T>) -> BTreeSet<String> {
    items
        .into_iter()
        .map(|item| {
            serde_json::to_value(item)
                .expect("ser")
                .as_str()
                .expect("a unit variant serializes as a string")
                .to_owned()
        })
        .collect()
}

#[test]
fn the_requested_capability_vocabulary_matches_the_schema() {
    // `hello.requested_capabilities` references the FULL capability enum,
    // admin included: a client may ask for anything, and refusing is the
    // server's job. That is why this type is separate from the granted
    // DataCapability set rather than an alias for it.
    let declared = string_set(&schema("ipc/capability.schema.json")["enum"]);
    assert_eq!(variants::<RequestedCapability>(), declared);
}

#[test]
fn the_error_vocabulary_matches_the_schema() {
    let declared = string_set(&schema("ipc/error-code.schema.json")["enum"]);
    assert_eq!(variants::<TransportError>(), declared);
}

#[test]
fn the_method_catalogue_matches_the_schema_both_ways() {
    let declared = string_set(&schema("ipc/method.schema.json")["enum"]);
    assert_eq!(variants::<Method>(), declared, "the enum and the schema");
    assert_eq!(names(Method::ALL), declared, "Method::ALL is every variant");
    for method in Method::ALL {
        assert_eq!(
            names([method]),
            BTreeSet::from([method.as_str().to_owned()])
        );
    }
    // `ipc/request` binds exactly the same names, one alternative each.
    let bound: Vec<String> = schema("ipc/request.schema.json")["oneOf"]
        .as_array()
        .expect("oneOf")
        .iter()
        .map(|alt| {
            alt["properties"]["method"]["const"]
                .as_str()
                .expect("const")
                .to_owned()
        })
        .collect();
    assert_eq!(bound.len(), declared.len(), "one alternative per method");
    assert_eq!(bound.into_iter().collect::<BTreeSet<_>>(), declared);
    // And the frame's request class names the catalogue, not a copy of it.
    assert_eq!(
        schema("ipc/frame.schema.json")["$defs"]["request"]["properties"]["method"]["$ref"],
        serde_json::json!("urn:interweave:schemas:ipc:method")
    );
}

#[test]
fn the_event_catalogue_matches_the_schema_both_ways() {
    let bound: Vec<String> = schema("ipc/event.schema.json")["oneOf"]
        .as_array()
        .expect("oneOf")
        .iter()
        .map(|alt| {
            alt["properties"]["event_type"]["const"]
                .as_str()
                .expect("const")
                .to_owned()
        })
        .collect();
    let declared: BTreeSet<String> = bound.iter().cloned().collect();
    assert_eq!(bound.len(), declared.len(), "one alternative per type");
    assert_eq!(variants::<EventType>(), declared, "the enum and the schema");
    assert_eq!(
        names(EventType::ALL),
        declared,
        "EventType::ALL is every variant"
    );
    for kind in EventType::ALL {
        assert_eq!(EventType::parse(kind.as_str()), Some(kind));
    }
    // The envelope's own list of event types is the same set.
    assert_eq!(
        string_set(
            &schema("ipc/frame.schema.json")["$defs"]["event"]["properties"]["event_type"]["enum"]
        ),
        declared
    );
}

/// The prose table's rows under `heading`, each split into its cells.
fn table_rows(heading: &str) -> Vec<Vec<String>> {
    let text = std::fs::read_to_string(root().join("architecture/contracts/LOCAL-IPC.md"))
        .expect("LOCAL-IPC.md");
    let section = text
        .split(&format!("\n## {heading}\n"))
        .nth(1)
        .unwrap_or_else(|| panic!("no section {heading}"));
    let section = section.split("\n## ").next().expect("section body");
    section
        .lines()
        .filter(|line| line.starts_with("| `"))
        .map(|line| {
            line.trim_matches('|')
                .split('|')
                .map(|cell| cell.trim().to_owned())
                .collect()
        })
        .collect()
}

/// The first `code` span of a cell, backticks removed.
fn first_code(cell: &str) -> &str {
    cell.split('`').nth(1).unwrap_or(cell)
}

#[test]
fn the_method_table_in_the_prose_is_the_rust_table() {
    let rows = table_rows("Method catalogue");
    assert_eq!(rows.len(), Method::ALL.len(), "one prose row per method");
    for row in rows {
        let method = Method::parse(first_code(&row[0])).unwrap_or_else(|| panic!("{row:?}"));
        let entry = method.entry();
        let domain = match entry.domain {
            AuthorityDomain::Data => "data",
            AuthorityDomain::Admin => "admin",
        };
        assert_eq!(row[1], domain, "{} domain", method.as_str());
        assert_eq!(
            first_code(&row[2]),
            names([entry.capability]).pop_first().expect("one"),
            "{} capability",
            method.as_str()
        );
        assert_eq!(
            row[5],
            format!("2.{}", entry.since_minor),
            "{} since",
            method.as_str()
        );
    }
}

#[test]
fn the_prose_params_column_is_the_request_schemas_binding() {
    let doc = schema("ipc/request.schema.json");
    let alternatives = doc["oneOf"].as_array().expect("oneOf");
    for row in table_rows("Method catalogue") {
        let method = first_code(&row[0]);
        let alt = alternatives
            .iter()
            .find(|alt| alt["properties"]["method"]["const"] == serde_json::json!(method))
            .unwrap_or_else(|| panic!("{method} has no alternative"));
        let bound = alt["properties"]["params"]["$ref"]
            .as_str()
            .expect("params $ref");
        let prose = first_code(&row[3]);
        let expected = if prose == "none" {
            "empty-result"
        } else {
            prose
        };
        assert_eq!(
            bound,
            format!("urn:interweave:schemas:ipc:{expected}"),
            "{method}"
        );
    }
}

#[test]
fn the_prose_event_table_is_the_event_schemas_binding() {
    let rows = table_rows("Event catalogue");
    assert_eq!(rows.len(), EventType::ALL.len(), "one prose row per type");
    let doc = schema("ipc/event.schema.json");
    for row in rows {
        let kind = EventType::parse(first_code(&row[0])).unwrap_or_else(|| panic!("{row:?}"));
        assert_eq!(
            row[3],
            format!("2.{}", kind.since_minor()),
            "{} since",
            kind.as_str()
        );
        let alt = doc["oneOf"]
            .as_array()
            .expect("oneOf")
            .iter()
            .find(|alt| {
                alt["properties"]["event_type"]["const"] == serde_json::json!(kind.as_str())
            })
            .expect("an alternative");
        let data = &alt["properties"]["data"];
        match data["$ref"].as_str() {
            Some(reference) => assert_eq!(
                reference,
                format!("urn:interweave:schemas:{}", first_code(&row[1])),
                "{}",
                kind.as_str()
            ),
            // `peer.disconnected` is the one inline body.
            None => assert_eq!(
                string_set(&data["required"]),
                BTreeSet::from(["peer".to_owned(), "reason_class".to_owned()])
            ),
        }
    }
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
    // Any positive major is a well-formed hello (hello 1.1.0): an
    // unsupported one is answered, not rejected as malformed. Which
    // major the server speaks is IPC_MAJOR; `negotiate` answers the rest.
    assert_eq!(
        doc["properties"]["ipc_version"]["properties"]["major"]["minimum"],
        serde_json::json!(1)
    );
    assert_eq!(IPC_MAJOR, 2, "the server speaks IPC v2");
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
        let body =
            usize::try_from(v["body_bytes"].as_u64().expect("body_bytes")).expect("fits usize");
        let headroom = usize::try_from(
            v["envelope_headroom_bytes"]
                .as_u64()
                .expect("envelope_headroom_bytes"),
        )
        .expect("fits usize");
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
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{b:02x}"));
        s
    })
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
