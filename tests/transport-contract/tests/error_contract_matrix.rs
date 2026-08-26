// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! Every normative `condition -> error` clause, and what proves it.
//!
//! Stage 6 took thirteen review rounds and thirty-seven findings, and
//! **roughly half were the same shape**: a normative document named an
//! error code and the implementation returned a different one.
//! `CapabilityDenied` where `ENDPOINTS.md` outbound step 3 says
//! `UnauthorizedPeer`. `PeerUnreachable` where `DIRECT.md` line 94 says
//! `PeerUnknown`. A `received_at` the schema marks **required** and the
//! event did not carry.
//!
//! None of those were hard to see once pointed at. They were hard to
//! *look for*, because the only thing binding the prose to the code was
//! whoever had most recently read both. `validate_contracts.py` checks
//! that schemas are well-formed and traceable — it checks **shape**.
//! Nothing checked specified **behaviour**.
//!
//! This file is that check, and it is deliberately two-sided:
//!
//! 1. **Each row quotes its clause verbatim.** If someone rewords the
//!    contract, [`every_cited_clause_still_reads_that_way`] fails and the
//!    row has to be revisited. A citation that cannot go stale silently
//!    is worth more than one that is merely present.
//! 2. **The matrix must be total.** [`the_contracts_name_no_error_this
//!    _matrix_has_missed`] re-derives the clause list from the documents
//!    themselves and fails when a clause has no row. Adding a rule to
//!    `ENDPOINTS.md` therefore turns this suite red until somebody says
//!    what proves it — which is the property the last thirteen rounds
//!    did not have.
//!
//! What this file does NOT do is re-assert the behaviour. The proofs
//! already exist and mostly run over real sockets, where they belong;
//! duplicating them here would create a second place to update and a new
//! way to disagree. Each row *names* its proof, and
//! [`every_proof_this_matrix_cites_exists`] checks the name still
//! resolves to a real test that mentions the error it claims to prove.

// A conformance matrix is assertions all the way down; `expect` on a
// contract that cannot be read is the correct failure, not a smell.
#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::PathBuf;

