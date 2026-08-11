# Testing architecture

## Unit

- ChannelId validation/topic hashing fixtures;
- payload/frame limit rejection before allocation;
- discovery PeerId dedup/address merge/provenance expiry;
- provider lifecycle/health aggregation;
- static config tagged-union parsing;
- trust decisions independent of discovery;
- duplicate LRU/TTL behavior;
- backoff/cancellation state machines;
- reply-token expiry/routing;
- IPC framing/version negotiation.

## Discovery provider conformance

Every provider runs `contracts/DISCOVERY-CONFORMANCE.md` tests. Providers additionally test their own I/O and corruption/failure cases.

## Integration

1. two local libp2p peers: Noise + direct accepted/rejected/timeouts;
2. three peers: GossipSub broadcast reaches subscribed trusted peers;
3. mDNS discovery then connection;
4. static bootstrap candidate path;
5. cache restart path;
6. one provider fails while another succeeds;
7. duplicate observations from multiple providers merge correctly;
8. partition and recovery with bounded backoff;
9. untrusted discovered peer connects but does not deliver Channel message;
10. daemon restart preserves PeerId.

## Claude integration

- normalized external message -> exact Channel content/meta;
- non-UTF8 payload -> base64url + metadata;
- broadcast tool -> daemon publish command;
- send/reply -> correct direct/broadcast route;
- slow Claude consumer -> per-client drops/overload signal;
- bridge restart -> daemon persists and subscriptions recover;
- inbound request asking to modify trust -> no privileged auto-action/tool exists.

## Security/load

- rogue PeerId denied;
- invalid Noise/protocol input fails peer-locally;
- flood cannot create unbounded queues;
- oversized declared length rejected early;
- IPC unauthorized cross-user attempt fails;
- corrupt cache quarantined without identity loss;
- corrupt identity fails closed;
- connection storm respects global/per-peer semaphores.

## Compatibility fixtures

Keep wire/IPC golden fixtures by major version. A new implementation must parse prior compatible minor frames and reject unsupported major versions clearly.
