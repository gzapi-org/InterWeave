---
role: "p2p-network-dev"
class: workflow
topic: "gate-the-commit-on-clippys-exit"
description: "A commit chained after clippy with ';' lands even when clippy fails -- gate it with && on the exit code (PIPESTATUS when grepping)"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-05"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - aafac248ee223941
---

## A commit chained after clippy with ';' lands even when clippy fails -- gate it with && on the exit code (PIPESTATUS when grepping)

In Stage 13 B5 (2026-10-01), two commits landed with clippy errors in them. The cause: lines like `cargo clippy ... | grep ...; git add -A && git commit ...`. The `;` runs the commit whatever clippy said, and the grep hides clippy's exit code.

Both were caught by reading the output and amended before any push.

**Why:** a commit that fails lint is a review round wasted, and CI fails on it.

**How to apply:**
- Run clippy alone, then commit only if its exit is 0: `cargo clippy ... 2>&1 | grep ...; [ "${PIPESTATUS[0]}" = 0 ] && git commit ...`.
- Or run clippy and commit in separate tool calls.
- Never `; git commit` after a verify step. Related: [[host-cargo-is-system-1-98-no-rustup]] (never pipe a verify step).

**And the tests, not only clippy** (#157 review fix, same day): a commit gated on clippy alone landed with a failing test. Gate on both: `clippy ... ; [ PIPESTATUS = 0 ] && cargo test ... ; [ PIPESTATUS = 0 ] && git commit`, or check the test tally before a separate commit call.

**It happened again in Stage 15 R2 (2026-10-04), twice in one hour**, both caught before any push: a commit after a clippy run whose failure scrolled past, and an `--amend` chained as `cargo test ... | grep ...; git add -A && git commit --amend`, where `&&` bound to the GREP's exit, not the tests'. What held: write the test output to a scratch log, take `rc=$?` from cargo itself, and run the commit only under `if [ $rc -eq 0 ]`. Never let a grep or a `;` stand between a verify step and the commit.

AGAIN 2026-10-05 (#192, 8781c265): `fmt --check && test && clippy; git add && git commit && git push`.
fmt failed, the `;` let the commit and the PUSH run anyway, and an unformatted commit went to the PR,
which needed a follow-up commit. The whole chain, push included, goes behind `&&`, with no `;` anywhere
before the commit.

*References: host-cargo-is-system-1-98-no-rustup*

*Observed 2026-10-01 (p2p-network-dev)*