/// What establishes a clause. The distinction is load-bearing: these are
/// three genuinely different kinds of assurance, and flattening them
/// would let the weakest pass for the strongest.
#[derive(Debug, Clone, Copy)]
enum Proof {
    /// A test drives the condition and asserts the error.
    Test(&'static str),
    /// The condition cannot be constructed. `EndpointId` validates its
    /// grammar in its constructor, so a malformed one never reaches the
    /// send path at all. The named test proves the type rejects it —
    /// without that, "unrepresentable" is an assertion about code
    /// somebody remembers writing.
    Unrepresentable(&'static str),
    /// The line declares the vocabulary rather than stating a
    /// condition. `DIRECT.md` line 58 lists the seven coarse reason
    /// codes; what makes that list correct is the schema it copies, and
    /// the round-trip that drives every local category through
    /// `to_wire` into that schema. Holding it to naming each code in a
    /// test body would demand seven tests of a list.
    Vocabulary(&'static str),
    /// Not reachable until a later stage opens, with the stage that will
    /// make it reachable. Checked against the open stage, so this cannot
    /// quietly become permanent — the same discipline as
    /// `tools/checks/domain_fn_exempt.txt`.
    Stage(u32, &'static str),
}

struct Clause {
    /// Path under the repository root.
    doc: &'static str,
    /// Verbatim from `doc`. Not a paraphrase: the point is that it
    /// breaks when the contract changes.
    text: &'static str,
    /// The error code the clause names.
    error: &'static str,
    proof: Proof,
}

const ENDPOINTS: &str = "architecture/contracts/ENDPOINTS.md";
const DIRECT: &str = "architecture/transport/libp2p/DIRECT.md";

#[rustfmt::skip]
const MATRIX: &[Clause] = &[
    // --- local endpoint lease (ENDPOINTS.md "Local endpoint lease") ---
    //
    // A lease is acquired by a `LocalDataSession` over IPC. Stage 6's
    // runtime handle owns every endpoint and is the only caller, so
    // "a session asks for an endpoint and is refused" has no second
    // party to be refused. Stage 8 is where that boundary arrives.
    Clause {
        doc: ENDPOINTS,
        text: "malformed EndpointId -> local `InvalidArgument`",
        error: "InvalidArgument",
        proof: Proof::Unrepresentable("endpoint_id_rejects_everything_outside_the_grammar"),
    },
    Clause {
        doc: ENDPOINTS,
        text: "configured endpoint absent -> local `EndpointUnknown`",
        error: "EndpointUnknown",
        proof: Proof::Stage(8, "lease acquisition needs a session to refuse"),
    },
    Clause {
        doc: ENDPOINTS,
        text: "configured endpoint disabled -> local `EndpointDisabled`",
        error: "EndpointDisabled",
        proof: Proof::Stage(8, "lease acquisition needs a session to refuse"),
    },
    Clause {
        doc: ENDPOINTS,
        text: "configured `allowed_client_kinds` mismatch -> local `EndpointClientKindDenied`",
        error: "EndpointClientKindDenied",
        proof: Proof::Stage(8, "no client kind exists to mismatch before IPC sessions"),
    },
    Clause {
        doc: ENDPOINTS,
        text: "ungranted capability/connection authorization -> local `CapabilityDenied`",
        error: "CapabilityDenied",
        proof: Proof::Stage(8, "capabilities are granted to sessions, which arrive in Stage 8"),
    },
    Clause {
        doc: ENDPOINTS,
        text: "duplicate claim -> `EndpointInUse`",
        error: "EndpointInUse",
        proof: Proof::Stage(8, "one caller cannot collide with itself"),
    },

    // --- outbound routing order (ENDPOINTS.md steps 1-8) -------------
    Clause {
        doc: ENDPOINTS,
        text: "caller must own an active endpoint lease or receive `EndpointNotRegistered`",
        error: "EndpointNotRegistered",
        proof: Proof::Test("a_source_endpoint_without_a_lease_is_refused"),
    },
    Clause {
        doc: ENDPOINTS,
        text: "malformed/noncanonical endpoint input is `InvalidArgument`",
        error: "InvalidArgument",
        proof: Proof::Unrepresentable("endpoint_id_rejects_everything_outside_the_grammar"),
    },
    Clause {
        doc: ENDPOINTS,
        text: "a destination excluded by that narrowing policy returns `UnauthorizedPeer` locally",
        error: "UnauthorizedPeer",
        proof: Proof::Test("the_source_endpoints_outbound_policy_narrows_a_trusted_peer"),
    },
    Clause {
        doc: ENDPOINTS,
        text: "self PeerId is rejected as `InvalidArgument`",
        error: "InvalidArgument",
        proof: Proof::Test("sending_to_the_local_peer_is_invalid_argument"),
    },
    Clause {
        doc: ENDPOINTS,
        text: "Invalid remote metadata is `ProtocolViolation` and does not become tool/UI metadata.",
        error: "ProtocolViolation",
        proof: Proof::Test("an_explicit_destination_must_be_answered_by_that_endpoint"),
    },

    // --- retry and dedup ---------------------------------------------
    Clause {
        doc: ENDPOINTS,
        text: "A retry rejected by the current direct-ingress token bucket receives coarse `overloaded` before dedup lookup",
        error: "overloaded",
        proof: Proof::Test("a_trusted_peer_is_refused_once_its_burst_is_spent"),
    },

    // --- endpoint directory and handshake (later stages) -------------
    Clause {
        doc: ENDPOINTS,
        text: ">32 entries, invalid EndpointId grammar, or duplicates are `ProtocolViolation`",
        error: "ProtocolViolation",
        proof: Proof::Stage(8, "no directory exchange exists before the endpoint directory"),
    },
    Clause {
        doc: ENDPOINTS,
        text: "malformed=`InvalidArgument`",
        error: "InvalidArgument",
        proof: Proof::Stage(13, "the endpoint handshake is part of desktop IPC v2"),
    },

    // --- DIRECT.md ----------------------------------------------------
    Clause {
        doc: DIRECT,
        text: "Invalid/mismatched response metadata is a local `ProtocolViolation`",
        error: "ProtocolViolation",
        proof: Proof::Test("a_response_echoing_the_wrong_id_is_not_an_answer_to_this_exchange"),
    },
    Clause {
        doc: DIRECT,
        text: "sending to the local profile PeerId is `InvalidArgument`; self-dial never occurs",
        error: "InvalidArgument",
        proof: Proof::Test("sending_to_the_local_peer_is_invalid_argument"),
    },
    Clause {
        doc: DIRECT,
        text: "no usable candidate addresses -> `PeerUnknown` without ad hoc discovery",
        error: "PeerUnknown",
        proof: Proof::Test("an_unknown_peer_and_an_unreachable_one_are_told_apart"),
    },
    Clause {
        doc: DIRECT,
        text: "connection/protocol negotiation failure -> `PeerUnreachable`",
        error: "PeerUnreachable",
        proof: Proof::Test("an_unknown_peer_and_an_unreachable_one_are_told_apart"),
    },

    // --- coarse wire rejections -------------------------------------
    //
    // Every one of these was invisible to the first version of this
    // matrix: the curated code list held only the local `ErrorCode`
    // vocabulary, so `ENDPOINTS.md` and `DIRECT.md` could state a wire
    // rule and the totality test would not look for it.
    Clause {
        doc: ENDPOINTS,
        text: "If no default exists, or the default endpoint has no active local lease, the remote request receives the same coarse `no_route` rejection",
        error: "no_route",
        proof: Proof::Test("every_routing_refusal_is_indistinguishable_on_the_wire"),
    },
    Clause {
        doc: ENDPOINTS,
        text: "overflow returns coarse `overloaded`",
        error: "overloaded",
        proof: Proof::Test("a_trusted_peer_is_refused_once_its_burst_is_spent"),
    },
    Clause {
        doc: ENDPOINTS,
        text: "the wire response is the coarse `no_route` rejection",
        error: "no_route",
        proof: Proof::Test("a_destination_endpoints_inbound_policy_is_coarse_no_route"),
    },
    Clause {
        doc: ENDPOINTS,
        text: "Queue admission failure at step 8 returns coarse `overloaded`, not `Accepted`",
        error: "overloaded",
        proof: Proof::Test("a_full_endpoint_queue_is_overloaded_and_never_falsely_accepted"),
    },
    Clause {
        doc: ENDPOINTS,
        text: "different fingerprint under the same dedup key -> reject as a duplicate-ID/content conflict (`malformed` on the coarse wire",
        error: "malformed",
        proof: Proof::Test("the_same_id_with_a_different_body_is_a_conflict"),
    },
    Clause {
        doc: ENDPOINTS,
        text: "a trusted peer denied by an endpoint-specific inbound policy receives `no_route`, not an oracle",
        error: "no_route",
        proof: Proof::Test("a_destination_endpoints_inbound_policy_is_coarse_no_route"),
    },
    Clause {
        doc: ENDPOINTS,
        text: "no default -> coarse remote `no_route`",
        error: "no_route",
        proof: Proof::Test("every_routing_refusal_is_indistinguishable_on_the_wire"),
    },
    Clause {
        doc: DIRECT,
        // This line declares the vocabulary rather than stating a
        // condition, so its proof is the round-trip that drives every
        // local category through `to_wire` and validates the result
        // against the schema that defines the list.
        text: "Coarse reason codes: `no_route`, `unauthorized_peer`, `overloaded`, `malformed`, `too_large`, `shutting_down`, `unsupported`.",
        error: "overloaded",
        proof: Proof::Test("every_direct_reject_reason_validates"),
    },
    Clause {
        doc: DIRECT,
        text: "`no_route` deliberately collapses endpoint unknown, endpoint disabled, no active lease, missing default endpoint, and endpoint-specific policy denial.",
        error: "no_route",
        proof: Proof::Test("every_routing_refusal_is_indistinguishable_on_the_wire"),
    },
    Clause {
        doc: DIRECT,
        text: "A rate-limited retry may instead receive coarse `overloaded`",
        error: "overloaded",
        proof: Proof::Test("a_trusted_peer_is_refused_once_its_burst_is_spent"),
    },
    Clause {
        doc: DIRECT,
        text: "overflow is `overloaded` and never creates a parallel enqueue path",
        error: "overloaded",
        proof: Proof::Test("a_waiter_with_no_recorded_owner_is_told_overloaded_not_nothing"),
    },
    Clause {
        doc: DIRECT,
        text: "successful v2 exchange but remote `no_route` -> `RemoteEndpointUnavailable` locally.",
        error: "RemoteEndpointUnavailable",
        proof: Proof::Test("an_unknown_endpoint_is_indistinguishable_no_route"),
    },
    Clause {
        doc: DIRECT,
        text: "Stop accepting new direct requests, respond `shutting_down` where possible",
        error: "shutting_down",
        proof: Proof::Test("draining_is_shutting_down_on_the_wire"),
    },
    Clause {
        doc: DIRECT,
        text: "Coarse reason codes: `no_route`, `unauthorized_peer`, `overloaded`, `malformed`, `too_large`, `shutting_down`, `unsupported`.",
        error: "no_route",
        proof: Proof::Vocabulary("every_direct_reject_reason_validates"),
    },
    Clause {
        doc: DIRECT,
        text: "Coarse reason codes: `no_route`, `unauthorized_peer`, `overloaded`, `malformed`, `too_large`, `shutting_down`, `unsupported`.",
        error: "unauthorized_peer",
        proof: Proof::Vocabulary("every_direct_reject_reason_validates"),
    },
    Clause {
        doc: DIRECT,
        text: "Coarse reason codes: `no_route`, `unauthorized_peer`, `overloaded`, `malformed`, `too_large`, `shutting_down`, `unsupported`.",
        error: "malformed",
        proof: Proof::Vocabulary("every_direct_reject_reason_validates"),
    },
    Clause {
        doc: DIRECT,
        text: "Coarse reason codes: `no_route`, `unauthorized_peer`, `overloaded`, `malformed`, `too_large`, `shutting_down`, `unsupported`.",
        error: "too_large",
        proof: Proof::Vocabulary("every_direct_reject_reason_validates"),
    },
    Clause {
        doc: DIRECT,
        text: "Coarse reason codes: `no_route`, `unauthorized_peer`, `overloaded`, `malformed`, `too_large`, `shutting_down`, `unsupported`.",
        error: "shutting_down",
        proof: Proof::Vocabulary("every_direct_reject_reason_validates"),
    },
    Clause {
        doc: DIRECT,
        text: "Coarse reason codes: `no_route`, `unauthorized_peer`, `overloaded`, `malformed`, `too_large`, `shutting_down`, `unsupported`.",
        error: "unsupported",
        proof: Proof::Vocabulary("every_direct_reject_reason_validates"),
    },
    // --- one line, several rules ------------------------------------
    //
    // Totality is matched per CODE, not per line, so the second rule on
    // a shared line needs its own row. Without these the handshake line
    // recorded one of six mappings and the retry paragraph one of two.
    Clause {
        doc: ENDPOINTS,
        text: "Capacity exhaustion rejects the new request as coarse wire `overloaded` / local `Overloaded`",
        error: "Overloaded",
        proof: Proof::Test("a_full_endpoint_queue_is_overloaded_and_never_falsely_accepted"),
    },
    Clause {
        doc: ENDPOINTS,
        text: "Capacity exhaustion rejects the new request as coarse wire `overloaded` / local `Overloaded`",
        error: "overloaded",
        proof: Proof::Test("a_full_endpoint_queue_is_overloaded_and_never_falsely_accepted"),
    },
    Clause {
        doc: ENDPOINTS,
        text: "absent=`EndpointUnknown`",
        error: "EndpointUnknown",
        proof: Proof::Stage(13, "the endpoint handshake is part of desktop IPC v2"),
    },
    Clause {
        doc: ENDPOINTS,
        text: "disabled=`EndpointDisabled`",
        error: "EndpointDisabled",
        proof: Proof::Stage(13, "the endpoint handshake is part of desktop IPC v2"),
    },
    Clause {
        doc: ENDPOINTS,
        text: "kind mismatch=`EndpointClientKindDenied`",
        error: "EndpointClientKindDenied",
        proof: Proof::Stage(13, "the endpoint handshake is part of desktop IPC v2"),
    },
    Clause {
        doc: ENDPOINTS,
        text: "capability denied=`CapabilityDenied`",
        error: "CapabilityDenied",
        proof: Proof::Stage(13, "the endpoint handshake is part of desktop IPC v2"),
    },
    Clause {
        doc: ENDPOINTS,
        text: "collision=`EndpointInUse`",
        error: "EndpointInUse",
        proof: Proof::Stage(13, "the endpoint handshake is part of desktop IPC v2"),
    },
    // `no_route` here is the STIMULUS, not the local result — the clause
    // reads "remote `no_route` -> `RemoteEndpointUnavailable`". The local
    // half is the row above; this is the remote half, proven where the
    // wire code is actually asserted.
    Clause {
        doc: DIRECT,
        text: "successful v2 exchange but remote `no_route`",
        error: "no_route",
        proof: Proof::Test("every_routing_refusal_is_indistinguishable_on_the_wire"),
    },
];

/// The error vocabularies, read from the schemas that define them.
///
/// **Not a list in this file.** A curated list is only as total as
/// whoever last curated it, and the first version of this matrix proved
/// exactly that: `RemoteEndpointUnavailable` was missing, so `DIRECT.md`
/// line 97 named an error nothing here looked for, and the totality test
/// passed while missing twelve clauses it claimed to enumerate. A matrix
/// that can be incomplete without failing is the defect it exists to
/// prevent.
///
/// Both vocabularies count. The local `ErrorCode` is what a caller sees;
/// the lowercase `DirectRejectReason` is what crosses the wire, and both
/// documents state rules about each.
const ERROR_CODE_SCHEMA: &str = "architecture/contracts/schemas/ipc/error-code.schema.json";
const REJECT_REASON_SCHEMA: &str =
    "architecture/contracts/schemas/direct/reject-reason.schema.json";

fn vocabulary() -> BTreeSet<String> {
    let mut all = BTreeSet::new();
    for schema in [ERROR_CODE_SCHEMA, REJECT_REASON_SCHEMA] {
        let doc: serde_json::Value =
            serde_json::from_str(&read(schema)).expect("the schema is valid JSON");
        let variants = doc
            .get("enum")
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| panic!("{schema} declares no `enum` of codes"));
        for v in variants {
            all.insert(
                v.as_str()
                    .expect("every code in the enum is a string")
                    .to_string(),
            );
        }
    }
    assert!(!all.is_empty(), "the error vocabulary cannot be empty");
    all
}

/// The Rust spelling of a wire reason: `no_route` -> `NoRoute`.
///
/// The schemas speak the wire's snake_case and the tests assert
/// `DirectRejectReason::NoRoute`, so a proof is accepted under either
/// spelling of the same code.
fn rust_spelling(code: &str) -> String {
    code.split('_')
        .map(|part| {
            let mut c = part.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("tests/<pkg> is two levels below the root")
        .to_path_buf()
}

fn read(doc: &str) -> String {
    let path = repo_root().join(doc);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read the normative contract {}: {e}", path.display()))
}

/// The open stage, from the one machine-readable place it is recorded.
fn open_stage() -> u32 {
    let manifest = read("Cargo.toml");
    let line = manifest
        .lines()
        .find(|l| l.trim_start().starts_with("status = \"stage-"))
        .expect("workspace.metadata.interweave.status names the open stage");
    let rest = line.split("stage-").nth(1).expect("checked above");
    rest.chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .expect("the status is stage-<number>-<name>")
}

/// Lines of a document that name an error code, with their 1-based
/// numbers — the clause list, re-derived from the source of truth
/// instead of from this file.
fn error_clauses(doc: &str, vocab: &BTreeSet<String>) -> Vec<(usize, String)> {
    read(doc)
        .lines()
        .enumerate()
        .filter(|(_, line)| vocab.iter().any(|c| line.contains(&format!("`{c}`"))))
        .map(|(i, line)| (i + 1, line.to_string()))
        .collect()
}

/// A cited clause must still read that way in the contract.
///
/// This is what makes the citations load-bearing rather than decorative.
/// Reword the contract and this fails, which is the moment to ask whether
/// the code and its proof still match the new words.
#[test]
fn every_cited_clause_still_reads_that_way() {
    let mut missing = Vec::new();
    for clause in MATRIX {
        if !read(clause.doc).contains(clause.text) {
            missing.push(format!("  {}\n    quoted: {:?}", clause.doc, clause.text));
        }
    }
    assert!(
        missing.is_empty(),
        "these rows quote text that is no longer in the contract.\n\
         The contract changed; the row, the code, and the proof all need \
         re-reading before the quote is updated:\n{}",
        missing.join("\n")
    );
}

/// Every error clause in the contracts has a row here.
///
/// **The totality property, and the reason this file exists.** Adding a
/// `condition -> error` rule to a normative document turns this test red
/// until someone records what proves it. Without this, a new rule is
/// enforced by whoever next reads the prose — which is exactly how
/// Stage 6 accumulated seventeen contract-code mismatches.
#[test]
fn the_contracts_name_no_error_this_matrix_has_missed() {
    let vocab = vocabulary();
    let mut uncovered = Vec::new();
    for doc in [ENDPOINTS, DIRECT] {
        for (line_no, line) in error_clauses(doc, &vocab) {
            // Per CODE, not per line. A single line often states more
            // than one rule — the handshake line maps six conditions,
            // and the resolution paragraph gives `no_route` for steps
            // 5-7 and `overloaded` for step 8. Marking a whole line
            // covered because one row matched it meant a rule could be
            // appended to any already-covered line while the totality
            // test stayed green.
            for code in vocab.iter().filter(|c| line.contains(&format!("`{c}`"))) {
                let covered = MATRIX
                    .iter()
                    .any(|c| c.doc == doc && c.error == code && line.contains(c.text));
                if !covered {
                    let shown: String = line.chars().take(140).collect();
                    uncovered.push(format!("  {doc}:{line_no}  `{code}`\n    {shown}"));
                }
            }
        }
    }
    assert!(
        uncovered.is_empty(),
        "these normative clauses name an error code and no row in MATRIX \
         covers them.\nAdd a row saying what proves each one — a test, a \
         type that makes the condition unrepresentable, or the stage that \
         will make it reachable:\n{}",
        uncovered.join("\n")
    );
}

/// Every proof this matrix cites is a real test that mentions its error.
///
/// A citation to a test that was renamed or deleted looks exactly like a
/// citation to one that still runs. This resolves the name against the
/// tracked sources so it cannot.
#[test]
fn every_proof_this_matrix_cites_exists() {
    let sources = tracked_rust_sources();
    let mut broken = Vec::new();

    for clause in MATRIX {
        // The two kinds of proof are checked differently, because they
        // claim different things. A `Test` asserts the caller sees this
        // error, so its body must name it. An `Unrepresentable` proves
        // the condition cannot be built at all — `EndpointId` rejects a
        // malformed string in its constructor, returning its own type's
        // error and never `TransportError::InvalidArgument`, which the
        // caller never reaches. Demanding the error name there asks the
        // proof to mention something it exists to make impossible.
        let (name, must_name_error) = match clause.proof {
            Proof::Test(n) => (n, true),
            Proof::Unrepresentable(n) | Proof::Vocabulary(n) => (n, false),
            Proof::Stage(..) => continue,
        };
        let needle = format!("fn {name}(");
        // EVERY file declaring that name, not the first. The name
        // `the_same_id_with_a_different_body_is_a_conflict` exists in
        // both `dedup.rs` and `direct_inbound.rs`, and taking the first
        // match reported a sound citation as broken — the dedup test
        // asserts the conflict, the admission test asserts the
        // `malformed` it becomes on the wire. A shared name is resolved
        // by "one of them proves it", which is looser than unique
        // resolution would be and the reason the name must still be a
        // test that names the error.
        let bodies: Vec<String> = sources
            .iter()
            .filter(|s| s.contains(&needle))
            .filter_map(|s| proof_body(s, name))
            .collect();
        if bodies.is_empty() {
            broken.push(format!("  `{name}` — no test by that name exists"));
            continue;
        }
        // The proof must speak about the error it claims, in ITS OWN
        // body. Searching the whole file was the first version of this
        // and it was worthless: any unrelated test, comment, or
        // production code in the same file satisfied it, which is how a
        // row citing `a_rate_limited_retry_does_not_erase_the_accepted_route`
        // for `Overloaded` passed while that test asserts
        // `Refusal::RateLimited` and never mentions the wire code.
        let names_it = bodies
            .iter()
            .any(|b| b.contains(clause.error) || b.contains(&rust_spelling(clause.error)));
        if must_name_error && !names_it {
            broken.push(format!(
                "  `{name}` — exists, but neither it nor the helpers it \
                 calls mention {}",
                clause.error
            ));
        }
    }
    assert!(
        broken.is_empty(),
        "these rows cite a proof that does not hold up:\n{}",
        broken.join("\n")
    );
}

/// A row records the code its own quotation names.
///
/// Without this a row can drift into claiming a mapping its clause does
/// not state, and the totality scan will not notice because it only asks
/// whether SOME row matched the line. Two rows had drifted: the
/// direct-ingress retry clause quotes the coarse wire `overloaded` and
/// the row recorded the distinct local `Overloaded`, so nothing linked
/// that condition to the error the contract actually specifies for it.
#[test]
fn every_row_records_the_code_its_quotation_names() {
    let mut mismatched = Vec::new();
    for clause in MATRIX {
        if !clause.text.contains(clause.error) {
            mismatched.push(format!(
                "  {} records {} but quotes:\n    {:?}",
                clause.doc, clause.error, clause.text
            ));
        }
    }
    assert!(
        mismatched.is_empty(),
        "these rows record a code their own quotation does not name. \
         Quote the fragment that states the mapping, or record the code \
         that fragment actually names:\n{}",
        mismatched.join("\n")
    );
}

/// A `Stage` proof expires once that stage is past.
///
/// Deferring a clause to a later stage is a decision with a date on it.
/// `authorize_outbound` was written in the very stage that was supposed
/// to call it and shipped uncalled anyway, so a deferral that cannot
/// expire is the failure mode, not the exception.
#[test]
fn no_clause_is_deferred_to_a_stage_that_has_already_passed() {
    let open = open_stage();
    let mut expired = Vec::new();
    for clause in MATRIX {
        if let Proof::Stage(stage, reason) = clause.proof
            && stage < open
        {
            expired.push(format!(
                "  {} — {:?}\n    deferred to stage {stage} ({reason}), but stage {open} is open",
                clause.doc, clause.text
            ));
        }
    }
    assert!(
        expired.is_empty(),
        "these clauses were deferred to a stage that is now behind us. \
         Prove them or move the deadline deliberately:\n{}",
        expired.join("\n")
    );
}

/// No two rows claim the same clause, and none is dead.
#[test]
fn the_matrix_has_no_duplicate_or_dead_rows() {
    let vocab = vocabulary();
    let mut seen = BTreeSet::new();
    for clause in MATRIX {
        assert!(
            seen.insert((clause.doc, clause.text, clause.error)),
            "MATRIX quotes {:?} from {} for {} twice",
            clause.text,
            clause.doc,
            clause.error
        );
        assert!(
            vocab.contains(clause.error),
            "MATRIX row for {:?} names {}, which is in neither schema \
             vocabulary — the totality test cannot look for it",
            clause.text,
            clause.error
        );
    }
}

/// The named function's body, plus the bodies of same-file helpers it
/// calls.
///
/// One level of expansion, and it is not optional: assertions are
/// routinely lifted into a shared helper, so
/// `a_trusted_peer_is_refused_once_its_burst_is_spent` proves
/// `Overloaded` entirely through `assert_only_overloaded`. Checking the
/// literal body alone would call that citation broken, which is a false
/// negative, and a check that cries wolf gets deleted.
///
/// Brace counting is deliberately naive — a brace inside a string
/// literal would confuse it. Test bodies do not do that, and the failure
/// mode is a parse error this test reports rather than a silent pass.
fn proof_body(source: &str, name: &str) -> Option<String> {
    let own = span(source, &format!("fn {name}("))?;
    let mut all = own.clone();
    for word in own.split(|c: char| !c.is_alphanumeric() && c != '_') {
        if word.is_empty() || word == name {
            continue;
        }
        if let Some(helper) = span(source, &format!("fn {word}(")) {
            all.push_str(&helper);
        }
    }
    Some(all)
}

/// The `{ .. }` block that follows `needle`, balanced.
fn span(source: &str, needle: &str) -> Option<String> {
    let start = source.find(needle)?;
    let open = start + source[start..].find('{')?;
    let mut depth = 0usize;
    for (i, ch) in source[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(source[open..=open + i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

fn tracked_rust_sources() -> Vec<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root())
        .args(["ls-files", "*.rs"])
        .output()
        .expect("git ls-files");
    assert!(out.status.success(), "git ls-files failed");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|p| std::fs::read_to_string(repo_root().join(p)).ok())
        .collect()
}
