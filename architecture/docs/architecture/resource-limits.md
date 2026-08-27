# Resource limits and backpressure

Default values are conservative architecture targets, not performance promises. **Deployment-neutral LocalDataSession/transport rows apply equally to desktop IPC and Android embedded sessions. Rows whose names begin with `IPC` are daemon-IPC binding limits only.** Removing the socket/serialization layer on Android never removes endpoint queues, event queues, command/in-flight, direct-rate, dedup or network-resource bounds.

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
| IPC connections (data + admin combined) | 16 | 64 |
| IPC admin-socket connections | 4 | 16 |
| IPC JSON body | 128 KiB | 128 KiB IPC v2 |
| IPC keepalive interval | 30 s | 5 min |
| IPC keepalive response timeout | 10 s | < interval, max 1 min |
| IPC keepalive missed probes | 3 | 10 |
| require keepalive for EndpointId lease | true | boolean policy |
| backend->runtime events | 1024 | 8192 |
| LocalDataSession event queue | 256 | 1024 |
| outstanding commands/LocalDataSession | 64 | 256 |
| direct inflight total | 128 | 512 |
| direct inflight/peer | 8 | 32 |
| inbound direct requests/trusted peer/minute | 120, burst 32 | 6000/min, burst 512 |
| inbound direct requests/global/minute | 1200, burst 256 | 60000/min, burst 2048 |
| inbound broadcast messages/trusted peer/minute | 120, burst 32 | 6000/min, burst 512 |
| inbound broadcast messages/global/minute | 1200, burst 256 | 60000/min, burst 2048 |
| direct dedup in-flight reservations/global | 128 | 512 |
| direct dedup in-flight reservations/source peer | 8 | 32 |
| dedup IDs | 10,000 / 5 min | configurable bounded |

## Payload/IPC sizing invariant

49,152 payload bytes expand to 65,536 base64url characters before JSON syntax. IPC v2 therefore keeps the 131,072-byte body ceiling. Golden fixtures include maximum source/destination EndpointIds plus other bounded metadata.

## Endpoint routing backpressure

Direct inbound acceptance is endpoint-queue-aware. If the resolved endpoint queue cannot admit the event, the transport sends `RejectedV2(overloaded)` rather than `AcceptedV2` followed by a local drop.

This removes the v1 architecture's shared-profile direct fan-out memory multiplier. Each direct message enters at most one local endpoint queue. Broadcast can still fan out to multiple joined local clients, bounded by `max_clients` and per-client queues.

Endpoint-directory responses are bounded to 32 route IDs, 12 queries/minute/peer by default, 16 in-flight/profile by default, and short-lived cache state; no unbounded presence catalog exists. Direct dedup reservation state is separately capped at 128 global / 8 per source peer by default. Trusted-peer inbound direct requests additionally pass fixed token buckets before endpoint routing. A human application using separate data-plane and admin IPC sockets consumes two IPC connection slots.

## Drop policy

Broadcast local delivery may drop according to per-client bounded policy under overload. Direct delivery must reject before acceptance when target queue admission fails. There is no disk spill or hidden unbounded fallback.


## Mandatory Internet reachability limits

| Resource | Default | Hard/config ceiling | Overflow behavior |
|---|---:|---:|---|
| total established/pending network connections | 384 | 4096 | root dial admission refuses/defer new work |
| pre-Noise inbound handshakes pending | 64 | 256 | close/refuse before authentication |
| pre-Noise pending per source-address bucket | 8 | 32 | close/refuse before authentication |
| pre-Noise starts/source bucket/minute | 30 | 600 | rate-limit before Noise |
| pre-Noise starts global/minute | 600 | 6000 | rate-limit before Noise |
| pre-Noise handshake timeout | 10 s | 30 s | close unauthenticated attempt |
| address identity-mismatch quarantine | 30 min | 24 h | suppress poisoned address, not whole trusted peer |
| connections per PeerId | 3 | 8 | refuse redundant new connection unless policy replaces one |
| AutoNAT v2 client probes in flight | 2 | 8 | defer next probe cycle |
| AutoNAT addresses tested per cycle | 4 | 16 | deterministic bounded selection |
| AutoNAT server concurrent probes | 8 | 64 | reject/defer probe |
| AutoNAT server probes per peer/min | 2 | 60 | rate-limit |
| AutoNAT server probes global/min | 60 | 600 | rate-limit |
| active relay reservations (client) | target 2 private/unknown, 1 public | 4 | do not acquire beyond cap |
| relay-server reservations total | 64 | 512 | deny new reservation |
| relay-server reservations per peer | 1 | 4 | deny new reservation |
| relay-server circuits total | 128 | 1024 | deny new circuit |
| relay-server circuits per source peer | 4 | 16 | deny new circuit |
| relay-server circuit bytes | 64 MiB | 1 GiB | close circuit at cap |
| relay-server pending control requests | 64 | 512 | reject/defer |
| DCUtR attempts in flight | 4 | 32 | defer |
| DCUtR attempts per peer | 1 | 4 | defer/cooldown |

These limits share the root connection/dial budget; reachability behaviours do not receive an unbounded side channel around `DialAdmissionGate`.

## Human/mobile resource profile

Human application SQLite retention (`pending_outbound`, `unread_inbound`, `kept_inbound`) is application-level state and is not part of transport resource ceilings. It must still be bounded by application storage quotas; exhaustion is surfaced as human-store degradation rather than silently violating unread durability. Android uses the same network hard ceilings; server roles are disabled, Kademlia is client-only, and mobile timer/query defaults may be lower-power within the frozen ranges. Android mDNS multicast resources are acquired only while the provider is active.

The embedded Android LocalDataSession uses the same default local event queue (256) and endpoint/direct admission limits as a desktop IPC data client; removing serialization does not justify an unbounded in-process queue.
