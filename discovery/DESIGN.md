# Discovery design

## Objective

Make discovery independently replaceable without turning the runtime into a general dynamic-plugin framework.

```text
PeerCache ----\
mDNS ----------> DiscoveryManager -> CandidatePeerSet -> ConnectionManager
Static --------/
Kademlia -----/ (deferred)
```

## DiscoveryManager responsibilities

- instantiate configured providers from a compile-time registry;
- start/stop/restart provider tasks;
- aggregate event streams;
- normalize/deduplicate by PeerId;
- merge address sets;
- retain per-address provenance and expiry;
- compute candidate expiry when all supporting observations expire;
- expose aggregate/provider health;
- apply global candidate/address bounds;
- accept successful connection observations for peer-cache persistence;
- support configuration-time composition and controlled reload.

It must not dial peers, alter trust, manage GossipSub, or interpret payloads.

## Candidate bounds

Default architecture caps:

- 4096 aggregate candidate PeerIds;
- 16 addresses per PeerId;
- 8 provenance records per address;
- provider-specific lower bounds where appropriate.

On overflow, evict expired then least-recently-observed untrusted candidates; configured static entries are retained within their own explicit cap. Eviction is diagnostic, not silent authority loss.
