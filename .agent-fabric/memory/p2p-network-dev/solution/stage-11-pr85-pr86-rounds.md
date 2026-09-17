---
role: "p2p-network-dev"
class: solution
description: "The review rounds of #85 (the vendored AutoNAT client) and #86 (the review sweep of merged code) — what each late round found and the fix that introduced the next"
tier: 2
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - 382859ad9a5354c3
---

## The review rounds of #85 (the vendored AutoNAT client) and #86 (the review sweep of merged code) — what each late round found and the fix that introduced the next

The late rounds of two Stage 11 PRs (the stage's state: [[stage-11-progress]]; the per-PR record: [[stage-11-review-rounds]]):

- #84 reached round 12; rounds 10-12 were the hysteresis rule, three times. See [[hysteresis-needs-counterexamples]].
- **#86 round 9 (`a0824f8` -> `6fb9ec7`)** — no P1, two P2 and one P3 that was a silent pass. (a) `("self.book", 6)` counted six of SEVEN book accesses: rustfmt wraps a long chain between receiver and field and `dial_candidates` is already written that way, so the pattern was blind to the likeliest next shape. Now `(".book", 7)`. (b) `record_failure` takes a TICKET — the split is two address-taking and four ticket-taking, and the two sibling guards contradicted each other. (c) The `outbound_gate.rs` guard used a bare `split_once`, so a second `canonical_for_peer(` call site below `mod tests` — Rust's conventional place for new code — was outside the production slice and passed. All three guards now share the same three protections, except the per-occurrence `#[cfg(test)]`-is-a-module check, which `connection_manager.rs` deliberately omits because it has two `#[cfg(test)]` non-module items counted as production.
- **#85 round 7 (`e004a81` -> `bda62b5`)** — no P1, one P2: the self-test's baseline asked only whether `cargo deny check advisories` exited non-zero, which an advisory-db outage satisfies as well as a real finding, while ~16 downstream assertions said "the baseline proved the database reachable" and classified exit 2 as failure rather than skip. It now requires the advisory to be NAMED. Also a class sweep: `printf … | grep -q` under `pipefail` can report non-zero for a MATCHING pipeline, which inverts a negative assertion's direction; 21 sites converted to `[[ "$out" == *needle* ]]`, plus the guard's own unreachable-database block.
- **#86 OPEN (2026-09-10, FIVE rounds, head `fd86b8e`)** — each round found a defect in the previous round's fix; see [[fix-both-ends-of-a-key-pair]]. Round 5: no P1, one P2 — the canonicalization guard's route table omitted `record_permanent_address_failure_unadmitted`, which keys the BOOK via `known.remove(address)`. Round 4 had DROPPED it for "reaching nothing", true of `learn_address` and the wrong question. **The guards' scope is now stated as every method that keys the book or the quarantine from a caller-supplied address**, which is wider than the set reaching `learn_address`; four mutation plants confirm both tables.
- **#85 round 4 (head `5eccc1c`)** — no P1, three P2. Two were comments measured a different way than the script measures: the domain-fn collision count was the comments-KEPT number and its lead examples occur only in comments the PR's own patch added; the advisory guard's accounting claim said "either direction" for a one-sided `shipped - rows` difference. Third was four missing assertions, including the guard's ONLY exit-0 path. **A reviewer's proposed mechanism was itself wrong** — deleting the severity filter does NOT surface an ignored advisory, because `cargo deny` emits no record for an id on its `ignore` list; what holds that assertion is the probe inheriting the repo `deny.toml`. Measure the mechanism before accepting a finding's reasoning.
- **#86 detail** — a REVIEW SWEEP, not a stage step: eight defects in already-merged code across four packages, one commit per root cause, off `3941ca8`. Findings: identity `restore()` could replace an established profile (split into `restore_new`/`restore_replace`); `load()` checked one inode by pathname and read another; `RecoveryRecord` bounded after Serde allocated; `PageLimits` zero ceiling ended a page walk; `u64` timestamps saturated into SQLite's `i64`; **address canonicalisation at `attempt_dial`** (the deferred follow-up below — now done); direct-v2 had two header parsers; `ConfigureBroadcast` committed more than it applied. Zero file overlap with #84 or #85.

*References:  "$out" == *needle* , fix-both-ends-of-a-key-pair, hysteresis-needs-counterexamples, stage-11-progress, stage-11-review-rounds*
