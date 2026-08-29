// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! SPIKE-003 — Kademlia integration validation.
//!
//! Evidence only. Every experiment asserts, and the process exits
//! non-zero when any required observation is false, so `cargo run`
//! cannot report success while its own output disproves the record.

mod experiments;
mod gate;
mod node;
mod topology;
mod namespace;

/// One experiment's outcome, printed and tallied.
pub struct Report {
    failures: Vec<String>,
    notes: Vec<String>,
}

impl Report {
    fn new() -> Self {
        Self {
            failures: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// A MEASUREMENT rather than an assertion: a number the record
    /// quotes, printed with the run that produced it. Notes never fail
    /// the harness — a claim that must hold is a `check`.
    pub fn note(&mut self, text: String) {
        println!("  [note] {text}");
        self.notes.push(text);
    }

    /// Assert, print, and remember. Never panics: a spike that aborts on
    /// the first surprise reports one fact instead of all of them.
    pub fn check(&mut self, id: &str, claim: &str, held: bool) {
        println!("  [{}] {} {claim}", if held { "ok" } else { "FAIL" }, id);
        if !held {
            self.failures.push(format!("{id}: {claim}"));
        }
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let mut r = Report::new();

    println!("K1 — deterministic protocol derivation from network_id");
    k1(&mut r);

    println!("\nK2 — no Kademlia activity when the behaviour is absent");
    experiments::k2_disabled_is_silent(&mut r).await;

    println!("\nK3 — BucketInserts::Manual: connecting is not routing");
    experiments::k3_manual_bucket_inserts(&mut r).await;

    println!("\nK4 — client and server mode semantics");
    experiments::k4_client_server_modes(&mut r).await;

    println!("\nK5 — bootstrap, and work the library starts by itself");
    experiments::k5_bootstrap_accounting(&mut r).await;

    println!("\nK6 — behaviour-originated dials, measured and gated as production does today");
    experiments::k6_behaviour_dials_are_gated(&mut r).await;

    println!("\nK7 — the same walk under the Stage 10 policy gate");
    experiments::k7_policy_admits_and_refuses(&mut r).await;

    println!("\nK8 — an untrusted peer returned by a query cannot be connected to");
    experiments::k8_untrusted_returned_peer_is_refused(&mut r).await;

    println!("\nK9 — backoff and drain state reach a behaviour dial too");
    experiments::k9_backoff_and_limits_apply(&mut r).await;

    println!("\nK10 — record and provider writes are refused and counted");
    experiments::k10_records_are_filtered(&mut r).await;

    println!("\nK11 — a ten-node line, expanded by random exploration");
    experiments::k11_ten_node_exploration(&mut r).await;

    println!("\nK12 — effective target, no-progress backoff, saturation");
    experiments::k12_effective_target_and_saturation(&mut r).await;

    println!("\nK13 — capability observation, namespace separation, supersession");
    experiments::k13_capability_observation(&mut r).await;

    println!("\nK15 — every SnapshotResult field is computable and bounded");
    experiments::k15_snapshot_is_bounded(&mut r).await;

    println!("\nK16 — disjoint query paths");
    experiments::k16_disjoint_paths(&mut r).await;

    println!("\nK18 — a malicious or stale routing response");
    experiments::k18_stale_routing_response(&mut r).await;

    println!("\nK17 — twenty nodes: convergence and bounded routing state");
    experiments::k17_twenty_node_convergence(&mut r).await;

    println!();
    if r.failures.is_empty() {
        println!("SPIKE-003: all observations held.");
    } else {
        println!("SPIKE-003: {} observation(s) FAILED:", r.failures.len());
        for f in &r.failures {
            println!("  - {f}");
        }
        std::process::exit(1);
    }
}

fn k1(r: &mut Report) {
    // THE GOLDEN VECTOR from kademlia-integration.md §4. Derivation
    // implemented from the specification text; if the two disagree the
    // spec and the code cannot both be right, which is the point.
    let hash = namespace::network_hash("example-private-network");
    r.check(
        "K1.1",
        &format!("the published golden vector reproduces: {hash}"),
        hash == "ssbtblqj7mexczivog5qfbfjvi",
    );
    r.check(
        "K1.2",
        "the derived protocol is the one the spec names",
        namespace::protocol_name("example-private-network")
            == "/interweave/kad/1.0.0/ssbtblqj7mexczivog5qfbfjvi",
    );
    r.check(
        "K1.3",
        "the tag is 26 base32 characters with no padding",
        hash.len() == 26 && !hash.contains('='),
    );

    // DETERMINISM and SEPARATION: the same id twice is the same tag, and
    // two ids that differ by one character share nothing.
    let a = namespace::network_hash("alpha");
    let b = namespace::network_hash("alphb");
    r.check(
        "K1.4",
        "derivation is deterministic across calls",
        a == namespace::network_hash("alpha"),
    );
    r.check(
        "K1.5",
        "a one-character difference gives an unrelated tag",
        a != b,
    );

    // The grammar, since an illegal id must never reach the hash.
    for legal in ["a", "0", "example-private-network", "a.b_c-d", &"z".repeat(64)] {
        r.check(
            "K1.6",
            &format!("legal network_id accepted: {}", &legal[..legal.len().min(20)]),
            namespace::network_id_is_legal(legal),
        );
    }
    for illegal in ["", "-leading", ".leading", "_leading", "Upper", "sp ace", &"z".repeat(65)] {
        r.check(
            "K1.7",
            &format!("illegal network_id refused: {illegal:?}"),
            !namespace::network_id_is_legal(illegal),
        );
    }

    // A LIBP2P STREAM PROTOCOL really accepts the derived name. The
    // derivation is worthless if the result cannot be used.
    let derived = namespace::protocol_name("example-private-network");
    let parsed = libp2p::StreamProtocol::try_from_owned(derived.clone());
    r.check(
        "K1.8",
        "the derived name is a valid libp2p StreamProtocol",
        parsed.as_ref().is_ok_and(|p| p.as_ref() == derived),
    );
}
