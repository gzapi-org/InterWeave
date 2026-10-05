---
role: "p2p-network-dev"
class: threads
topic: "correction-threads-86-gate-history"
description: "Correction to threads/threads-carried-2026-09-17.md: the --automated-only/@codex gate its #86 paragraph records was the procedure of 2026-09-10, superseded by #114; history, not current procedure"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-05"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 6f625252ec9994aa
  - e782510282f02191
---

## Correction to threads/threads-carried-2026-09-17.md: the --automated-only/@codex gate its #86 paragraph records was the procedure of 2026-09-10, superseded by #114; history, not current procedure

Qualifies `.agent-fabric/memory/p2p-network-dev/threads/threads-carried-2026-09-17.md`
(the drain carried the old `threads.md` there): its paragraph on #86 (merged
2026-09-10 as f922e77, "armed on a subagent-only gate (`pr-review-status.sh
--automated-only` exit 1, eleven consecutive @codex usage-limit refusals)") and its
"HEADS AS OF 2026-09-10" paragraph. Both are TRUE AS HISTORY: that was the gate then.
They are not current procedure: since InterWeave PR #114 (1cde0049, 2026-09-25) the
automated reviewer is retired, `--automated-only` no longer exists, and the gate is
the review class's blind review posted with `tools/gh/post-review.sh`.

**Why:** a reader of the threads slice should not copy a dated gate as a live one.
**How to apply:** keep the record; read the gate it names as superseded by #114.

*Observed 2026-09-26 (p2p-network-dev)*
