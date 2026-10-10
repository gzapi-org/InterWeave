---
role: "p2p-network-dev"
class: workflow
topic: "carry-commits-count-as-work"
description: "The gate's work count is authoritative; arm without asking as soon as pr-gate.sh shows 8 work commits; below 8, add work to the batch instead of asking"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - b7be45b905043096
---

## The gate's work count is authoritative; arm without asking as soon as pr-gate.sh shows 8 work commits; below 8, add work to the batch instead of asking

The owner's ruling of 2026-10-07, on #216: "the tool are right merge as 8 work commit are in". An earlier note of mine said the gate miscounts commits that carry an earlier review's findings. The owner rejected that reading.

**Why:** pr-gate.sh's classification is the rule. A commit whose message names a review counts as a fix. The owner wants batches built up to the threshold and merged without a question, not counts argued case by case.

**How to apply:**
- Count with `tools/gh/pr-gate.sh <n>` and nothing else.
- At 8 or more work commits, with the review gate met (a blind review of the head, no open P1 or P2), arm at once with arm.sh. Do not ask.
- Below 8, keep adding the next backlog items to the same branch until the gate shows 8, then arm.
- Do not stop to ask the owner while a batch is short.

Related: [[arming-rule-by-pr-commit-count]], [[arm-through-arm-sh]].

*References: arm-through-arm-sh, arming-rule-by-pr-commit-count*

*Observed 2026-10-07 (p2p-network-dev)*
