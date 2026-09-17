---
role: "p2p-network-dev"
class: workflow
description: "Run both reviewers, and READ the automated one's threads every round — a non-zero gate exit means \"this head uncovered\", never \"no review exists"
tier: 1
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - 0fdbb0d12e948ad0
---

## Run both reviewers, and READ the automated one's threads every round — a non-zero gate exit means "this head uncovered", never "no review exists

Both reviewers on every review; the blinded opus subagent alone when the
automated one refuses; never zero.

**A NON-ZERO `pr-review-status.sh --automated-only` exit means "this head
is not covered", NOT "no review exists."** Read the body and the
unresolved threads before concluding anything about what the automated
reviewer said.

**Then actually read them, every round.** On PR #78 the automated
reviewer posted a review on five consecutive heads while five subagent
rounds ran in parallel, and none of its five threads was opened until
round 6 — the gate kept exiting non-zero (correctly: it was reviewing
the previous head each time), and that was read as "nothing to see".

The cost was not duplication, it was coverage. The two reviewers find
different classes:

- the subagent, briefed on "can it report a PASS it did not earn", found
  structural defects and unfalsifiable claims;
- the automated one found **probabilistic and timing** defects the brief
  never pointed at — a classifier that infers a class from one
  coincident sample, a fixed sleep standing in for asynchronous
  readiness, a detached container reported as started before its
  listener binds.

Three of those had survived every subagent round. Where the two
disagree, that is signal; where only one speaks, that is the whole
finding.

**Why:** the gate answers a narrower question than "has anyone reviewed
this" — it answers "is THIS head covered by the recognised reviewer" —
and threads on an earlier head stay open and unread while every
indicator says uncovered.

**How to apply:** each round, after requesting a review, list the
unresolved threads (`gh api graphql` on `reviewThreads`) and read every
one, including those marked `outdated=true`. Answer each on the thread
with the commit that fixes it; resolve only what is actually done.
See [[assertions-that-cannot-fail]] and [[stage-11-progress]].

*References: assertions-that-cannot-fail, stage-11-progress*
