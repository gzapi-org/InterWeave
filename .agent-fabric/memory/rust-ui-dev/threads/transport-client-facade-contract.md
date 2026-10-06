---
role: "rust-ui-dev"
class: threads
topic: "transport-client-facade-contract"
description: "The agreed caller-facing contract of crates/human/transport-client (Stage 14 batch 5), before its PR quotes it"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-05"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - bdeca224eeb2c072
---

## The agreed caller-facing contract of crates/human/transport-client (Stage 14 batch 5), before its PR quotes it

Agreed 2026-10-02 between rust-ui-dev-01 and p2p-network-dev-01 over GZCoord; built and MERGED in #167 (2026-10-02, 2864597a). The authoritative text is now `crates/human/transport-client/README.md` "The contract" on main — read it, not this summary.

Key points ui-model relies on:
- Outbound row status: Sending{attempts,next_retry_at,last_problem} / Unconfirmed{..,last_problem} / NeedsAttention{problem} (retry/cancel only) / Accepted{endpoint} / Published / Cancelled{may_have_reached}. No "failed" terminal. send() errs with no row on encode refusal or store failure.
- Transient retry unbounded: 1 s doubling, 5 min cap, deterministic jitter. Transport MessageId minted at commit, stored (schema v6), reused across restarts.
- SessionState: Ready / Reconnecting / StorageDegraded / Refused{EndpointInUse | NotAvailableToThisClient | Internal} / Closed; Refused leaves only via reopen(); EndpointLeaseChanged → Reconnecting.
- Connectivity: OnlineDirect/OnlineRelay/OnlinePartial/Offline + Unknown (never shown as Offline); no per-peer path until Stage 15.
- Inbound origin Direct{peer, asserted endpoint} | Channel{channel, publisher}; malformed inbound discarded, counted in `undecodable_discarded`.
- Residual dedup is ui-model's (session dedup on origin+app_message_id); the after-restart duplicate of a read-unkept message reappears as unread — a named limit; closable in Stage 15 with content-free dedup metadata (RETENTION §5), see [[stage-15-client-reading-notes]].

Amended 2026-10-02 from #167's review (seqs 10561, 10567, 10570):
- may_have_reached is a per-row fact, monotone until terminal, derived on load as attempts > 0 (attempt recorded before the transport call). Rows past Timeout/CancellationRaced/BackendUnavailable/ShuttingDown/PeerUnreachable/ProtocolViolation/Internal (TRANSPORT.md §Error model "outcome unknown" class, architect-cto bfae98fd) report Unconfirmed; Sending only when nothing went out or the remote said no (RouteUnavailable, Busy). On the in-process binding "no network path" mostly shows as "not confirmed".
- send refuses up front with no row: NotConfigured, InvalidEnvelope, TooLarge, store failure. A restarted row the config no longer allows → NeedsAttention(NotConfigured).
- NotConfigured comes only from the config check; transport EndpointNotRegistered/ChannelNotJoined = lost lease/join → re-open and retry as ServiceUnavailable (#167 23e019be).
- held_overflow (Diagnostics count): on a facade-initiated close, held inbound past one queue's worth is not handed over via drain — it is already unread in the store. So ui-model's unread view must be loaded from the store on (re)open, not built from drain alone. A5 (seqs 10582/10585): `ClientEvent::UnreadInStore { not_handed_over }` (coalesced, cumulative, pushed after the commit is durable) tells ui-model to re-list `unread_inbound`; merge with drained rows by store row id.
- Wall clock (caller-supplied fn) for persisted/wire times; inbound ordered by local received_at, never by peer-asserted sent_at_ms.
- Queue keeps the latest state per key; transient session transitions can collapse.

Related: [[stage-14-ui-batches-ownership]]

*References: stage-14-ui-batches-ownership, stage-15-client-reading-notes*

*Observed 2026-10-02 (rust-ui-dev)*
