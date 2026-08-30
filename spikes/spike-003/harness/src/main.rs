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
    /// How many checks actually ran. A run that asserts nothing is not
    /// a run that passed.
    checks: usize,
}

impl Report {
    fn new() -> Self {
        Self {
            failures: Vec::new(),
            notes: Vec::new(),
            checks: 0,
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
        self.checks += 1;
        println!("  [{}] {} {claim}", if held { "ok" } else { "FAIL" }, id);
        if !held {
            self.failures.push(format!("{id}: {claim}"));
        }
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let mut r = Report::new();
    // A single-experiment filter, so a failing observation can be
    // iterated on without paying six minutes for the whole set. No
    // argument runs everything, which is what the record's reproduction
    // instructions describe.
    let only = std::env::args().nth(1);
    // COUNTED, so a filter that matches nothing cannot report success.
    // `cargo run -- K99` used to select no experiment, run no check,
    // and print "all observations held" with exit 0 — a false green of
    // exactly the kind this harness exists to refuse, in the harness
    // itself.
    let ran = std::cell::Cell::new(0_usize);
    let want = |id: &str| {
        let hit = only.as_deref().is_none_or(|o| o == id);
        if hit {
            ran.set(ran.get() + 1);
        }
        hit
    };

    if want("K1") {
        println!("K1 — deterministic protocol derivation from network_id");
        k1(&mut r);
    }

    if want("K2") {
        println!("\nK2 — no Kademlia activity when the behaviour is absent");
        experiments::k2_disabled_is_silent(&mut r).await;
    }

    if want("K3") {
        println!("\nK3 — BucketInserts::Manual: connecting is not routing");
        experiments::k3_manual_bucket_inserts(&mut r).await;
    }

    if want("K4") {
        println!("\nK4 — client and server mode semantics");
        experiments::k4_client_server_modes(&mut r).await;
    }

    if want("K5") {
        println!("\nK5 — bootstrap, and work the library starts by itself");
        experiments::k5_bootstrap_accounting(&mut r).await;
    }

    if want("K6") {
        println!("\nK6 — behaviour-originated dials, measured and gated as production does today");
        experiments::k6_behaviour_dials_are_gated(&mut r).await;
    }

    if want("K7") {
        println!("\nK7 — the same walk under the Stage 10 policy gate");
        experiments::k7_policy_admits_and_refuses(&mut r).await;
    }

    if want("K8") {
        println!("\nK8 — an untrusted peer returned by a query cannot be connected to");
        experiments::k8_untrusted_returned_peer_is_refused(&mut r).await;
    }

    if want("K9") {
        println!("\nK9 — backoff and drain state reach a behaviour dial too");
        experiments::k9_backoff_and_limits_apply(&mut r).await;
    }

    if want("K10") {
        println!("\nK10 — record and provider writes are refused and counted");
        experiments::k10_records_are_filtered(&mut r).await;
    }

    if want("K11") {
        println!("\nK11 — a ten-node line, expanded by random exploration");
        experiments::k11_ten_node_exploration(&mut r).await;
    }

    if want("K12") {
        println!("\nK12 — effective target, no-progress backoff, saturation");
        experiments::k12_effective_target_and_saturation(&mut r).await;
    }

    if want("K13") {
        println!("\nK13 — capability observation, namespace separation, supersession");
        experiments::k13_capability_observation(&mut r).await;
    }

    if want("K14") {
        println!("\nK14 — targeted lookup, and the evidence rule that gates it");
        experiments::k14_targeted_lookup(&mut r).await;
    }

    if want("K19") {
        println!("\nK19 — the global ceilings reach a behaviour dial");
        experiments::k19_ceilings_apply_to_behaviour_dials(&mut r).await;
    }

    if want("K29") {
        println!("\nK29 — a routed peer that is not connected is still revoked");
        experiments::k29_disconnected_peer_is_still_revoked(&mut r).await;
    }
    if want("K28") {
        println!("\nK28 — a connection whose authority lapsed does not survive");
        experiments::k28_withdrawn_connection_is_closed(&mut r).await;
    }
    if want("K27") {
        println!("\nK27 — behaviour dials under address-table pressure");
        experiments::k27_address_table_pressure(&mut r).await;
    }
    if want("K26") {
        println!("\nK26 — capability-aware manual admission");
        experiments::k26_capability_aware_admission(&mut r).await;
    }
    if want("K25") {
        println!("\nK25 — a multi-address dial where every candidate fails");
        experiments::k25_every_candidate_fails(&mut r).await;
    }
    if want("K24") {
        println!("\nK24 — single-path capture, measured against controls");
        experiments::k24_single_path_capture(&mut r).await;
    }
    if want("K23") {
        println!("\nK23 — behaviour dial volume, by query class");
        experiments::k23_dial_volume_by_class(&mut r).await;
    }
    if want("K22") {
        println!("\nK22 — the bounded query scheduler");
        experiments::k22_bounded_query_scheduler(&mut r).await;
    }
    if want("K21") {
        println!("\nK21 — a behaviour dial offering several addresses");
        experiments::k21_multi_address_behaviour_dial(&mut r).await;
    }
    if want("K20") {
        println!("\nK20 — trust revoked between admission and the handshake");
        experiments::k20_authority_withdrawn_mid_dial(&mut r).await;
    }
    if want("K15") {
        println!("\nK15 — every SnapshotResult field is computable and bounded");
        experiments::k15_snapshot_is_bounded(&mut r).await;
    }

    if want("K16") {
        println!("\nK16 — disjoint query paths");
        experiments::k16_disjoint_paths(&mut r).await;
    }

    if want("K18") {
        println!("\nK18 — a malicious or stale routing response");
        experiments::k18_stale_routing_response(&mut r).await;
    }

    if want("K17") {
        println!("\nK17 — twenty nodes: convergence and bounded routing state");
        experiments::k17_twenty_node_convergence(&mut r).await;
    }

    println!();
    if ran.get() == 0 {
        println!(
            "SPIKE-003: {:?} matched no experiment — nothing ran.",
            only.as_deref().unwrap_or("<none>")
        );
        std::process::exit(2);
    }
    if r.failures.is_empty() {
        println!(
            "SPIKE-003: all observations held ({} experiment(s), {} check(s)).",
            ran.get(),
            r.checks
        );
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
