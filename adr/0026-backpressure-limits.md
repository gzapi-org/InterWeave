# Bounded queues and conservative resource limits

**Status:** Accepted

## Context

A network can produce data faster than Claude consumes it. Unbounded queues are a denial-of-service and crash risk.

## Decision

Adopt explicit defaults: 48 KiB payload; 128-byte ChannelId; 1024 backend/runtime events; 256 events per IPC client; 16 IPC clients; 64 outstanding commands/client; 128 global direct sends; 8 direct sends/peer; 4096 candidates; 16 addresses/peer. All operationally configurable within hard safety ceilings.

## Alternatives considered

unbounded async channels; persistent overflow spool; block the Swarm loop behind Claude; silently unlimited subscriptions.

## Consequences

Overload can drop realtime messages, consistent with best-effort semantics. Dedicated control capacity keeps degradation visible.

## Security implications

Bounds limit flooding/slow-consumer memory exhaustion. Per-peer concurrency/rate limits reduce one peer monopolizing resources.

## Operational implications

Metrics/counters show drops and saturation. Configuration tuning is deployment-specific but safe ceilings prevent accidental disabling of protection.

## Implementation implications

Use bounded Tokio channels/queues, early length checks, per-peer/global semaphores, and reserve control-event lane.

## Revisit conditions

Revisit defaults after load tests; changing defaults does not change the semantic contract as long as boundedness remains.
