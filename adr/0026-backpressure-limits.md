# Bounded queues and conservative resource limits

**Status:** Accepted; endpoint-routing limits added by ADR-0030.

## Context

Network producers can outrun local consumers. Unbounded queues are denial-of-service risks. Model B adds endpoint configuration/directory state and requires direct acceptance to reflect target endpoint queue admission.

## Decision

Keep 48 KiB payload and 128 KiB IPC JSON-body ceilings; 128-byte ChannelId; add 64-byte EndpointId, default 16/max64 configured endpoints, default 16/max32 advertised endpoints, one endpoint lease per IPC client, short-lived endpoint-directory cache. Existing queue/client/direct/discovery bounds remain.

Direct inbound messages are rejected as overloaded before `AcceptedV2` when the resolved endpoint queue is full. Broadcast retains bounded best-effort local drop behavior.

## Alternatives considered

Unbounded channels; persistent overflow spool; acknowledge direct before local queue; all-client direct fan-out; unlimited endpoint directory; reduce payload just to fit smaller IPC.

## Consequences

Direct endpoint routing reduces local memory amplification versus v1 fan-out. Endpoint presence/catalog state stays bounded.

## Security implications

Bounds limit network/local endpoint probing and slow-consumer exhaustion. Early length/EndpointId validation prevents attacker-directed allocation.

## Operational implications

Counters show endpoint lease conflicts, route/no-route/overload, directory bounds, and ordinary queue saturation.

## Implementation implications

Use bounded channels/semaphores and fixed directory caps. Golden fixtures prove max payload plus endpoint metadata fits 131072-byte IPC v2 body.

## Revisit conditions

Only with measured evidence and compatibility/security review.
