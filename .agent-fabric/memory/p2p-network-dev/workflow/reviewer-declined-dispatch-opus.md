---
role: "p2p-network-dev"
class: workflow
topic: "reviewer-declined-dispatch-opus"
description: "Correction to workflow/reviewer-declined-dispatch-opus.md: there is no automated reviewer; the review class's blind review, posted with post-review.sh, is the review"
tier: 1
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - 0fdbb0d12e948ad0
  - d4c06fb7437ba6ab
---

## Correction to workflow/reviewer-declined-dispatch-opus.md: there is no automated reviewer; the review class's blind review, posted with post-review.sh, is the review

Corrects `.agent-fabric/memory/p2p-network-dev/workflow/reviewer-declined-dispatch-opus.md`
(line 22 and the section it heads). Since InterWeave PR #114 merged (1cde0049,
2026-09-25) the automated reviewer is retired: nothing is requested with
`@codex review`, and `tools/gh/pr-review-status.sh` forwards to agent-fabric's
`runtime/github/pr-review-status.sh`, which takes no `--automated-only`.

The rule now: every PR head gets the review class's blind review (`code-review`,
briefed by `bin/fabric-review brief`), posted as a review object with
`tools/gh/post-review.sh <n>`; that posted review of the current head with no
open P1/P2 is the gate (CLAUDE.md §9 on main after #114).

**Why:** the slice taught running two reviewers and reading the automated one's
threads, and a non-zero `--automated-only` exit as the trigger; none of that
exists now.
**How to apply:** dispatch the review class on the finished head, post its report
with post-review.sh, arm when it reports no P1/P2.

*Observed 2026-09-25 (p2p-network-dev)*
