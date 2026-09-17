---
role: "p2p-network-dev"
class: threads
description: "Stage 11 current state — which PRs merged (#86, #85), what #84 waits on, the arming rule for a PR whose late rounds find only comment defects, and what is next"
tier: 2
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - a7a5009bfed28ba6
---

## Stage 11 current state — which PRs merged (#86, #85), what #84 waits on, the arming rule for a PR whose late rounds find only comment defects, and what is next

Stage 11 (mandatory Internet connectivity) as of 2026-09-10:

**#85 MERGED 2026-09-10 21:39:00Z as `fa58c06`** — thirteen rounds. Rounds 9-13 found only false claims in the PREVIOUS round's comments; the fifth attempt at one paragraph was answered by DELETING it, which is what ended the cycle, and round 14 then reported "these hunks are clean". Seven prose nits carried to the follow-up branch, including a 111-char line reaching printed `--help`.

**#84's INTEGRATION IS A MERGE, NOT A REBASE** — its branch is published, so a rebase would need a force-push, which §9 forbids. Done 2026-09-10 as `db3dd39`: exactly the three predicted conflicts, every hunk resolved to #84's side AFTER checking each one individually rather than by the rule. What the per-hunk check found that the rule would have hidden: hunk 2 of AUTONAT.md is an EMPTY-vs-added line, and the side that changed is #84 — it DELETES `autonat_probes_inflight` from §9 (the base had it; nothing tracks in-flight probes since round 10 stripped the manager's probe-planning half), while both branches add `autonat_retests_total`. Also: resolve by EDITING THE MARKERS, never `git checkout --ours <file>`, which would discard main's auto-merged edits to the same file. All five post-resolution checks passed.

**#86 MERGED 2026-09-10 21:07:35Z as `f922e77`** — fourteen review rounds, zero unresolved threads, armed on a subagent-only gate (`pr-review-status.sh --automated-only` exit 1, eleven consecutive @codex usage-limit refusals). Rounds 9-14 found only defects in the PREVIOUS round's COMMENTS while the code stood still; round 12 was the exception and found a real gap (the policy's `record_address_failure` in no route table). **Follow-up branch owes six items**, all recorded on #86: an off-by-one ordinal and three over-general sentences in the canonicalization guards; a drift check comparing the two hand-maintained cross-crate route tables (nothing compares them, which is how round 12's gap survived); and `check_dependencies.sh:100`'s `grep -q`-at-end-of-pipe, which makes the same network-vs-violation decision over the whole of `cargo deny check`'s output in a script gating a required context.

**HEADS AS OF 2026-09-10 20:00Z:** #86 `6fb9ec7` (round 9 posted), #85 `bda62b5` (round 7 posted), #84 `f9f1671` (untouched since round 16; reviewed after its rebase, not before). `origin/main` = `3941ca8`. Conflict set #85↔#84 re-measured at these heads with `git merge-tree`: still exactly the three files below. @codex has now refused **seven** consecutive requests for usage limits across #85 and #86, so `pr-review-status.sh --automated-only` exits 1 every time and the opus subagent is the only coverage — recorded on both PRs with the exit code and head, per §9's terminating clause.

**ARM ON P1/P2-CLEAR, NOT ON P3-CLEAR.** Rounds 8 (#86) and 6 (#85) each found only defects in the PREVIOUS round's comment corrections. CLAUDE.md blocks arming on unresolved P1/P2, not on P3, so from round 9 / round 7 the policy is: fix P1/P2, record P3 prose on the PR, arm on the next P1/P2-clear review. Chasing P3s seeds the next round indefinitely.

**ORDER FOR THE THREE OPEN PRs (2026-09-10):** #86 is independent — re-measured with `git merge-tree` at heads `fd86b8e`/`5eccc1c`/`f9f1671`: zero file overlap with EITHER other PR, so it conflicts with nothing. #85 lands next. #84 lands after #85 MERGES and conflicts with it on exactly THREE files (re-measured, still three) — `AUTONAT.md`, `CONNECTIVITY.md`, `resource-limits.md`. **Take #84's side in all of them**: #85 annotates the wrong numbers, #84 replaces them, so #85's interim annotations are superseded on contact. The overlap GROWS every #85 round that touches a spec, so re-measure with `git merge-tree` rather than trusting any note. When resolving, CHECK — and these are now MEASURED rather than remembered, at heads `f9f1671`/`e30e2d7` against `main`: (1) `autonat_retests_total` appears once in AUTONAT.md on BOTH branches independently, so taking #84's side preserves it either way; (2) `probe timeout: 15 seconds` appears TWICE in main -- §4 line 61 (client evidence model) and §7 line 103 (server role) -- and #84 deliberately deletes the §4 client row, leaving exactly ONE occurrence, under §7 Server role. So the post-resolution check is: `grep -c 'probe timeout: 15 seconds'` == 1 AND that survivor is below the `## 7. Server role` heading. Two more server look-alikes live in `CONNECTIVITY.md` §6 and the schema. The merged head still needs its own review, and `pr-review-status.sh 84` already reports "head reviewed? no" BEFORE the rebase.

**Why:** the stage cannot close without steps 3–10 and the five remaining phase-B items; the NAT row is settled.
**How to apply:** before touching `SPIKES.md`/the plan, check #81 has merged; before step 3, check #80 has merged (it owns `ProfileConfig.transport`). See [[connectivity-constructor-gated-off]], [[stage-closure-needs-approval]].

The per-PR review-round record is [[stage-11-review-rounds]].

*References: connectivity-constructor-gated-off, stage-11-review-rounds, stage-closure-needs-approval*
