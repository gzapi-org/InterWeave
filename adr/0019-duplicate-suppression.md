# Bounded ephemeral duplicate suppression

**Status:** Accepted

## Context

GossipSub already deduplicates internally, but a backend-neutral local boundary avoids duplicate Channel events across reconnect/protocol quirks and direct retries.

## Decision

Maintain a runtime-local LRU/TTL cache keyed canonically by `(mode, source_peer, channel_or_none, message_id)` with default 10,000 entries and 5-minute TTL. For direct messages `channel_or_none = None`; for broadcast it is the logical ChannelId. Direct caller retries reuse the same message ID. Persistence is prohibited.

## Alternatives considered

No runtime dedup; persistent message ledger; unbounded set; content hashing as identity.

## Consequences

Duplicates outside the TTL/window can reappear. Memory is predictably bounded. Message IDs remain opaque, exactly 128 bits in v1, and payload agnostic. The mode/channel context prevents a coincidentally reused ID from collapsing distinct direct and broadcast/channel deliveries.

## Security implications

Replay within the cache window is suppressed but this is not a cryptographic anti-replay protocol. Attackers cannot force unbounded memory growth.

## Operational implications

Counters expose duplicate drops/evictions. Restart clears history by design.

## Implementation implications

Use bounded cache with monotonic expiry; never persist it as workflow/message history.

## Revisit conditions

Revisit if measured legitimate retry windows require different TTL/size or if end-to-end replay defense becomes a requirement.
