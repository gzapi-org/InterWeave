# Discovery design

## Objective

Make discovery independently replaceable without turning the runtime into a general dynamic-plugin framework.

```text
PeerCache ----\
mDNS ----------> DiscoveryManager -> CandidatePeerSet -> ConnectionManager
Static --------/
Kademlia -----/ (standard-v1 supported; configured entries default enabled)
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

A **configured static entry**, for the purpose of that retention, is one supported by a provider whose descriptor declares `scope: configured` **and** `supports_expiry: false`. Scope alone does not identify one: `PeerCacheDiscovery` is also `configured`, and protecting it inverts the rule — cache records are observed more recently than a bootstrap entry emitted once at start, so an implementation keying on scope alone fills the retained set with cache records and leaves the static entries evictable.

The discriminator is `supports_expiry` because that is the property the retention rests on. A provider that declares no expiry retracts an entry or leaves it standing; it does not re-emit, so an evicted entry is lost until the provider is reloaded. A cache record ages out and is re-read from disk, so losing one to eviction costs a refresh rather than the entry.

For the same reason, a candidate address supported by such a provider may **displace** an address that is not, when a peer is already at its per-address cap. Insertion order is not a quality ranking, and LAN or cache observations routinely reach a peer before an operator's configured route does. The displacement is one-directional and does not raise any cap: when every slot is already held by a non-expiring configured source, a further address is refused like any other.


## Kademlia-specific integration

Kademlia is unusual because the actual `libp2p::kad::Behaviour` must live inside the single Swarm owner. `KademliaDiscovery` therefore owns only scheduling/normalization/health and communicates with a Kademlia driver inside `transport-libp2p` through a bounded internal handle. This does not change the generic DiscoveryProvider contract and does not give the provider Swarm ownership.

See [../docs/architecture/kademlia-integration.md](../docs/architecture/kademlia-integration.md).


## Connectivity-infrastructure boundary

Phase-9 relay/AutoNAT service authorization is **not peer discovery and not application trust**. Discovery providers may contribute ordinary address/protocol observations, but they do not add PeerIds to `transport.connectivity.infrastructure.allowed_peers`, create relay reservations, run AutoNAT probes, or initiate DCUtR. Those responsibilities stay in the libp2p connectivity/connection layer.
