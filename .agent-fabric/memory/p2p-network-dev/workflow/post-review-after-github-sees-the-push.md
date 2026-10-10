---
role: "p2p-network-dev"
class: workflow
topic: "post-review-after-github-sees-the-push"
description: "A review posted with post-review.sh seconds after git push can attach to the PR's previous head; check headRefOid first"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 9548ac7d3452ff50
---

## A review posted with post-review.sh seconds after git push can attach to the PR's previous head; check headRefOid first

On #190 (2026-10-04), `git push` and then `tools/gh/post-review.sh` in the same command pinned the
re-review to the PREVIOUS head (634d5101). GitHub had not yet moved the PR's head, so
`pr-review-status.sh` said "head reviewed? no" for 09dc7633, and the review had to be reposted.

**Why:** a review object takes the PR's head as GitHub knows it at the moment of posting, not the
local HEAD.
**How to apply:** after a push, confirm `gh pr view <n> --json headRefOid` equals
`git rev-parse HEAD` before posting a review, then read `pr-review-status.sh`'s "head reviewed"
line. A thread reply through pr-reply.sh also creates an unmarked review object at the head, which
is not coverage. See [[review-dispatch-shape-in-interweave]].

*References: review-dispatch-shape-in-interweave*

*Observed 2026-10-04 (p2p-network-dev)*
