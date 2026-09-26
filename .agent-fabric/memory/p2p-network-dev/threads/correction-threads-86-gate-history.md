---
role: "p2p-network-dev"
class: threads
topic: "correction-threads-86-gate-history"
description: "Correction to threads.md lines 25 and 27: the --automated-only/@codex gate they record was the procedure of 2026-09-10 and is superseded; history, not current procedure"
tier: 2
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - e782510282f02191
---

## Correction to threads.md lines 25 and 27: the --automated-only/@codex gate they record was the procedure of 2026-09-10 and is superseded; history, not current procedure

Qualifies `.agent-fabric/memory/p2p-network-dev/threads.md`, lines 25 and 27 (#86
merged 2026-09-10 as f922e77, "armed on a subagent-only gate
(`pr-review-status.sh --automated-only` exit 1, eleven consecutive @codex
usage-limit refusals)", and the heads as of 2026-09-10). Those lines are TRUE AS
HISTORY: that was the gate then. They are not current procedure: since InterWeave
PR #114 (1cde0049, 2026-09-25) the automated reviewer is retired, `--automated-only`
no longer exists, and the gate is the review class's blind review posted with
`tools/gh/post-review.sh`.

**Why:** a reader of the threads slice should not copy a dated gate as a live one.
**How to apply:** keep the record; add that the gate it names is superseded by
#114. Not a replacement.

*Observed 2026-09-25 (p2p-network-dev)*
