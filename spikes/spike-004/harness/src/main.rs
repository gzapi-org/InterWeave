// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! SPIKE-004 harness — AutoNAT v2, Circuit Relay v2 and DCUtR against
//! the production root dial gate.
//!
//! EVIDENCE ONLY. Not a workspace member, not a dependency of anything,
//! and its results reach production as permanent tests rather than as
//! code (CLAUDE.md §4).
//!
//! Phase A: loopback. Everything here is protocol semantics, dial
//! attribution, admission-class enforcement and state machines — the
//! half of SPIKE-004's brief a single machine can answer honestly.
//! NAT traversal efficacy is phase B and is not claimed by anything
//! this binary prints.

mod attribute;
mod experiments;
mod gate;
mod node;
mod production;
mod report;
mod topology;

use report::Report;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let mut report = Report::default();

    experiments::r1_crate_semantics(&mut report).await;
    experiments::r2_dial_attribution(&mut report).await;
    experiments::r3_infrastructure_cannot_reach_the_data_plane(&mut report);
    experiments::r4_autonat_server_dial_back(&mut report).await;
    experiments::r5_circuit_is_not_a_reservation(&mut report).await;
    experiments::r6_production_gate_refuses_the_reservation(&mut report).await;
    experiments::r7_relayed_path_trust(&mut report).await;
    experiments::r8_relayed_inbound_accounting(&mut report).await;
    experiments::r9_relayed_preauth_bucket(&mut report).await;

    print!("{}", report.render());

    // THE EXIT CODE IS THE RECORD. A spike whose own output disproves
    // its README must not report success.
    if report.failed() {
        std::process::exit(1);
    }
}
