# Bounded ephemeral duplicate suppression

**Status:** Accepted

## Context

GossipSub already suppresses many duplicates, but local normalization and caller retries can still produce repeated presentations. Direct v2 also permits multiple EndpointIds under one PeerId, so the same 128-bit message ID used independently by two source endpoints must not collapse into one delivery.

## Decision

Maintain a runtime-local LRU/TTL cache with default 10,000 entries and 5-minute TTL.

Canonical keys:

```text
broadcast: (mode=broadcast, source_peer, channel, message_id)
direct:    (mode=direct, source_peer, source_endpoint, destination_selector[Explicit(id)|Default], message_id)
```

Direct caller retries reuse the same message ID. A positive direct entry stores the first resolved destination endpoint and **DirectContentFingerprintV1**, exactly the SHA-256 canonicalization specified in `contracts/ENDPOINTS.md` (domain-separated binary framing; absent media distinct from any present value; empty media invalid). Matching retries that pass current trust/structural/direct-ingress rate admission return the same acceptance/route without re-enqueueing even if the default later changes. A rate-limited retry may receive `overloaded` without deleting the prior positive entry. Same key with different content is a duplicate-ID conflict and is rejected. Rejected requests need not be positively cached. Persistence is prohibited.

To close the concurrent-duplicate race, direct admission also maintains a bounded **in-flight reservation map** keyed identically to the positive cache. The first request becomes owner; matching concurrent duplicates wait on/share that owner's eventual result and never run a second enqueue path. A concurrent request with the same key but different content fingerprint fails immediately as a duplicate-ID/content conflict. Default reservation limits equal direct request admission limits: 128 global / 8 per source PeerId, ceilings 512 / 32. Reservation exhaustion returns `Overloaded` locally / `overloaded` on the coarse direct wire. When the owner is rejected, all matching waiters observe the same rejection and the reservation is removed without creating a positive cache entry, so a later retry can succeed after route recovery.

**Waiter retention binds from the stage where admission yields.** The rule above — that a waiter's response channel is held until the owner's admission resolves — presumes an admission that can be interrupted while a reservation is held. It is not conditional on the deployment; it is conditional on that property of the admission path, and it takes effect in the first stage where admission acquires it, which is the local-client IPC boundary.

Until then a conforming implementation MAY treat the waiter branch as unreachable and MUST NOT answer it as though the owner had resolved. In a synchronous admission the owner acquires, resolves, enqueues and releases within one call, so no second request can observe an in-flight reservation: a duplicate arriving afterwards is a positive-cache hit, which is the path the rules above already govern. Answering an attached waiter from an absent cache entry — `overloaded`, say — is NOT conforming, because it reports exhaustion for a request that was admitted.

The **bound** is unaffected and remains mandatory in every stage. Waiters are charged against the same per-peer and global budgets as owners and released together, because SPIKE-002/A11 measured the unbounded form accumulating 39 waiters on one key with zero refusals — a memory-exhaustion path a peer controls. What this amendment scopes is when the channel must be HELD, never whether the accumulation must be bounded.

## Alternatives considered

No local dedup; persistent ledger; key direct messages only by peer/message ID; include timestamps in replay identity.

## Consequences

Local presentation is at-most-once only inside the bounded cache window. The same message ID from two different source endpoints on one PeerId remains independently deliverable. Cache eviction can allow a very late replay to present again.

## Security implications

Bounds prevent replay/flood state from growing without limit. `sent_at` is diagnostic only and does not become a freshness/authentication input.

## Operational implications

Expose duplicate-drop counters; do not log payload bodies. Endpoint dimensions should not become unbounded metric labels.

## Implementation implications

After current trust/structural/direct-ingress rate admission, direct parsing constructs the selector-aware key and content fingerprint before current default resolution. On positive hit with matching fingerprint, return the stored accepted resolved endpoint without re-enqueue. On fingerprint mismatch, reject. On miss, atomically acquire the bounded in-flight reservation; only its owner may resolve/admit/enqueue. Matching waiters receive the owner result. Successful owner completion atomically installs the positive entry before releasing waiters.

## Revisit conditions

Revisit if a higher-level protocol requires durable replay protection or cryptographically sequenced application messages.

## Amendments

Full notes: [`history/0019-amendments.md`](./history/0019-amendments.md).

| Date | Amendment | Effect |
|---|---|---|
| 2026-08-27 | Waiter retention binds from the stage where admission yields | A synchronous admission may treat the waiter branch as unreachable, but must not answer it as exhaustion; the reservation bound is unchanged |
