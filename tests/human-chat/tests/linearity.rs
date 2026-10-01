// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Render cost stays linear in the input (`HUMAN-CHAT.md`: "a renderer
//! must not superlinearly amplify pathological input").
//!
//! Each pathological shape is rendered at `n` and at `4n` bytes, both
//! inside the decoded ceiling, and the larger may take at most
//! [`ALLOWED_RATIO`] times the smaller. Linear is 4; quadratic is 16. The
//! best of several runs is compared, so a loaded host slows both sides
//! rather than one, and the margin above 4 absorbs what remains.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::{Duration, Instant};

use interweave_human_chat_protocol::render;

/// Four times the input may cost at most this many times the time.
const ALLOWED_RATIO: f64 = 9.0;

/// The size of the smaller input; the larger is four times it, still
/// under the 196,608-byte ceiling so both are parsed.
const BASE: usize = 40_000;

fn best_of(runs: usize, source: &str) -> Duration {
    (0..runs)
        .map(|_| {
            let start = Instant::now();
            std::hint::black_box(render(std::hint::black_box(source)));
            start.elapsed()
        })
        .min()
        .expect("at least one run")
}

/// `unit` repeated to `len` bytes.
fn fill(unit: &str, len: usize) -> String {
    unit.repeat(len / unit.len())
}

#[test]
fn every_pathological_shape_renders_in_linear_time() {
    let shapes: [(&str, &str); 7] = [
        ("unclosed link brackets", "["),
        ("unclosed emphasis runs", "*a"),
        ("alternating delimiters", "*_~"),
        ("backtick runs", "`a``"),
        ("nested blockquote markers", "> > > > a\n"),
        ("raw html openers", "<a "),
        ("a long table", "| x | y |\n"),
    ];
    for (name, unit) in shapes {
        let small = fill(unit, BASE);
        let large = fill(unit, BASE * 4);
        let source_small = if name == "a long table" {
            format!("| h | h |\n| - | - |\n{small}")
        } else {
            small
        };
        let source_large = if name == "a long table" {
            format!("| h | h |\n| - | - |\n{large}")
        } else {
            large
        };
        let t_small = best_of(5, &source_small).max(Duration::from_micros(50));
        let t_large = best_of(5, &source_large);
        let ratio = t_large.as_secs_f64() / t_small.as_secs_f64();
        assert!(
            ratio <= ALLOWED_RATIO,
            "{name}: 4x the input took {ratio:.1}x the time ({t_small:?} -> {t_large:?})"
        );
    }
}
