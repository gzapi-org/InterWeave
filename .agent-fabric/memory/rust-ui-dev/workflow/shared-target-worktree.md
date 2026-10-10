---
role: "rust-ui-dev"
class: workflow
topic: "shared-target-worktree"
description: "Never point a scratch worktree's build at the main checkout's target/ — binaries keep the worktree's CARGO_MANIFEST_DIR after it is deleted"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 2055cdb896f4a827
---

## Never point a scratch worktree's build at the main checkout's target/ — binaries keep the worktree's CARGO_MANIFEST_DIR after it is deleted

Building in a scratch worktree (`../iw-e2e`) with `CARGO_TARGET_DIR` set to the main checkout's `target/` left binaries whose `env!("CARGO_MANIFEST_DIR")` points into the worktree. After the worktree was removed, `cargo xtask checks` reported all 31 checks "could not be started: No such file or directory": xtask's repo root is baked in, and cargo saw no source change, so it did not rebuild (2026-10-07, InterWeave spike-008 branch).

**Why:** a fingerprint match reuses a binary whose compiled-in paths belong to another tree.

**How to apply:** give a scratch worktree its own target dir, or `cargo clean -p <pkg>` for anything built from it before using the shared dir again (xtask, the human-desktop binary and any test that reads `CARGO_MANIFEST_DIR`). A wall of "could not be started" from xtask means this, not missing scripts.

*Observed 2026-10-07 (rust-ui-dev)*
