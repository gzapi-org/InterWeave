# Bounded queues and conservative resource limits

**Status:** Accepted

## Context

A network can produce data faster than Claude consumes it. Unbounded queues are a denial-of-service and crash risk. IPC framing must also have enough space for the configured maximum transport payload after representation overhead.

## Decision

Adopt explicit defaults: 48 KiB payload hard ceiling; **128 KiB IPC JSON body ceiling**; 128-byte ChannelId; 1024 backend/runtime events; 256 events per IPC client; 16 IPC clients; 64 outstanding commands/client; 128 global direct sends; 8 direct sends/peer; 4096 candidates; 16 addresses/peer. Operational limits may be configured within hard safety ceilings; the effective configured payload limit is reported through `TransportCapabilities.max_payload_bytes`.

The IPC frame ceiling is not operator-tunable in v1. It must always be large enough to encode the transport hard-ceiling payload plus bounded envelope metadata in both directions.

## Alternatives considered

unbounded async channels; persistent overflow spool; block the Swarm loop behind Claude; silently unlimited subscriptions; lower payload to fit 64 KiB JSON; keep 64 KiB and rely on typical smaller payloads.

## Consequences

Overload can drop realtime messages, consistent with best-effort semantics. Dedicated control capacity keeps degradation visible. IPC memory reservations must account for a 128 KiB per-frame maximum even though normal frames are much smaller.

## Security implications

Bounds limit flooding/slow-consumer memory exhaustion. Per-peer concurrency/rate limits reduce one peer monopolizing resources. Early length validation prevents allocation from attacker-declared oversized frames.

## Operational implications

Metrics/counters show drops, frame-too-large rejects, and saturation. Configuration tuning is deployment-specific but safe ceilings prevent accidental disabling of protection.

## Implementation implications

Use bounded channels/semaphores; validate payload and frame sizes before large allocation; reserve control capacity. Golden fixtures and boundary tests must prove 49,152-byte payloads fit under the 131,072-byte JSON-body frame limit with maximal v1 metadata.

## Revisit conditions

Revisit hard ceilings only with measured memory/throughput evidence and a transport/IPC compatibility review.
