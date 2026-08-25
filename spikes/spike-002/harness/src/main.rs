// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! SPIKE-002 — rust-libp2p direct request-response and GossipSub cache
//! behaviour, measured rather than assumed.
//!
//! Run: `cargo run` inside this directory. Every experiment prints what
//! it observed; the README records what those observations mean.

mod direct;
mod mesh;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    println!("SPIKE-002 — libp2p 0.56, request-response and gossipsub\n");

    println!("A1. protocol-family negotiation, matching majors");
    direct::a1_matching_majors().await;

    println!("\nA2. unsupported major");
    direct::a2_unsupported_major().await;

    println!("\nA3. two protocol families on one connection");
    direct::a3_two_families().await;

    println!("\nA4. a withheld response, and whether the Swarm keeps working");
    direct::a4_withheld_response().await;

    println!("\nA5. request timeout and what each side is told");
    direct::a5_timeout().await;

    println!("\nA6. concurrent same-key retransmissions through the real scheduler");
    direct::a6_same_key_race().await;

    println!("\nA7. reservation-capacity overflow");
    direct::a7_reservation_overflow().await;

    println!("\nA8. a cancellation race: the owner's connection dies mid-admission");
    direct::a8_cancellation_race().await;

    println!("\nA11. many waiters on ONE key: what bounds them?");
    direct::a11_same_key_waiter_flood().await;

    println!("\nA10. the GLOBAL reservation budget, reached by many peers");
    direct::a10_global_reservation_budget().await;

    println!("\nA9. the no_route privacy class: five reasons, one answer");
    direct::a9_no_route_is_one_answer().await;

    println!("\nB0. the id function under test is the frozen one");
    mesh::b0_message_id_matches_the_golden_vectors();

    println!("\nB1. two publishers, one application message id");
    mesh::b1_distinct_mesh_ids().await;

    println!("\nB2. does an INVALID message create a lasting duplicate-cache entry?");
    mesh::b2_authenticity_before_cache().await;

    println!("\ndone.");
}
