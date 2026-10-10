---
role: "p2p-network-dev"
class: threads
topic: "stage-14-batch-6-ui-model-contract"
description: "Stage 14 batch 6 (ui-model) agreed surface with rust-ui-dev-01 and architect-cto's client-api ruling; replay relay seqs 10630, 10639, 10642, 10633 for full text"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 7c724b3fed0ec4ea
---

## Stage 14 batch 6 (ui-model) agreed surface with rust-ui-dev-01 and architect-cto's client-api ruling; replay relay seqs 10630, 10639, 10642, 10633 for full text

Branch develop-qzapp/p2p-network-dev-01/feat/stage-14-ui-model (off 3b7c6ffb, after #167 merged).

Layering (architect-cto seq 10633): (b2) new types-only crates/human/client-api holding the facade's vocabulary (OutboundStatus, SendProblem, SessionState, Connectivity, Origin, Received, ClientEvent...); RowId/AppMessageId moved to human-core (done, 9f4bdb9a), store re-exports. ui-model depends on client-api + human-core only (no rusqlite: P2 rule 2). architect supplies plan §17/layout text onto the branch; I must tell devex-tooling the human-layering guard's list grows by client-api.

ui-model contract (proposal 10630, amendments 10639, agreed 10642):
- pure + synchronous; outputs Intents toward the root (MarkRead, Keep only after read, Unkeep, Retry, Cancel, Send(key, draft), OpenLink only on activation + allowlisted, Reopen, RecheckStorage); actions(item) returns only legal intents.
- inputs: client_event, received, sent, pending_listed (restart content), unread_listed, kept_listed, read/kept/unkept results, send_refused(key, draft, SendError); composer state per conversation; MarkRead only from conversation_viewed(key, focused: true) — test "received while unfocused is still unread after restart".
- outputs: order by LOCAL time (received_at / created_at), sent_at_ms display only; stable ItemKey (row id or session id); untitled direct title = short PeerId + RouteLabel; channel items carry authenticated publisher; items carry raw source + render model + retention state (Unread/ReadEphemeral/Kept); connectivity() always present (Unknown never Offline) and session_notice() Option{Reconnecting, StorageDegraded, Refused(class)} with its resolving Intent; RAM cap evicting only read-unkept + terminal outbound, oldest first, tested at cap/cap+1; diagnostics() counts.
- ErrorClass covers SendProblem, SendError, SessionProblem (EndpointInUse own class), exhaustive.
- LabelKey closed enum; P6 test enumerates it; may_have_reached variants distinct keys.
- §13: one named test per bullet; bullets with no Stage-14 surface assert the absence and say so (no trust-mutating Intent; adversarial fixture raises no Intent without activation, none reaches admin/recovery). Accessibility bullet is ui-slint's (batch 8).
- dedup (origin, app_message_id) within session; across-restart limit stated in the test.

Related: [[pr167-armed-carry-to-batch-6]], [[batch-5-agree-client-contract-first]].
**Why:** build to agreed text; quote in PR body.
**How to apply:** client-api crate first, then ui-model.

*References: batch-5-agree-client-contract-first, pr167-armed-carry-to-batch-6*

*Observed 2026-10-02 (p2p-network-dev)*
