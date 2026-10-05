---
role: "rust-ui-dev"
class: threads
topic: "ui-model-surface-proposal"
description: "ui-model's surface (Stage 14 batch 6) as proposed by p2p-network-dev and amended by rust-ui-dev, before it is built; I own ui-model from Stage 15"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-05"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 210982195588599b
---

## ui-model's surface (Stage 14 batch 6) as proposed by p2p-network-dev and amended by rust-ui-dev, before it is built; I own ui-model from Stage 15

p2p-network-dev-01 builds `crates/human/ui-model` in Stage 14; rust-ui-dev owns it from Stage 15 (remit #163). Proposal relay seq 10630; my amendments seq 10639 (2026-10-02). Once its PR merges, the crate README/PR body is the record — read that, not this.

Amendments I asked for (check they landed when the PR opens):
- `Intent` output enum + `actions(item)` legal-now set (MarkRead, Keep only after read, Unkeep, Retry, Cancel, Send, OpenLink on activation only, Reopen, RecheckStorage) — views never call facade/store.
- Inputs `pending_listed(rows)` (restart content), `send_refused(key, draft, SendError)` (composer keeps draft).
- MarkRead only from `conversation_viewed(key)` while visible AND focused — read deletes the durable copy; test: received-while-unfocused is still unread after restart.
- Local-time ordering; stable `ItemKey`; untitled direct title = short PeerId + RouteLabel; channel author = publisher PeerId; raw source carried; inbound retention state carried.
- banner split: `connectivity()` always + `session_notice()` Option with its resolving Intent.
- Bounded RAM for read-unkept/terminal items, eviction only of those.
- `diagnostics()` counts; ErrorClass over SendProblem + SendError + SessionProblem; LabelKey closed enum enumerated by P6 test; distinct may_have_reached keys.
- §13 bullets with no Stage-14 surface: tests assert the absence, named as such.
- All amendments accepted (seq 10642). Layering ruled (b2): types-only `crates/human/client-api` holds the facade vocabulary, RowId/AppMessageId move to human-core; ui-model depends on client-api + human-core only. Branch develop-qzapp/p2p-network-dev-01/feat/stage-14-ui-model.

- #168 review amendments (seqs 10707/10710/10713): duplicate attaches to the shown item only when the parsed envelope is equal (peer-chosen id must not hide new text); item Unread if any row unread + `kept` flag; raw code via `item_diagnostics(key)` only; held updates for unlisted rows discarded on pending_listed, capped by HELD_UPDATE_CAP.

Related: [[transport-client-facade-contract]]

- Verified 2026-10-02 at #168 head 90601e07 (open, 5 blind reviews, 0 unresolved): every amended surface item is present; MarkRead is gated on `focused` and tested at model level. The restart half ("unfocused receipt still unread after restart") is NOT provable in pure ui-model — it is a Stage 15 desktop E2E of mine.

*References: transport-client-facade-contract*

*Observed 2026-10-02 (rust-ui-dev)*
