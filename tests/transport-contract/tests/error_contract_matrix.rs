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
        error: "Overloaded",
        proof: Proof::Test("a_rate_limited_retry_does_not_erase_the_accepted_route"),
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
        text: "endpoint handshake error mapping is exact",
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
];

/// Every error code the matrix knows how to look for. Used to re-derive
/// the clause list from the documents in the totality test, so a code
/// added to the vocabulary but never to a contract cannot slip past.
const CODES: &[&str] = &[
    "EndpointNotRegistered",
    "InvalidArgument",
    "UnauthorizedPeer",
    "CapabilityDenied",
    "PeerUnknown",
    "PeerUnreachable",
    "ProtocolViolation",
    "Overloaded",
    "EndpointUnknown",
    "EndpointDisabled",
    "EndpointClientKindDenied",
    "EndpointInUse",
];

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
fn error_clauses(doc: &str) -> Vec<(usize, String)> {
    read(doc)
        .lines()
        .enumerate()
        .filter(|(_, line)| CODES.iter().any(|c| line.contains(&format!("`{c}`"))))
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
    let mut uncovered = Vec::new();
    for doc in [ENDPOINTS, DIRECT] {
        for (line_no, line) in error_clauses(doc) {
            let covered = MATRIX.iter().any(|c| c.doc == doc && line.contains(c.text));
            if !covered {
                let shown: String = line.chars().take(160).collect();
                uncovered.push(format!("  {doc}:{line_no}\n    {shown}"));
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
        let name = match clause.proof {
            Proof::Test(n) | Proof::Unrepresentable(n) => n,
            Proof::Stage(..) => continue,
        };
        let needle = format!("fn {name}(");
        let Some(body) = sources.iter().find(|s| s.contains(&needle)) else {
            broken.push(format!("  `{name}` — no test by that name exists"));
            continue;
        };
        // The proof must at least speak about the error it claims. This
        // is weak on its own and deliberately so: it catches the citation
        // that drifted onto an unrelated test, which is the realistic
        // failure. It is not a substitute for the test itself.
        if !body.contains(clause.error) {
            broken.push(format!(
                "  `{name}` — exists, but its file never mentions {}",
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
    let mut seen = BTreeSet::new();
    for clause in MATRIX {
        assert!(
            seen.insert((clause.doc, clause.text)),
            "MATRIX quotes {:?} from {} twice",
            clause.text,
            clause.doc
        );
        assert!(
            CODES.contains(&clause.error),
            "MATRIX row for {:?} names {}, which is not in CODES — the \
             totality test cannot look for it",
            clause.text,
            clause.error
        );
    }
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
