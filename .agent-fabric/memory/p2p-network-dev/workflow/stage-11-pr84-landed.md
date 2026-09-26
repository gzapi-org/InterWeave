---
role: "p2p-network-dev"
class: workflow
topic: "stage-11-pr84-landed"
description: "A review loop where each fix round finds one comment-level defect in the previous fix ends by arming on P1/P2-clear with the comment-only delta recorded on the PR"
tier: 1
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 3043c1ab50bff634
---

## A review loop where each fix round finds one comment-level defect in the previous fix ends by arming on P1/P2-clear with the comment-only delta recorded on the PR

Stage 11 steps 3–5 (PRs #84, #88, #89, #93, #94, #96; merged 2026-09-17/18) each ran
review loops where every fix round found one comment-level defect in the previous
round's fix (on #84, rounds 18→19→a62815e). What ended the loop was arming once a
round was P1/P2-clear, with the remaining comment-only findings recorded on the PR and
carried as the next step's first commits. #88 took two rounds because its first fix
changed one sentence and left three in the same amendment saying the opposite.

**Why:** a fix to prose is itself prose a reviewer can find stale, so the loop does
not converge on its own; each round costs a full blind review.

**How to apply:** grep for the old behaviour's WORDS (e.g. "two knobs", "adapter
sets"), not your own new phrasing, before pushing a prose fix; once a round is
P1/P2-clear, record the P3s on the PR, carry them, and arm. The step records
themselves are in git and the plan's `Met.` blocks, not here.

*Observed 2026-09-25 (p2p-network-dev)*
