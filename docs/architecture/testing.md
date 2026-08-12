# Testing architecture

## Unit

- ChannelId validation/topic hashing fixtures;
- transport payload and IPC frame limit rejection before large allocation;
- exact 49,152-byte payload base64url/JSON sizing in request and event directions;
- discovery PeerId dedup/address merge/provenance expiry;
- provider lifecycle/health aggregation;
- known-but-unsupported provider config rejection when enabled;
- static config tagged-union parsing;
- trust decisions independent of discovery;
- outbound `UnauthorizedPeer` before dial;
- connection admission/revocation policy;
- GossipSub `Accept | Ignore | Reject` mapping;
- duplicate LRU/TTL behavior with `(mode, source_peer, channel_or_none, message_id)`;
- backoff/cancellation state machines;
- reply-token expiry/routing and reply-after-leave;
- IPC framing/version/capability negotiation.

## Discovery provider conformance

Every provider runs `contracts/DISCOVERY-CONFORMANCE.md` tests. Providers additionally test their own I/O and corruption/failure cases.

## Integration

1. two local trusted libp2p peers: Noise + direct accepted/rejected/timeouts;
2. three trusted peers: GossipSub broadcast reaches subscribed peers;
3. mDNS discovery then trust-gated connection;
4. static bootstrap candidate path without implicit trust;
5. cache restart path;
6. one provider fails while another succeeds;
7. duplicate observations from multiple providers merge correctly;
8. partition and recovery with bounded backoff;
9. untrusted discovered peer is **not admitted to ordinary data-plane connectivity** and cannot deliver/receive GossipSub/direct data through this node;
10. trusted forwarding peer carries a signed message from an original publisher not in the local allowlist -> validation `Ignore`, no local delivery, no forwarding, no invalidity penalty solely for trust mismatch;
11. objectively malformed/cryptographically invalid GossipSub message -> validation `Reject`;
12. trust revocation while connected -> `TrustPolicyChanged`, policy disconnect, mesh/data-plane eviction;
13. daemon restart preserves PeerId.

## Claude integration

- normalized external message -> exact Channel content/meta;
- transport `media_type` -> Claude meta `content_type` mapping;
- non-UTF8 payload -> base64url + metadata;
- joined broadcast tool -> daemon publish command;
- broadcast without caller join -> `ChannelNotJoined` and no backend publish;
- broadcast reply token after leave -> `ChannelNotJoined`, no implicit rejoin;
- untrusted `send` target -> `UnauthorizedPeer`, no dial;
- send/reply -> correct direct/broadcast route;
- slow Claude consumer -> per-client drops/overload signal;
- bridge restart -> daemon persists and subscriptions recover;
- Claude bridge requests `shutdown` -> IPC capability denial;
- two same-profile IPC clients receive independent copies of one admitted direct message;
- broadcast reaches only clients holding the ChannelId join reference;
- profile `channels.desired` with zero joined clients keeps backend subscription state but creates no local delivery queue/replay;
- direct send to local PeerId -> `InvalidArgument`, no dial;
- inbound request asking to modify trust -> no privileged auto-action/tool exists.

## Security/load

- rogue PeerId denied at connection/data-plane boundary;
- invalid Noise/protocol input fails peer-locally;
- flood cannot create unbounded queues;
- oversized declared length rejected early;
- max legal 49,152-byte payload fits under 131,072-byte IPC body with maximal bounded v1 metadata;
- IPC body above 131,072 bytes is rejected;
- IPC unauthorized cross-user attempt fails;
- corrupt cache quarantined without identity loss;
- corrupt identity fails closed;
- connection storm respects trust plus global/per-peer semaphores.

## Compatibility fixtures

Keep wire/IPC golden fixtures by major version. IPC v1 fixtures **must** include an outbound request and inbound event carrying exactly 49,152 opaque payload bytes plus maximum bounded v1 envelope fields. A new implementation must parse prior compatible minor frames and reject unsupported major versions clearly.


## Kademlia optional-provider test suite

These tests are required before a build may advertise Kademlia as supported. `enabled: false` remains the shipped default.

### Configuration/unit

- disabled Kademlia creates no provider task, query, protocol advertisement, or routing state;
- unsupported build + `enabled: true` is a hard configuration/startup failure;
- supported build validates `network_id`, mode, bounds, and peer-routing-only `record_mode`;
- protocol derivation from `network_id` has deterministic golden fixtures;
- random exploration keys are independent of ChannelId/application input;
- K-bucket insert path is manual and refuses unauthorized peers;
- query concurrency/rate/cooldown budgets are deterministic under fake time;
- `PeerInfo` normalization applies self/address/candidate limits and Kademlia TTL;
- Kademlia cross-field config constraints and enabled seed-source references are validated;
- peer-cache Kademlia server-capability observations survive restart within TTL and remain advisory;
- `KadCommand::Snapshot` produces a bounded correlated `SnapshotResult` and never dumps raw routing tables.

### Multi-peer integration

1. one trusted server seed + two client nodes bootstrap successfully;
2. 10-20 trusted nodes converge under random exploration within configured query bounds;
3. targeted lookup is scheduled only for an allowlisted peer with fresh cached/Identify evidence of the exact Kademlia server protocol, and can recover an address not otherwise usable; no equivalent guarantee is asserted for client-mode peers;
4. client-mode peers do not become Kademlia servers;
5. server-mode peers answer peer-routing queries without storing value/provider records;
6. Kademlia behaviour-originated dials obey ConnectionManager backoff, trust, shutdown state, and global limits through the Swarm-wide dial gate;
7. a small allowlist reaches an effective target below the configured target and can become healthy/saturated; repeated no-new-peer exploration backs off;
8. server-mode reachability evidence produces the documented healthy/degraded diagnostics without claiming AutoNAT verification;
9. trust revocation removes a Kademlia routing peer and prevents further query use;
10. an unauthorized discovered/returned peer may remain advisory state but cannot pass Kademlia routing admission or outbound Swarm dial admission;
11. static/cache/mDNS seed observations merge with Kademlia provenance without duplicate peer identity;
12. protocol/network namespace mismatch fails cleanly without public-IPFS-DHT fallback;
13. Kademlia provider/driver failure does not stop direct/GossipSub traffic on existing peers.

### Adversarial/load

- malicious trusted router returns maximum hostile/stale address sets -> global caps hold;
- trusted Sybil/eclipse simulation -> disjoint-path behavior and bootstrap diversity are measured/documented;
- query flood trigger -> max concurrent/query-per-minute budgets hold;
- inbound PUT_VALUE/ADD_PROVIDER flood -> no record store growth, bounded diagnostics;
- driver event flood -> Swarm responsiveness remains within test threshold;
- repeated bootstrap failure -> provider degrades without restart storm.
