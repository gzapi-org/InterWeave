---
role: "p2p-network-dev"
class: workflow
description: "A review brief that restates the full PR range and asks for guard/count re-verification every round burns ~50k tokens per comment-only commit and manufactures fresh prose nits; scope it to the diff's hunks and forbid re-verifying settled…"
tier: 1
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - 5d816c9877813dbe
---

## A review brief that restates the full PR range and asks for guard/count re-verification every round burns ~50k tokens per comment-only commit and manufactures fresh prose nits; scope it to the diff's hunks and forbid re-verifying settled facts

**Scope a re-review brief to the diff's hunks, and tell the agent NOT to
re-verify anything prior rounds already measured.**

On InterWeave PRs #85/#86 the owner read the subagent token counts and said
the briefs were "likely too broad". Measured: six reviews in one session
came to ~1.26M subagent tokens, and the last two were **160k and 174k for
diffs of three and four COMMENT-ONLY commits** — about 50k tokens per commit
of prose, 15–20 minutes each.

The cause was in the brief, not the agent. Even briefs that said "review
ONLY the last three commits" still opened with `Full PR range:
3941ca8..HEAD (62 commits)` and then asked for:

- reimplementing all four structural guards' cut-and-count in Python over
  ten files — 90 (pattern, file) cells — every round;
- a tree-wide stale-prose sweep;
- verification of every count and distance in the whole range;
- substring-collision checks across all 21 patterns.

None of that can change when the diff touches only comments, and three
earlier rounds had already reported it clean.

**Why:** the waste is the smaller half. Re-reading settled paragraphs is
what *generates* the next round's findings — rounds 10, 11 and 12 each
objected to a new sentence in prose the previous round had just corrected,
while the code under it did not change once. A broad brief therefore feeds
the asymptote it is meant to close.

**How to apply.**

- Name the hunks, not the range: "review commits `X..Y`; these are the only
  hunks" — and do not restate the full PR range at all on a re-review.
- Say it explicitly: **"Do not re-verify the guard tables, the counts, or
  anything outside the diff — prior rounds measured them; assume them."**
- Give one job: for each claim the hunks make, check it against the source
  it describes and say true or false.
- Cap the reading: the files the hunks touch, plus whatever a cited claim
  points at. Nothing else.
- Keep "say so plainly if it is clean" — that is the outcome that lets the
  gate terminate.
- A comment-only round should cost ~20–40k, not ~170k. If an agent returns
  far more than that for a small diff, the brief was wrong.

**The one exception is the deliberate wide audit**
([[audit-agent-ends-review-whack-a-mole]]): after ~3 rounds finding the
same invariant, ONE uncontexted full-tree pass is the right move. That is a
once-per-class escalation, not the per-round default — which is the mistake
here: the audit shape was kept for every subsequent round.

This narrows, but does not cancel,
[[ask-reviewers-to-generalise-findings]]: generalise within the classes the
DIFF raises, rather than sweeping the repository for them.

Related: [[verify-subagent-coverage-not-just-findings]],
[[reviewer-declined-dispatch-opus]].

*References: ask-reviewers-to-generalise-findings, audit-agent-ends-review-whack-a-mole, reviewer-declined-dispatch-opus, verify-subagent-coverage-not-just-findings*
