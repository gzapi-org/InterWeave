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
