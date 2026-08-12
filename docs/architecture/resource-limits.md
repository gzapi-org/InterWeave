# Resource limits and backpressure

Default values are conservative architecture targets, not performance promises.

| Resource | Default | Hard architectural ceiling |
|---|---:|---:|
| application payload | 48 KiB | 48 KiB transport contract |
| ChannelId | 128 bytes | 128 bytes |
| EndpointId | 64 bytes | 64 bytes |
| configured endpoints/profile | 16 | 64 |
| advertised endpoints/profile | 16 | 32 |
| endpoint directory cache TTL | 60 s | 5 min |
| endpoint directory queries/peer/minute | 12 | 60 |
| endpoint directory inflight/profile | 16 | 64 |
| endpoint leases/client | 1 | 1 |
| subscriptions/profile | 128 | 1024 |
| connected peers | 256 | 2048 |
| discovery candidates | 4096 | 16384 |
| addresses/peer | 16 | 32 |
| advisory protocol observations/peer | 16 | 16 |
| IPC clients | 16 | 64 |
| IPC JSON body | 128 KiB | 128 KiB IPC v2 |
| backend->runtime events | 1024 | 8192 |
| per-client event queue | 256 | 1024 |
| outstanding commands/client | 64 | 256 |
| direct inflight total | 128 | 512 |
| direct inflight/peer | 8 | 32 |
| dedup IDs | 10,000 / 5 min | configurable bounded |

## Payload/IPC sizing invariant

49,152 payload bytes expand to 65,536 base64url characters before JSON syntax. IPC v2 therefore keeps the 131,072-byte body ceiling. Golden fixtures include maximum source/destination EndpointIds plus other bounded metadata.

## Endpoint routing backpressure

Direct inbound acceptance is endpoint-queue-aware. If the resolved endpoint queue cannot admit the event, the transport sends `RejectedV2(overloaded)` rather than `AcceptedV2` followed by a local drop.

This removes the v1 architecture's shared-profile direct fan-out memory multiplier. Each direct message enters at most one local endpoint queue. Broadcast can still fan out to multiple joined local clients, bounded by `max_clients` and per-client queues.

Endpoint-directory responses are bounded to 32 route IDs, 12 queries/minute/peer by default, 16 in-flight/profile by default, and short-lived cache state; no unbounded presence catalog exists.

## Drop policy

Broadcast local delivery may drop according to per-client bounded policy under overload. Direct delivery must reject before acceptance when target queue admission fails. There is no disk spill or hidden unbounded fallback.
