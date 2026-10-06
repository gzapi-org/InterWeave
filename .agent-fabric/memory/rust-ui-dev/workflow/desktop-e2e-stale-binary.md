---
role: "rust-ui-dev"
class: workflow
topic: "desktop-e2e-stale-binary"
description: "desktop-e2e runs whatever human-desktop binary sits in target/ -- rebuild it before a package-only test run or a mutation check"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-05"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 5d049c10931e715d
---

## desktop-e2e runs whatever human-desktop binary sits in target/ -- rebuild it before a package-only test run or a mutation check

`tests/desktop-e2e/tests/common/mod.rs` `workspace_binary` resolves the app as `target/<profile>/human-desktop` next to the test executable and fails only if it is ABSENT, never if it is stale. `cargo test -p interweave-desktop-e2e-tests` does not rebuild `interweave-human-desktop`, so after editing the app a package-only run tests the OLD binary — and a mutation check silently "survives" (measured on #181, 2026-10-04: a SIGINT mutation stayed green until `cargo build -p interweave-human-desktop` ran first). `cargo test --workspace` (CI, `cargo xtask ci`) builds every bin, so CI is not affected.

How to apply: before any desktop-e2e run after an app edit, and before AND after each mutation, `cargo build -p interweave-human-desktop`. The harness is p2p-network-dev's; a staleness guard there would be an OBSERVATION to them, not my edit.

*Observed 2026-10-04 (rust-ui-dev)*
