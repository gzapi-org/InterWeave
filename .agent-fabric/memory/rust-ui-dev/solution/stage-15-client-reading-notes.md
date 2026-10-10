---
role: "rust-ui-dev"
class: solution
topic: "stage-15-client-reading-notes"
description: "What the remit's reading list (ADR-0039/0040, clients/human/*, plan §17-§18) means for the desktop client work from Stage 15 — the non-obvious points"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 9525fca18803f8af
---

## What the remit's reading list (ADR-0039/0040, clients/human/*, plan §17-§18) means for the desktop client work from Stage 15 — the non-obvious points

Read 2026-10-02 at main 233ece2 (remit landed in #163). Stage marker then: `stage-14-human-core-ui`.

- **The after-restart duplicate is closable inside the contract.** RETENTION.md §5 allows content-free metadata "for ... bounded duplicate suppression", so a bounded table of read (origin, app_message_id) pairs — no bodies — can stop a late re-send of a read-unkept message reappearing as unread. I earlier told p2p-network-dev it was "a RETENTION.md question"; it is not, it is store-schema work (mine from Stage 15). Constraint: such metadata must never suffice to reconstruct a deleted body (§5), and deletion must remove app-owned indexes/caches too (§8).
- **Shadow archives are forbidden** (RETENTION §8): logs, crash reports, notification DBs, OS backup, search indexes. Applies to Slint/desktop notification integration and any a11y/text-search caching.
- **Pending outbound is excluded from any backup** (§6), and Android system backup excludes the whole store.
- **Migration failure → recovery/read-only/export mode, never identity regeneration** (STATE.md).
- **ADR-0040:** closing the UI releases its endpoint lease but does not stop the daemon; desktop never links transport-libp2p; admin and data are separate sockets even in one executable.
- **ADR-0039:** Slint is revisited only on accessibility, platform integration, performance, licensing or store failure; Android JVM shim limited to lifecycle/notification/Keystore glue — no routing/trust/parsing/persistence there.
- **HUMAN-CHAT rendering duties for ui-slint:** links only `https`/`mailto`; raw HTML literal; remote images never auto-fetched (user-triggered placeholder); block and inline nesting ≤16, tables ≤256 rows/32 cols else plain-text fallback; raw source always viewable; `sent_at_ms` display-only; on broadcast `from_endpoint` is an unauthenticated hint.
- **Stage 15 (plan §18)** activates `apps/human-desktop`; required desktop E2E list there (shared daemon with Claude, unread persistence, read-unkept evaporation, Keep, outbox restart, daemon reconnect, admin/data separation, storage-failure degrade). Carried in from Stage 14: real process-kill restart, trust read + §13 trust-mutation bullet, `server_state` surfacing and per-peer path event (contract amendments, not mine to write), and `ui-slint` + §13 accessibility bullet if Stage 14 closes without them.

Related: [[ui-model-surface-proposal]], [[transport-client-facade-contract]]

*References: transport-client-facade-contract, ui-model-surface-proposal*

*Observed 2026-10-02 (rust-ui-dev)*
