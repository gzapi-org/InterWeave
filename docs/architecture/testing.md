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
