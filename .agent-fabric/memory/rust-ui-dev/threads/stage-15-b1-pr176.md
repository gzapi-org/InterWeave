---
role: "rust-ui-dev"
class: threads
topic: "stage-15-b1-pr176"
description: "Stage 15 B1 = PR #176 (branch develop-qzapp/rust-ui-dev-01/feat/stage-15-core): what it holds, its review history, and lessons for later batches"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - ed56458965bc26bb
---

## Stage 15 B1 = PR #176 (branch develop-qzapp/rust-ui-dev-01/feat/stage-15-core): what it holds, its review history, and lessons for later batches

PR #176 (opened 2026-10-03): app-core (FacadeSide/ModelSide/Command/Update/Failure/Problem), ui-model G3 + Keep{item,from} + ViewEvent + copy_gone + composer edit revisions (send_pressed), store unkeep → Option<ReadEphemeral> (Q6 re-keep), ui-slint re-export + #170 P3s. 9 work commits + 5 review fixes. Gate met at 8f2773ff (blind review + 3 re-reviews, 2 bot threads judged P2/P3 and fixed by edit-revision rule). Persistence change → needs owner's word to arm.

Lessons:
- Comparing values cannot tell edits apart: use an edit revision recorded at the press (bot threads A/B on #176).
- A test asserting "not offered" must first show it WAS offered (control) — my first CopyGone test was vacuous until mutation showed it.
- Local tooling: cargo-deny and cargo-machete live in ~/.cargo/bin, NOT on PATH — export PATH="$HOME/.cargo/bin:$PATH" before `cargo xtask ci`.
- Not yet addressed (noted in reviews): two model items for one kept row after DEDUP_CAP eviction (stale Unkeep); a second Send while one is in flight is dropped silently (text now survives) — batch 2 UX.

Related: [[stage-15-rulings]], [[ui-slint-stage15-carry]]

*References: stage-15-rulings, ui-slint-stage15-carry*

*Observed 2026-10-03 (rust-ui-dev)*
