---
role: "rust-ui-dev"
class: workflow
topic: "render-brief-before-dispatch"
description: "fabric-review brief must render successfully BEFORE a review is dispatched; never dispatch in the same parallel batch as the render"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - fe1d5e6eb0cc95c7
---

## fabric-review brief must render successfully BEFORE a review is dispatched; never dispatch in the same parallel batch as the render

Render the review brief (`fabric-review brief <yaml> > brief.md`) and check it succeeded, then dispatch the review in a LATER step -- never in the same parallel tool batch.

**Why:** twice on 2026-10-07 (#214's store range, then the trust supply's fix range) the brief tool refused a verdict-shaped objective ("names what was fixed"), `>` had already truncated brief.md, and the review agent dispatched alongside it started against an empty brief and had to be stopped.

**How to apply:** an objective states only facts -- the PR, the files/packages the range changes -- never "the fixes for X" or what the change achieves; put what must be true in `requirements`. Run the render, confirm a non-zero line count, then dispatch.

*Observed 2026-10-07 (rust-ui-dev)*
