---
role: "p2p-network-dev"
class: workflow
topic: "reviewer-declined-dispatch-opus"
description: "The review is the review class's blind review of the current head, posted with post-review.sh; there is no automated reviewer to request or wait for"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-05"
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
  - 2957e5cb05b94060
  - d4c06fb7437ba6ab
---

## The review is the review class's blind review of the current head, posted with post-review.sh; there is no automated reviewer to request or wait for

Every PR head in InterWeave gets the review class's blind review: a `code-review`
dispatch on `fable`, briefed with `fabric-review brief` (facts only, the exact
`base..head`), no isolation, description beginning `review` or `re-review`. It is
posted on the PR as a review object with `tools/gh/post-review.sh <n>`, and
`tools/gh/pr-review-status.sh <n>` counts it on its `blind reviews` line against the
head. A posted review of the current head with no open P1/P2 is the gate; a fix range
is re-reviewed before the arm. The automated reviewer is retired (InterWeave #114,
1cde0049, 2026-09-25): nothing asks `@codex review`, `--automated-only` does not
exist, and anything that installation still posts unasked is a finding to judge,
never coverage (#120 took one such P2 and fixed it, 795c0fc0).

**Why:** the section this replaces taught running two reviewers and reading a
non-zero `--automated-only` exit as the trigger; neither exists since #114.
**How to apply:** dispatch the review class on the finished head, post its report
with post-review.sh, fix and re-review, arm when the head's posted review has no
open P1/P2 (and, on a security boundary, the owner has spoken).

*Observed 2026-09-26 (p2p-network-dev)*
