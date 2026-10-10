---
role: "rust-ui-dev"
class: threads
topic: "stage-15-b4-pr181"
description: "Stage 15 B4 / PR #181 (merged 1c2f8dc2, 2026-10-04) -- desktop window, deny delta, what it left open for B5-B7"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 8891ca948ef8bc36
---

## Stage 15 B4 / PR #181 (merged 1c2f8dc2, 2026-10-04) -- desktop window, deny delta, what it left open for B5-B7

PR #181 merged 2026-10-04 as 1c2f8dc2: the winit + software renderer + AccessKit `desktop` feature in ui-slint, deny.toml delta (targets = Linux; Royalty-free exceptions; RUSTSEC-2026-0192 ttf-parser ignored -- reached via winit's Wayland decorations sctk-adwaita, NOT Slint's font stack), focus-gated reads via unstable-winit-030, signals installed before the facade asks for the lease, both lists scrollable with accessible-item-index/count. 8 work + 10 review fixes; blind review + 2 re-reviews + a judged bot round (conversation pane P2 fixed).

Carried, by name:
- B7: an e2e that reads the client's tree over AT-SPI (harness now forwards DBUS_SESSION_BUS_ADDRESS/AT_SPI_BUS_ADDRESS/XAUTHORITY; a missing bus is still silent).
- Window close is covered only by reading; SIGTERM and SIGINT are tested end to end.
- A long conversation list leaving the window's minimum height alone is unverified on a real winit window (testing backend fixes the size).
- signals.rs pre-existing: SIGTERM registered + SIGINT failing leaves SIGTERM swallowed; a second Ctrl-C during close is ignored.
- Slint reveals a focused item itself (i-slint-core publish_focus_item -> try_scroll_into_visible): no hand-written scroll logic needed.

Lessons: `git rebase --autosquash` without `--rebase-merges` flattens supply merges and rewrites suppliers' commits -- always pass --rebase-merges on a branch with folded supplies (see [[desktop-e2e-stale-binary]] for the other trap of this batch).

*References: desktop-e2e-stale-binary*

*Observed 2026-10-04 (rust-ui-dev)*
