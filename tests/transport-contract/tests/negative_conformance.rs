// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Differential negative conformance: what the schema REJECTS, Rust must
//! reject too.
//!
//! # The gap this closes
//!
//! `schema_round_trip.rs` proves a Rust value serializes to something the
//! schema accepts, and that a schema-valid document deserializes. Both
//! are positive. Neither can catch a boundary that is MORE PERMISSIVE
//! than its contract — and that is the only direction in which a peer can
//! send something a conforming validator would refuse and this
//! implementation will act on.
//!
//! Three such defects reached `main` and passed both positive suites: a
//! framed `null` decoding as a valid frame, nine duplicate capabilities
//! collapsing inside a `BTreeSet` before the cardinality check, and
//! feature names outside their length bounds.
//!
//! # How it works
//!
//! For every boundary: take a valid seed, generate one mutation per
//! constraint the schema declares, then ask two independent questions
//! about each candidate — what does a real JSON Schema validator say, and
//! what does the Rust parser say. The generator does not decide validity;
//! it only proposes. That is what lets it work against `oneOf` roots and
//! not care whether a mutation was rescued by another branch.
//!
//! Two verdicts, and they are not symmetric:
//!
//! - **schema invalid, Rust accepts** — a defect, always. The boundary
//!   admits what the contract forbids.
//! - **schema valid, Rust rejects** — being stricter than the contract.
//!   Sometimes correct, never accidental, so each case must be listed in
//!   [`STRICTER_THAN_SCHEMA`] with a reason.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use interweave_discovery_api::CandidatePeer;
use interweave_human_chat_protocol::HumanChatV2;
use interweave_ipc_protocol::{Hello, decode_frame, encode_frame};
use interweave_transport_api::DirectDestination;
use interweave_transport_contract_tests::{Candidate, SchemaIndex, index_from, mutations};
use serde_json::{Value, json};

const PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("tests/<pkg> is two levels below the root")
        .to_path_buf()
}

