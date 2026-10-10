---
role: "p2p-network-dev"
class: workflow
topic: "review-targets-under-home"
description: "Review subagents' CARGO_TARGET_DIR goes under /home, never /var/tmp (shared 40G root fs filled on 2026-10-07); clean finished review dirs at once"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 5ac647a257bdfe4c
---

## Review subagents' CARGO_TARGET_DIR goes under /home, never /var/tmp (shared 40G root fs filled on 2026-10-07); clean finished review dirs at once

On 2026-10-07 develop-qzapp's root filesystem (/, 40G, which holds /var/tmp for every account) reached 0 bytes free.
- The cause was three #215 review subagents' cargo target directories under /var/tmp: 4.5G, 3.9G and 1.8G.
- Every account's tests, gpg, git and sockets failed with ENOSPC. python-dev-01 reported it at relay seq 16867.
- The review hook refuses `rm` to a reviewer, so the directories outlive the review.

**Why:** /var/tmp is shared and small. /home has about 140G free.

**How to apply:**
- In every review dispatch brief, tell the reviewer to put any CARGO_TARGET_DIR under /home/p2p-network-dev-01/ (for example a `review-targets/` folder there).
- When a review finishes, remove its directory yourself. These are this account's own build caches, and the hygiene rule makes cleaning them the session's job.

Related: [[release-target-after-test-runs]].

*References: release-target-after-test-runs*

*Observed 2026-10-07 (p2p-network-dev)*
