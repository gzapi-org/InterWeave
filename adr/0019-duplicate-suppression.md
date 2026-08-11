# Bounded ephemeral duplicate suppression

**Status:** Accepted

## Context

GossipSub already deduplicates internally, but a backend-neutral local boundary avoids duplicate Channel events across reconnect/protocol quirks and direct retries.

## Decision

Maintain a runtime-local LRU/TTL cache keyed by `(source_peer,message_id,mode/channel context)` with default 10,000 entries and 5-minute TTL. Direct caller retries reuse message ID. Persistence is prohibited.

## Alternatives considered

No runtime dedup; persistent message ledger; unbounded set; content hashing as identity.

## Consequences

Duplicates outside the TTL/window can reappear. Memory is predictably bounded. Message IDs remain opaque and payload agnostic.

## Security implications

Replay within the cache window is suppressed but this is not a cryptographic anti-replay protocol. Attackers cannot force unbounded memory growth.

## Operational implications

Counters expose duplicate drops/evictions. Restart clears history by design.

## Implementation implications

Use bounded cache with monotonic expiry; never persist it as workflow/message history.

## Revisit conditions

Revisit if measured legitimate retry windows require different TTL/size or if end-to-end replay defense becomes a requirement.
