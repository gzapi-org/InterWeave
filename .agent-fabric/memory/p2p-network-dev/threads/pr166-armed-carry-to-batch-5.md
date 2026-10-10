---
role: "p2p-network-dev"
class: threads
topic: "pr166-armed-carry-to-batch-5"
description: "Stage 14 first PR #166 (batches 2+4+3) armed 2026-10-02 at 7fcd8a85; three P3s and one risk carried to the batch-5 PR"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 7ea3357453cd0b47
---

## Stage 14 first PR #166 (batches 2+4+3) armed 2026-10-02 at 7fcd8a85; three P3s and one risk carried to the batch-5 PR

PR #166 (Stage 14 batches 2+4+3, 15 work commits incl. architect-cto's 75a03f6a contract supply) armed on the owner's word after re-review 2 at 7fcd8a85 found no P1/P2. MERGED as af90e4c2 on 2026-10-02.

Carried by name to the next Stage 14 PR (batch 5, transport-client), listed in rr2 posted on #166:
1. crates/human/store/src/schema.rs: "One table's complete expected shape." doc line sits on the `ForeignKey` alias, not `TableShape`.
2. tests/human-chat/tests/render.rs `nesting_17_and_a_33_column_table_fall_back_without_rejecting_the_envelope` lacks an inline-17 source (HUMAN-CHAT.md:117 now names inline depth 17).
3. envelope.rs `MAX_TABLE_ROWS` doc says "rows"; the contract says body rows, header excluded.
Risk C: storage_mechanics cascade test enables foreign_keys on its own raw connection, not through store.rs's pragma (no contact-delete API yet).

Ruled by architect-cto (seq 10435): inline nesting is the CONTRACT's bound, 16, counted apart from block nesting; a bound a peer's bytes can defeat into an abort is never a renderer's own. Related: [[stage-14-batch-order]].

**Why:** these are the open items the next PR must close first.
**How to apply:** start batch 5's branch off main after #166 merges; fold these three fixes in as its first commits.

*References: stage-14-batch-order*

*Observed 2026-10-01 (p2p-network-dev)*