fn schema_dir() -> PathBuf {
    repo_root().join("architecture/contracts/schemas")
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

fn all_schema_docs() -> Vec<Value> {
    walk_schemas(&schema_dir())
        .into_iter()
        .filter_map(|p| {
            let text = std::fs::read_to_string(&p).ok()?;
            serde_json::from_str(&text).ok()
        })
        .collect()
}

fn schema_doc(relative: &str) -> Value {
    let path = schema_dir().join(relative);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

fn validator_for(relative: &str) -> jsonschema::Validator {
    static REGISTRY: std::sync::OnceLock<jsonschema::Registry<'_>> = std::sync::OnceLock::new();
    let registry = REGISTRY.get_or_init(|| {
        let pairs: Vec<(String, jsonschema::Resource)> = all_schema_docs()
            .into_iter()
            .filter_map(|doc| {
                let id = doc.get("$id").and_then(Value::as_str)?.to_owned();
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

fn index() -> SchemaIndex {
    index_from(all_schema_docs())
}

/// Whether the Rust boundary admits this document.
type Parse = fn(&Value) -> bool;

/// Which candidates a boundary is answerable for.
type Responsible = fn(&Candidate) -> bool;

struct Boundary {
    name: &'static str,
    schema: &'static str,
    seed: Value,
    parse: Parse,
    /// What this boundary PROMISES to enforce.
    ///
    /// Not every schema has one implementation that owns all of it.
    /// `decode_frame` is the framing layer: it promises a length-prefixed
    /// UTF-8 JSON **object**, and says nothing about whether that object
    /// is a well-formed `request` — the dispatcher that would decide that
    /// does not exist yet. Holding it to the whole frame schema would
    /// manufacture forty failures for one real defect and bury it.
    ///
    /// Constraints outside this predicate are reported as UNENFORCED
    /// rather than passed over, so a missing layer stays visible instead
    /// of reading as coverage.
    responsible: Responsible,
}

fn everything(_: &Candidate) -> bool {
    true
}

/// Only the root's JSON type — what a framing layer can answer.
fn only_root_type(c: &Candidate) -> bool {
    c.pointer == "<root>" && c.label.starts_with("root is a JSON")
}

// -------------------------------------------------------------------
// The Rust side of each boundary
// -------------------------------------------------------------------

/// The IPC frame boundary: does `decode_frame` report a valid frame?
fn parse_frame(doc: &Value) -> bool {
    let text = serde_json::to_string(doc).expect("serializes");
    let Ok(framed) = encode_frame(&text) else {
        return false;
    };
    decode_frame(&framed).is_ok()
}

/// The IPC hello boundary: deserialization plus the malformedness checks
/// in `evaluate`.
///
/// `CapabilityDenied` is deliberately NOT a rejection here — it is an
/// authorization outcome on a well-formed frame, and counting it would
/// make this suite assert policy instead of conformance.
fn parse_hello(doc: &Value) -> bool {
    use interweave_ipc_protocol::AuthorityDomain;
    use interweave_transport_api::TransportError;

    let Ok(hello) = serde_json::from_value::<Hello>(doc.clone()) else {
        return false;
    };
    !matches!(
        hello.evaluate(AuthorityDomain::Data, false),
        Err(TransportError::InvalidArgument | TransportError::VersionIncompatible)
    )
}

fn parse_human_chat(doc: &Value) -> bool {
    HumanChatV2::parse(&serde_json::to_string(doc).expect("serializes")).is_ok()
}

fn parse_candidate_peer(doc: &Value) -> bool {
    serde_json::from_value::<CandidatePeer>(doc.clone()).is_ok_and(|c| c.validate().is_ok())
}

fn parse_direct_destination(doc: &Value) -> bool {
    serde_json::from_value::<DirectDestination>(doc.clone()).is_ok()
}

fn boundaries() -> Vec<Boundary> {
    vec![
        Boundary {
            name: "ipc.frame",
            schema: "ipc/frame.schema.json",
            seed: json!({"type": "ping", "nonce": "0123456789abcdef"}),
            parse: parse_frame,
            responsible: only_root_type,
        },
        Boundary {
            name: "ipc.hello",
            schema: "ipc/hello.schema.json",
            seed: json!({
                "type": "hello",
                "ipc_version": {"major": 2, "minor": 0},
                "client": {"kind": "human-client", "version": "1.0.0"},
                "requested_capabilities": ["events"],
                "features": ["keepalive"]
            }),
            parse: parse_hello,
            responsible: everything,
        },
        Boundary {
            name: "human-chat.envelope",
            schema: "human-chat/envelope.schema.json",
            seed: json!({
                "v": 2,
                "kind": "text",
                "app_message_id": "0123456789abcdef0123456789abcdef",
                "text": "hello"
            }),
            parse: parse_human_chat,
            responsible: everything,
        },
        Boundary {
            name: "discovery.candidate-peer",
            schema: "discovery/candidate-peer.schema.json",
            seed: json!({
                "peer_id": PEER,
                "addresses": ["/ip4/10.0.0.1/tcp/4001"],
                "source": "peer-cache",
                "observed_at": 1_700_000_000_000_u64
            }),
            parse: parse_candidate_peer,
            responsible: everything,
        },
        Boundary {
            name: "endpoints.direct-destination",
            schema: "endpoints/direct-destination.schema.json",
            seed: json!({"peer": PEER, "endpoint": "human"}),
            parse: parse_direct_destination,
            responsible: everything,
        },
    ]
}

/// Cases where Rust is deliberately stricter than the schema.
///
/// Each entry is `(boundary, label-fragment, reason)`. Being stricter is
/// sometimes right — a canonical PeerId check is stronger than a
/// `pattern` can express — but it must never be accidental, because a
/// boundary that refuses a document conforming clients will legitimately
/// send is an interoperability failure that only shows up in the field.
const STRICTER_THAN_SCHEMA: &[(&str, &str, &str)] = &[];

/// Defects this harness found on first run, still outstanding.
///
/// # This list may only shrink
///
/// Every entry is asserted to STILL disagree. Fixing one without
/// deleting its line fails the suite, so the inventory cannot rot into a
/// list of things that were true once. A new disagreement not on the
/// list fails immediately — the list is an inventory of known debt, never
/// a way to admit more.
///
/// Each entry is `(boundary, exact label, what is wrong)`.
const KNOWN_DISAGREEMENTS: &[(&str, &str, &str)] = &[
    // Both arrays deserialize straight into `BTreeSet`, so duplicates are
    // gone before `evaluate` counts them: nine repeats become one member.
    // `evaluate` checks the client kind's length and the collection
    // sizes, and no individual string beyond that.
    (
        "ipc.hello",
        "/client/version is one over maxLength 128",
        "version length unvalidated",
    ),
    // A missing property is absence; `null` is a value, and no schema
    // here includes it in any type.
    (
        "ipc.hello",
        "/client/version is explicit null rather than absent",
        "null read as absence",
    ),
    (
        "ipc.hello",
        "/endpoint is explicit null rather than absent",
        "null read as absence",
    ),
    // Found by this harness, not previously reported. The schema's own
    // description says `additionalProperties: false` is what stops an
    // EndpointId or presence field being added to discovery "as an
    // obvious convenience" — and the Rust type does not enforce it.
    (
        "discovery.candidate-peer",
        "<root> is closed but carries an unknown property",
        "no deny_unknown_fields on the type the schema relies on being closed",
    ),
    // The same BTreeSet defect as hello, in a different crate: 65
    // duplicate addresses collapse to one before `validate` counts them.
    (
        "discovery.candidate-peer",
        "/addresses repeats an item",
        "BTreeSet dedupes before the cardinality check",
    ),
    (
        "discovery.candidate-peer",
        "/addresses has 65 copies of one item, over maxItems",
        "BTreeSet dedupes before the cardinality check",
    ),
    (
        "discovery.candidate-peer",
        "/expires_at is explicit null rather than absent",
        "null read as absence",
    ),
    (
        "endpoints.direct-destination",
        "<root> is closed but carries an unknown property",
        "no deny_unknown_fields",
    ),
    (
        "endpoints.direct-destination",
        "/endpoint is explicit null rather than absent",
        "null read as absence",
    ),
];

fn known(boundary: &str, label: &str) -> bool {
    KNOWN_DISAGREEMENTS
        .iter()
        .any(|(b, l, _)| *b == boundary && *l == label)
}

#[derive(Debug)]
struct Disagreement {
    boundary: &'static str,
    label: String,
    pointer: String,
    document: String,
    kind: &'static str,
}

fn excused(boundary: &str, label: &str) -> bool {
    STRICTER_THAN_SCHEMA
        .iter()
        .any(|(b, fragment, _)| *b == boundary && label.contains(fragment))
}

struct Outcome {
    checked: usize,
    disagreements: Vec<Disagreement>,
    /// Schema constraints no Rust boundary enforces yet.
    unenforced: Vec<String>,
}

fn run(boundary: &Boundary, index: &SchemaIndex) -> Outcome {
    let validator = validator_for(boundary.schema);
    let schema = schema_doc(boundary.schema);

    assert!(
        validator.is_valid(&boundary.seed),
        "{}: the seed must be VALID or every mutation is meaningless: {}",
        boundary.name,
        boundary.seed
    );
    assert!(
        (boundary.parse)(&boundary.seed),
        "{}: Rust must accept the seed, or this suite measures nothing",
        boundary.name
    );

    let candidates: Vec<Candidate> = mutations(&schema, &boundary.seed, index);
    assert!(
        candidates.len() > 5,
        "{}: only {} candidates — the generator is not seeing this schema",
        boundary.name,
        candidates.len()
    );

    let mut checked = 0;
    let mut out = Vec::new();
    let mut unenforced = Vec::new();
    for c in candidates {
        let schema_valid = validator.is_valid(&c.document);
        let rust_accepts = (boundary.parse)(&c.document);
        checked += 1;

        if !(boundary.responsible)(&c) {
            if !schema_valid && rust_accepts {
                unenforced.push(format!("{}: {}", boundary.name, c.label));
            }
            continue;
        }

        let kind = match (schema_valid, rust_accepts) {
            // The defect direction: the boundary admits what the contract
            // forbids.
            (false, true) => "SCHEMA REJECTS, RUST ACCEPTS",
            // Stricter than the contract. Legal, but only on purpose.
            (true, false) if !excused(boundary.name, &c.label) => "SCHEMA ACCEPTS, RUST REJECTS",
            _ => continue,
        };
        out.push(Disagreement {
            boundary: boundary.name,
            label: c.label.clone(),
            pointer: c.pointer,
            document: {
                let s = serde_json::to_string(&c.document).unwrap_or_default();
                if s.len() > 200 {
                    format!("{}… ({} bytes)", &s[..200], s.len())
                } else {
                    s
                }
            },
            kind,
        });
    }
    Outcome {
        checked,
        disagreements: out,
        unenforced,
    }
}

#[test]
fn every_boundary_rejects_what_its_schema_rejects() {
    let index = index();
    let mut all = Vec::new();
    let mut total = 0;

    let mut unenforced = Vec::new();
    for boundary in boundaries() {
        let outcome = run(&boundary, &index);
        total += outcome.checked;
        all.extend(outcome.disagreements);
        unenforced.extend(outcome.unenforced);
    }

    // Not a failure: these are constraints whose enforcing layer has not
    // been built yet. Printed every run so a missing layer stays visible
    // instead of quietly reading as coverage.
    if !unenforced.is_empty() {
        println!(
            "\n{} schema constraint(s) with no Rust boundary yet:",
            unenforced.len()
        );
        for u in &unenforced {
            println!("  - {u}");
        }
    }

    assert!(
        total > 100,
        "only {total} candidates across every boundary; the generator has stopped generating"
    );

    let (expected, fresh): (Vec<_>, Vec<_>) =
        all.into_iter().partition(|d| known(d.boundary, &d.label));

    if !fresh.is_empty() {
        let mut report = format!(
            "\n{} NEW disagreement(s) between the schemas and the Rust boundaries \
             ({total} candidates checked):\n\n",
            fresh.len()
        );
        for d in &fresh {
            report.push_str(&format!(
                "  [{}] {}\n    at {}: {}\n    {}\n\n",
                d.kind, d.boundary, d.pointer, d.label, d.document
            ));
        }
        panic!("{report}");
    }

    // The inventory may only shrink. A fixed defect whose line survives
    // here would leave the list describing a past that no longer exists,
    // and the next reader would trust it.
    let still_broken: Vec<&str> = expected.iter().map(|d| d.label.as_str()).collect();
    let stale: Vec<&(&str, &str, &str)> = KNOWN_DISAGREEMENTS
        .iter()
        .filter(|(b, l, _)| {
            !expected
                .iter()
                .any(|d| d.boundary == *b && d.label.as_str() == *l)
        })
        .collect();
    assert!(
        stale.is_empty(),
        "these known disagreements no longer reproduce — delete their lines from \
         KNOWN_DISAGREEMENTS, the defect is fixed:\n{stale:#?}"
    );

    println!(
        "negative conformance: {total} invalid candidates checked, \
         {} known disagreement(s) outstanding",
        still_broken.len()
    );
}

#[test]
fn the_generator_finds_the_constraints_it_claims_to() {
    // A generator that silently stopped generating would make the suite
    // above pass forever. These are floors, not exact counts: adding a
    // constraint to a schema must never require editing this test.
    let index = index();
    for boundary in boundaries() {
        let schema = schema_doc(boundary.schema);
        let candidates = mutations(&schema, &boundary.seed, &index);

        assert!(
            candidates
                .iter()
                .any(|c| c.label.contains("root is a JSON null")),
            "{}: no root-type candidate",
            boundary.name
        );
        assert!(
            !candidates
                .iter()
                .any(|c| c.label.contains("UNRESOLVABLE $ref")),
            "{}: the generator could not follow a $ref, so constraints behind it are untested: {:?}",
            boundary.name,
            candidates
                .iter()
                .filter(|c| c.label.contains("UNRESOLVABLE"))
                .map(|c| &c.pointer)
                .collect::<Vec<_>>()
        );
    }
}
