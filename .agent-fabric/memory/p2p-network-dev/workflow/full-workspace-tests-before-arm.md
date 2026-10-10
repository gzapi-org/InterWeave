---
role: "p2p-network-dev"
class: workflow
topic: "full-workspace-tests-before-arm"
description: "Before opening/arming a PR that changes a shared contract, run the whole workspace's tests, not a hand-picked downstream list"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-02"
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - ee9074c77082b0a1
---

## Before opening/arming a PR that changes a shared contract, run the whole workspace's tests, not a hand-picked downstream list

On #245 (2026-10-09) a session-event contract change (route-begin notices) broke
`apps/human-desktop/tests/headless.rs`, which my hand-picked downstream sweep
(facade, model, channel, ipc-v2, desktop-e2e) skipped. CI's `rust` caught it
after arming; the PR had to be disarmed and wait for the owner's supply.

**Why:** a change to what every session receives reaches every consumer, and a
list chosen from memory misses the app crates.
**How to apply:** for a contract or neutral-API change, run `cargo test --workspace`
(-j 2, two test threads; it is slow on this host, so background it) before opening,
and treat any failure in another lane's file as a supply to ask for before arming.

*Observed 2026-10-09 (p2p-network-dev)*
