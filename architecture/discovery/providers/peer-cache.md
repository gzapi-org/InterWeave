# PeerCacheDiscovery

Purpose: advisory fast-restart candidate source.

## State

Stored under the profile cache directory, separate from config and identity. Safe to delete. Suggested persisted record fields:

- PeerId;
- validated addresses;
- first/last successful observation timestamps;
- last failure timestamp;
- expiry;
- bounded **protocol capability observations** learned from authenticated connections.

A protocol capability observation is advisory metadata such as:

```text
ProtocolCapabilityObservation {
  protocol_family: "interweave/kad",
  wire_major: 1,
  network_hash: "...",
  role: "server",
  supported: true | false,
  observed_at,
}
```

The record stores at most a small bounded number of capability observations (initial target: 16). Capability freshness never outlives the enclosing peer-cache record TTL. Deleting the cache removes this evidence without affecting trust or identity.

For Kademlia, this observation solves a cold-start scheduling problem: a prior authenticated Identify exchange can establish that a trusted PeerId advertised the exact project Kademlia server protocol. After restart, a targeted lookup may use that **fresh advisory observation** even when the peer's cached addresses are now stale/unusable. The cache entry does not prove the peer is currently online or still server-mode.

A fresh authenticated Identify exchange supersedes the cached observation. If the peer no longer advertises the exact Kademlia server protocol/network namespace, the corresponding positive capability observation is replaced/removed (and an optional bounded negative observation may be recorded to suppress pointless targeting until expiry).

## Defaults

- TTL after last successful connection/validated observation: 7 days;
- max peers: 1024;
- max addresses/peer: 8;
- max protocol capability observations/peer: 16;
- write debounce: 5 s;
- stale entries are ignored on read and compacted asynchronously;
- corrupt file: quarantine/rename, report degraded, continue with empty cache.

## Ownership

ConnectionManager reports successful connection/address observations to TransportRuntime; authenticated Identify/protocol observations follow the same bounded hint path. Runtime submits reachability/capability hints to PeerCacheDiscovery. DiscoveryManager owns provider lifecycle. The cache provider owns persistence format.

This avoids persistence logic in GossipSub, Kademlia, or Claude code. The Kademlia provider may **read** fresh capability observations through normal candidate/hint data, but it does not own the cache file.

A cached peer is never trusted because it was cached. A cached protocol capability is never authorization and never guarantees current reachability.
