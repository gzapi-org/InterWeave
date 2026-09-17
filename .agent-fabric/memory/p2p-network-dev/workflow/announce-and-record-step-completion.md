---
role: "p2p-network-dev"
class: workflow
description: "When a canonical-plan step finishes, say so explicitly in the reply and update the stage-progress memory in the same turn"
tier: 1
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - e7ec043d3576eb54
---

## When a canonical-plan step finishes, say so explicitly in the reply and update the stage-progress memory in the same turn

**When a step of the canonical plan finishes, notify it and write it down.**
Owner instruction, 2026-09-06.

- **Say it in the reply, plainly and up front** — "step N is done" — not
  buried under the review narrative that produced it. A step landing is the
  fact the owner is tracking; the findings that got it there are detail.
- **Update [[stage-11-progress]] in the same turn**, with the date, the PR
  number, and anything that landed deliberately open. A step's completion is
  not derivable from the code or `git log`: the merge commit says a branch
  landed, not that a numbered step of a plan is satisfied.

**Why:** during Stage 11 step 2 the owner asked twice what stage and step the
work was at, and both times the answer had to be reconstructed by reading the
plan and grepping the tree. It should have been a fact already recorded.

This is NOT permission to flip stage status or write a `Met.` block —
[[stage-closure-needs-approval]] still governs that, and a step finishing is
not a stage closing.

*References: stage-11-progress, stage-closure-needs-approval*
