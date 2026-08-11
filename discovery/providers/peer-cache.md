# PeerCacheDiscovery

Purpose: advisory fast-restart candidate source.

## State

Stored under the profile cache directory, separate from config and identity. Safe to delete. Suggested persisted record fields: PeerId, validated addresses, first/last successful observation timestamps, last failure timestamp, and expiry.

## Defaults

- TTL after last successful connection/validated observation: 7 days;
- max peers: 1024;
- max addresses/peer: 8;
- write debounce: 5 s;
- stale entries are ignored on read and compacted asynchronously;
- corrupt file: quarantine/rename, report degraded, continue with empty cache.

## Ownership

ConnectionManager reports successful connection/address observations to TransportRuntime; runtime submits `PeerHint::ObservedReachable` to PeerCacheDiscovery. DiscoveryManager owns provider lifecycle. The cache provider owns persistence format. This avoids persistence logic in GossipSub or Claude code.

A cached peer is never trusted because it was cached.
