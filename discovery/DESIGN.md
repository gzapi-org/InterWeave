# Discovery design

## Objective

Make discovery independently replaceable without turning the runtime into a general dynamic-plugin framework.

```text
PeerCache ----\
mDNS ----------> DiscoveryManager -> CandidatePeerSet -> ConnectionManager
Static --------/
Kademlia -----/ (optional; default disabled)
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

It must not dial peers, alter trust, manage GossipSub, interpret payloads, or discover/aggregate `EndpointId` routes. Remote endpoint-directory results belong to the direct-routing/application-facing transport layer, not peer discovery.

## Candidate bounds

Default architecture caps:

- 4096 aggregate candidate PeerIds;
- 16 addresses per PeerId;
- 8 provenance records per address;
- provider-specific lower bounds where appropriate.

On overflow, evict expired then least-recently-observed untrusted candidates; configured static entries are retained within their own explicit cap. Eviction is diagnostic, not silent authority loss.


## Kademlia-specific integration

Kademlia is unusual because the actual `libp2p::kad::Behaviour` must live inside the single Swarm owner. The optional `KademliaDiscovery` provider therefore owns only scheduling/normalization/health and communicates with a Kademlia driver inside `transport-libp2p` through a bounded internal handle. This does not change the generic DiscoveryProvider contract and does not give the provider Swarm ownership.

See [../docs/architecture/kademlia-integration.md](../docs/architecture/kademlia-integration.md).
