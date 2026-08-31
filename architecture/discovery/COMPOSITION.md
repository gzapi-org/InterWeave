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

Bounded `protocol_observations` are merged separately by `(peer_id, protocol_id, source)` with their own observation timestamp/freshness inherited from the source. A fresh authenticated observation supersedes an older positive/negative observation from the same source. Protocol observations never change trust and do not keep an otherwise expired peer candidate alive beyond the source's TTL.

**A candidate is its source's WHOLE statement of protocol facts, so omission is withdrawal.** When a source emits a candidate for a peer, the observations it carries replace every observation that source held for that peer: a fact previously asserted and now absent is retracted at once, and is not kept alive until the candidate expires. Other sources' facts for the same peer are untouched, because the statement is per source.

**Only a snapshot that is not stale may withdraw.** A candidate whose observation time is older than what that source has already had applied is too stale to update an address, and it is equally too stale to retract a fact — otherwise a delayed older snapshot silently undoes a newer one. Withdrawal is judged at snapshot granularity rather than per fact, because that is what an omission is a statement about: a source saying "these are my facts" can only speak for the moment it describes.

The rule exists because a source's own freshness bound is finer than the candidate's. A peer-cache record kept alive by a reachability refresh outlives the capability observation attached to it, and without withdrawal-by-omission the empty re-emission that follows the observation ageing out could take nothing back — the refreshed expiry arrived first, so a lapsed positive stayed eligible for another full record lifetime. Withdrawal does not lower the applied-freshness floor, so a retracted fact still cannot be re-asserted by delayed older evidence.

**The obligation this places on a provider:** a source that asserts protocol facts must re-assert them on every candidate it emits for that peer. Emitting a candidate with the facts omitted is a retraction and is read as one. A provider that never asserts protocol facts retracts nothing and is unaffected.

## Priority

Providers can have an integer priority/cost hint used when selecting among candidate addresses. Priority does not suppress concurrent providers and is not trust.

Default suggested configuration intent:

1. peer-cache: fastest recent hints;
2. mDNS: local low-latency paths;
3. static bootstrap: deterministic external entry points;
4. Kademlia: trust-bounded wider network peer routing when explicitly enabled in a supporting build.

ConnectionManager may prefer LAN/private or previously successful addresses independently of provider priority.

## Phasing

- Peer-cache and static providers emit initial configured/local state immediately.
- mDNS starts concurrently.
- active wide-area discovery can reduce query intensity when the runtime has sufficient diverse trusted connectivity.
- provider backoff is independent.

This is adaptive scheduling, not a mandatory sequential discovery pipeline.


## Kademlia seed flow

Configured seed-source observations (`peer-cache`, `static-bootstrap`, optionally `mdns`) may be forwarded as hints to Kademlia. Peer-cache hints may include freshness-bounded exact protocol observations from prior authenticated Identify exchanges. They remain advisory and must pass current trust/address/protocol eligibility before manual Kademlia routing insertion. Kademlia-derived observations are not recursively fed back into Kademlia as external seed hints.

The first integration does not use unauthorized peers as DHT routing intermediaries; this preserves the existing connection and GossipSub trust boundary.


## Connectivity-infrastructure boundary

Phase-9 relay/AutoNAT service authorization is **not peer discovery and not application trust**. Discovery providers may contribute ordinary address/protocol observations, but they do not add PeerIds to `transport.connectivity.infrastructure.allowed_peers`, create relay reservations, run AutoNAT probes, or initiate DCUtR. Those responsibilities stay in the libp2p connectivity/connection layer.
