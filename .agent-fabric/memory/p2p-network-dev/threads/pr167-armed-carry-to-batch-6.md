---
role: "p2p-network-dev"
class: threads
topic: "pr167-armed-carry-to-batch-6"
description: "Stage 14 batch 5 PR #167 (transport-client facade, v6) armed 2026-10-02 at 2864597a after 4 re-review rounds; what is carried to batch 6"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - b48c694830e01a46
---

## Stage 14 batch 5 PR #167 (transport-client facade, v6) armed 2026-10-02 at 2864597a after 4 re-review rounds; what is carried to batch 6

#167 = Stage 14 batch 5: crates/human/transport-client (the facade), human-store v6 (pending_outbound.transport_message_id), StoreError::is_duplicate, pending_outbound_row, tests/human-retention client_half.rs (cases 1, 5, 14). Armed on the owner's word ("next" in answer to "shall I arm?") at 2864597a; MERGED as 3b7c6ffb on 2026-10-02; review rounds: R1 3 P2 + 6 P3, R2 1 P2 + 1 P3, R3 3 P3 (owner: fix all), R4 2 P3 fixed + 1 optional.

The contract lives in the crate README "The contract", agreed with rust-ui-dev-01 (relay seqs 10522/10534/10540, amended 10561/10567/10570, A5 10582/10585). architect-cto added TRANSPORT.md §Error model "Dispatch state" (bfae98fd): may_have_reached == the outcome-unknown class, incl. Internal.

Carried to batch 6 (ui-model):
- optional P3: tests/facade.rs second overflow phase loops 0..=cap with .ok(); make it 0..cap + .expect and comment that the broadcast is the row past the cap.
- pre-existing: ipc-client events(usize::MAX) loops try_recv until empty, not a strict snapshot under a flood.
- risks: a lease revoked on every claim is re-claimed at REOPEN's base delay forever (reopen_attempt resets on success); a non-medium SQL error during the pre-close take goes Reconnecting while drain would degrade.
- ui-model owes: dedup of a late duplicate by (origin, app_message_id) within a session, stating the across-restart limit (rust-ui-dev seq 10543); re-list unread on UnreadInStore.
- substrate (mine, carried by architect in §17): split PeerUnreachable into pre-dispatch and outcome-unknown.

Related: [[batch-5-agree-client-contract-first]], [[pr166-armed-carry-to-batch-5]].

**Why:** the next PR must start from these.
**How to apply:** batch 6 branches off main after #167 merges; agree ui-model's surface with rust-ui-dev-01 first (charter #79).

*References: batch-5-agree-client-contract-first, pr166-armed-carry-to-batch-5*

*Observed 2026-10-02 (p2p-network-dev)*
