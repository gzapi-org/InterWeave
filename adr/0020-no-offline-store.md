# No persistent offline message store

**Status:** Accepted

## Context

A durable transport store changes security, retention, replay, quotas, deletion, ordering, and acknowledgement semantics. Endpoint addressing makes the temptation to queue for an offline human/Claude route especially strong.

## Decision

Do not write application messages to disk for later network, endpoint, Claude, or human-client delivery. If a target EndpointId has no active lease, direct v2 returns `no_route` and does not accept/store the message.

A human application may persist its own local conversation history **after** it receives/sends content. That store is outside transport and does not create remote offline delivery.

## Alternatives considered

SQLite mailbox; append-only log; reuse peer cache; hidden IPC replay buffer; per-endpoint spool.

## Consequences

Architecture stays simple/private but endpoints must be online for realtime direct delivery.

## Security implications

No default payload-at-rest transport database. Application-owned history has its own security policy outside this architecture.

## Operational implications

Restart/reconnect has no missed-message replay. Endpoint availability and no-route counters make behavior visible.

## Implementation implications

Do not serialize payload queues during shutdown. EndpointRegistry persists config only, not messages/leases. Peer cache and endpoint-directory cache never contain payloads.

## Revisit conditions

Only through a new capability-backed durable-delivery design with retention, encryption-at-rest, quotas, and acknowledgement semantics.
