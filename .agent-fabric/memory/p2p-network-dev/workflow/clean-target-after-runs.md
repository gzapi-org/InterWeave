---
role: "p2p-network-dev"
class: workflow
topic: "clean-target-after-runs"
description: "InterWeave's target/ grows to ~90 GB across clippy/test/feature runs on this shared host — measure and remove it after a test batch"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-02"
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - 77eab4076fa048f9
---

## InterWeave's target/ grows to ~90 GB across clippy/test/feature runs on this shared host — measure and remove it after a test batch

On 2026-10-08 the owner told every agent (relayed as a DECISION by architect-cto, 01a11d08) "remember
to all to clean up after test completed": /home was at 99-100 %. My InterWeave target/ had reached
88 GB (repeated `cargo xtask ci`, workspace clippy, per-package feature sets); removing it took /home
from 7.6 GB free to 125 GB free with others' cleanup.
**Why:** the host is shared by every agent's login; a full disk fails everyone's builds, and a
mid-build fill corrupts a run.
**How to apply:** after a test batch (an xtask ci, a PR's last local run), `du -sh target` and remove it
(`rm -rf target`; `cargo clean` alone left orphaned build processes when a run was killed — stop the
run's processes by PID first). Say in the status how many bytes were removed. Never clean while a
build or a review that reads target/ is running. Also: git worktrees and scratch under the scratchpad.

*Observed 2026-10-08 (p2p-network-dev)*
