# transport-client

The human client's transport facade (plan §17 (1); the blueprint's `human-transport-client`). It is generic over the neutral `DataSessionBinding`, with the admin `Status` connection beside it, and it owns the client's half of retention:
- commit-pending, then send, then transport-terminal;
- drain, then commit-unread, then present;
- re-open with backoff when a binding's connection has ended;
- the degraded-storage reaction;
- the retry, which resends the stored bytes (ADR-0050 rule 7) under the transport `MessageId` stored with the row (ADR-0019; schema v6, `HUMAN-CHAT.md` §Compression).

It depends on `local-client-api`, `transport-api`, `human-store` and `chat-protocol`, and names nothing under `crates/transport/*`, no libp2p and no Slint.

**Current status:** active workspace member since Stage 14 batch 5, built and tested against `tests/local-client-fake`.

## The contract

The caller-facing surface was agreed with the client's role before it was built (relay seqs 10522, 10534 and 10540). Every `TransportError` maps exhaustively onto these types, and none of them claims more than the transport proved.

**Outbound status** (`OutboundStatus`):
- `Sending`: the facade retries on its own, without bound. The backoff starts at 1 s, doubles, caps at 5 min, and has deterministic per-row jitter.
- `Unconfirmed`: the remote may have accepted. The facade retries under the same transport id, which the receiver's dedup makes safe.
- `NeedsAttention`: no retry until the person acts. The row stays pending and durable; there is no "failed" terminal state.
- `Accepted`: bounded remote queue admission, never "read" or "seen".
- `Published`: published locally, not delivered to any recipient in particular.
- `Cancelled { may_have_reached }`.

`send` commits nothing, and returns an error, when the envelope is too large or the store cannot hold it.

**Send problems** (`SendProblem`), as `human-client-ui.md` §12 lists them. The transient ones are `NoNetworkPath`, `Busy`, `ServiceUnavailable` and `RouteUnavailable`.

**Session** (`SessionState`):
- `Ready`;
- `Reconnecting`;
- `Refused { SessionProblem }`, left only through `reopen`, so a refusal never loops;
- `StorageDegraded`: the lease is released and joins are suspended until a re-check finds the store healthy;
- `Closed`.

**Connectivity** (`human-client-ui.md` §7):
- `OnlineDirect`;
- `OnlineRelay`;
- `OnlinePartial`;
- `Offline`;
- `Unknown`, which is never shown as `Offline`.

**Inbound** (`drain`):
- A message is committed as unread before it is returned.
- A duplicate of a held row is not returned.
- A malformed envelope is discarded and counted per reason, and never stored (`HUMAN-CHAT.md` §Consumers).

**Driving:** poll-driven. `tick(now)` runs what is due on the caller's monotonic clock, and nothing is spawned. Events come out of one queue coalesced per row, session, connectivity and peer, latest wins; a terminal status or a session transition is never dropped.

## What it does not do

- **Read, keep and unkeep** stay on the store (`store_mut`).
- **Hiding a late duplicate is ui-model's job.** A duplicate that arrives after the first copy was read, and outside the transport's dedup window, is returned again; hiding it is ui-model's, within a session. After a restart it shows again as unread, and closing that would mean retaining read ids, which is a `RETENTION.md` question.
- **No per-peer path state.** Plan §17 (5) carries the per-peer path event to Stage 15.
- **A retry is deduplicated only inside the receiver's window.** A retry is deduplicated by the receiver only while the re-opened session holds the same endpoint lease, and only inside ADR-0019's window.
