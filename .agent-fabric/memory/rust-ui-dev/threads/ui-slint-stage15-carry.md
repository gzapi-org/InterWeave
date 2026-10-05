---
role: "rust-ui-dev"
class: threads
topic: "ui-slint-stage15-carry"
description: "What I inherit in crates/human/ui-slint at Stage 15 from PR #170 — the root's drain contract and carried P3s; read before writing apps/human-desktop's root or first touching ui-slint"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-05"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - b46f3c568de063b3
---

## What I inherit in crates/human/ui-slint at Stage 15 from PR #170 — the root's drain contract and carried P3s; read before writing apps/human-desktop's root or first touching ui-slint

From #170 (head 0b46e408, relay seq 10946, 2026-10-02):
- **Drain contract:** the composition root must call `View::take_events` repeatedly until it returns empty, applying each `DraftChanged` via `UiModel::draft_changed`, BEFORE returning to the event loop. The queue's 4*INPUT_CAP+3 bound holds only under that; stopping between takes lets one extra edit wait per conversation selected meanwhile. Root also calls take after every `render` (render sets the "viewed" flag without waking).
- Carried P3s (comment-only, on #170's last posted review): INPUT_CAP's doc omits the bound's failure case; a 108-char line in take_events' doc.
- F4 (mine): every item's status is a polite live region — risk of announcement flood on list insertion; decide (likely one summary live region) once an AccessKit adapter is in the graph.
- Also from Stage 14: drawing the markdown subset with activation-only links (U2d), Slint attribution duty, backend choice.

Related: [[ui-slint-surface-proposal]], [[stage-15-client-reading-notes]]

**Handover (relay seq 11002, 2026-10-03):** on the owner's direction the human-client line (crates/human/*, apps/human-*, tests/human-*, human_chat.rs) is mine; p2p-network-dev-01 moved to Track C (Claude channel bridge, Stage 16 / SPIKE-001). Stage 14 batches all on main (#166-#170, #170 merged 630d02d6). Stage marker still `stage-14-human-core-ui` at that point — stage close/open is architect-cto's and the owner's.
- **fontconfig-dlopen (d700cedb):** ui-slint names `i-slint-common` only to enable `fontconfig-dlopen`, so fontconfig is loaded at RUN time. apps/human-desktop owes a startup check that fontconfig loaded, with a clear failure for the person — a missing library must never mean a silent fontless or crashing window.
- p2p-network-dev keeps: transport, daemon + IPC, daemon.rs, tests/desktop-e2e/tests/common/, facade binding side; ask over GZCoord, supplied on my branch by the supplier rule.

*References: stage-15-client-reading-notes, ui-slint-surface-proposal*

*Observed 2026-10-03 (rust-ui-dev)*
