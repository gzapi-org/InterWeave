# Resource limits and backpressure

Default values are conservative architecture targets, not performance promises.

| Resource | Default | Hard architectural ceiling |
|---|---:|---:|
| application payload | 48 KiB | 48 KiB v1 transport contract |
| ChannelId | 128 bytes | 128 bytes v1 contract |
| subscriptions/profile | 128 | 1024 |
| connected peers | 256 | 2048 |
| discovery candidates | 4096 | 16384 |
| addresses/peer | 16 | 32 |
| IPC clients | 16 | 64 |
| IPC JSON body | 128 KiB | 128 KiB v1 IPC |
| backend->runtime events | 1024 | 8192 |
| per-client event queue | 256 | 1024 |
| outstanding commands/client | 64 | 256 |
| direct inflight total | 128 | 512 |
| direct inflight/peer | 8 | 32 |
| dedup IDs | 10,000 / 5 min | configurable bounded |

## Payload/IPC sizing invariant

The transport hard ceiling is 49,152 payload bytes. Base64url representation of that exact byte count is 65,536 characters before JSON syntax and metadata. Therefore v1 IPC uses a 131,072-byte JSON-body ceiling; a 64 KiB frame is not contract-compliant.

Every legal maximum-size transport payload must fit in both an outbound IPC command and inbound IPC event together with maximum bounded v1 metadata. Golden fixtures test this exact boundary. Profiles may lower `max_payload_bytes`; `TransportCapabilities.max_payload_bytes` reports that effective value.

## Drop policy

Network ingress is validated before queue admission. When the normalized runtime queue is full, reject new direct requests where a rejection can be returned and drop/record broadcast messages. Per-IPC-client queue overflow drops oldest ordinary message events while preserving reserved control-health capacity.

There is no disk spill and no unbounded memory fallback.
