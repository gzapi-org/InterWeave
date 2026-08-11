# Discovery composition

## Merge model

Key by PeerId. Each address carries a set of source observations:

```text
Peer X
  /ip4/192.168.1.50/tcp/4001
     sources: mdns(expiry=t1)
  /dns4/node.example/tcp/4001
     sources: static(no expiry)
  /ip4/203.0.113.5/tcp/4001
     sources: cache(expiry=t2), kademlia(expiry=t3)
```

Expiry removes a source observation. An address disappears only when no active source supports it. A peer candidate disappears when no addresses/provenance remain, except a transient connected-peer observation maintained by ConnectionManager outside discovery.

## Priority

Providers can have an integer priority/cost hint used when selecting among candidate addresses. Priority does not suppress concurrent providers and is not trust.

Default suggested configuration intent:

1. peer-cache: fastest recent hints;
2. mDNS: local low-latency paths;
3. static bootstrap: deterministic external entry points;
4. Kademlia: wider network exploration when enabled.

ConnectionManager may prefer LAN/private or previously successful addresses independently of provider priority.

## Phasing

- Peer-cache and static providers emit initial configured/local state immediately.
- mDNS starts concurrently.
- active wide-area discovery can reduce query intensity when the runtime has sufficient diverse trusted connectivity.
- provider backoff is independent.

This is adaptive scheduling, not a mandatory sequential discovery pipeline.
