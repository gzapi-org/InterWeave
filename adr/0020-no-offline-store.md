# No persistent offline message store in v1

**Status:** Accepted

## Context

A durable store changes security, retention, replay, storage quotas, deletion, ordering, and acknowledgement semantics. It is outside a transport-only v1.

## Decision

Do not write application messages to disk for later network or Claude delivery. When peers/Claude clients are offline, realtime messages may be missed.

## Alternatives considered

SQLite mailbox; append-only log; reuse peer cache as message store; hidden IPC replay buffer.

## Consequences

Architecture is simpler and privacy/storage risks are lower, but offline usability is intentionally limited.

## Security implications

No sensitive payload-at-rest database is created by default. Availability/reliability is lower.

## Operational implications

Restart behavior is predictable: reconnect and continue, no replay. Drop counters reveal slow/offline local consumers.

## Implementation implications

Do not serialize payload queues during shutdown. Peer cache stores addresses/observations only, never payloads.

## Revisit conditions

Revisit only through a new capability-backed durable-delivery design with retention, quotas, encryption-at-rest, and acknowledgement semantics.
