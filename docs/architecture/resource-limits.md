# Resource limits and backpressure

Default values are conservative architecture targets, not performance promises.

| Resource | Default | Hard architectural ceiling idea |
|---|---:|---:|
| application payload | 48 KiB | 48 KiB v1 contract |
| ChannelId | 128 bytes | 128 bytes v1 contract |
| subscriptions/profile | 128 | 1024 |
| connected peers | 256 | 2048 |
| discovery candidates | 4096 | 16384 |
| addresses/peer | 16 | 32 |
| IPC clients | 16 | 64 |
| IPC frame | 64 KiB | 64 KiB v1 IPC |
| backend->runtime events | 1024 | 8192 |
| per-client event queue | 256 | 1024 |
| outstanding commands/client | 64 | 256 |
| direct inflight total | 128 | 512 |
| direct inflight/peer | 8 | 32 |
| dedup IDs | 10,000 / 5 min | configurable bounded |

## Drop policy

Network ingress is validated before queue admission. When the normalized runtime queue is full, reject new direct requests where a rejection can be returned and drop/record broadcast messages. Per-IPC-client queue overflow drops oldest ordinary message events while preserving reserved control-health capacity.

There is no disk spill and no unbounded memory fallback.
