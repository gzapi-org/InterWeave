// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! The tally, and the reason `cargo run` cannot lie about it.
//!
//! A required observation that is false is a FAILURE and the process
//! exits non-zero. A note records something measured that has no
//! pass/fail sense on its own — a crate's event shapes, a ledger — and
//! is still printed, because a spike's value is as much in what it
//! observed as in what it asserted.

use std::fmt::Write as _;

#[derive(Debug, Default)]
pub struct Report {
    lines: Vec<String>,
    failures: Vec<String>,
    divergences: Vec<String>,
    required: usize,
}

impl Report {
    /// Assert something the record depends on.
    pub fn require(&mut self, id: &str, held: bool, claim: &str) {
        self.required += 1;
        let mark = if held { "PASS" } else { "FAIL" };
        let line = format!("{mark} {id}  {claim}");
        if !held {
            self.failures.push(line.clone());
        }
        self.lines.push(line);
    }

    /// Record a measured disagreement between the CODE and an accepted
    /// document.
    ///
    /// Not a failure: the code is what it is, and a spike that exited
    /// non-zero because production has a defect would be unrunnable
    /// until production changed. Not a note either — a note is
    /// something observed, and this is something WRONG. It is printed
    /// under its own heading, counted, and is the spike's primary
    /// output: work the stage must do, with the document it violates
    /// named so a reader can check the claim.
    pub fn divergence(&mut self, id: &str, measured: &str, violates: &str) {
        let line = format!("DIVERGES {id}  {measured}\n           violates: {violates}");
        self.divergences.push(line.clone());
        self.lines.push(line);
    }

    /// Record something measured.
    pub fn note(&mut self, id: &str, detail: impl Into<String>) {
        self.lines.push(format!("note {id}  {}", detail.into()));
    }

    /// Everything observed, in order.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        for line in &self.lines {
            let _ = writeln!(out, "{line}");
        }
        let _ = writeln!(
            out,
            "\n{} required observation(s), {} failed; {} divergence(s) from accepted \
             documents",
            self.required,
            self.failures.len(),
            self.divergences.len()
        );
        for divergence in &self.divergences {
            let _ = writeln!(out, "  {divergence}");
        }
        for failure in &self.failures {
            let _ = writeln!(out, "  {failure}");
        }
        out
    }

    #[must_use]
    pub fn failed(&self) -> bool {
        !self.failures.is_empty()
    }
}
