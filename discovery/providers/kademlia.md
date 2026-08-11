
# KademliaDiscovery

Status: **fully specified optional provider; implementation deferred; `enabled: false` by default**.

The end-to-end design is [../../docs/architecture/kademlia-integration.md](../../docs/architecture/kademlia-integration.md). ADR-0009 is normative for the role and security boundary.

## Provider descriptor

Conceptual descriptor:

```text
name: kademlia
interface_version: discovery-v1
config_version: 1
scope: network
mode: active
supports_expiry: true
supports_hints: true
```

## Purpose

Produce advisory `CandidatePeer` observations by using a private, project-specific libp2p Kademlia peer-routing overlay.

It does not provide:

- trust or authorization;
- channel membership;
- value storage;
- provider records;
- durable peer registry;
- direct dialing ownership;
- GossipSub membership management.

## Lifecycle

### start

1. Validate config and build support.
2. If disabled, the provider is not instantiated.
3. Obtain a bounded `KadControlHandle` from the libp2p backend.
4. Set explicit client/server mode.
5. Accept eligible seed hints from configured source providers.
6. Begin bootstrap only after at least one trusted/routable DHT peer is admitted.
7. Start bounded bootstrap/targeted/random query scheduling.

### add_hint

Accepted hints are normalized peer/address observations from configured seed sources. A hint does not go directly into the Kademlia routing table. The provider/driver path still applies address checks, `PeerTrustPolicy`, protocol-support/Identify evidence, routing-table bounds, and manual insertion.

Kademlia-originated candidates are not fed back as external Kademlia seed hints.

### shutdown

- stop scheduling new queries;
- cancel/settle bounded in-flight work;
- remove/disable the Kademlia behavior/protocol participation;
- emit expiry/removal for Kademlia-only provenance as appropriate;
- terminate the provider event stream deterministically.

## Event normalization

Driver `PeerInfo`, routing updates, and successful query observations become normal discovery events:

```text
Discovered / Updated {
  peer_id,
  addresses,
  source: kademlia,
  observed_at,
  expires_at: observed_at + candidate_ttl
}
```

Routing-table eviction or TTL expiry removes only Kademlia provenance. DiscoveryManager decides whether the aggregate peer/address remains due to other providers.

## Query classes

- `bootstrap`: initial/self/bucket refresh;
- `targeted`: opportunistic lookup keyed by an independently trusted **server-mode DHT participant** PeerId with missing/unusable addresses; client-mode peers are not promised to be discoverable by Kademlia peer routing;
- `exploration`: random 32-byte keys with bounded `get_n_closest_peers` results.

All classes share global rate/concurrency budgets.

## Routing eligibility

First integration rule:

```text
Kademlia routing peer
  => valid PeerId/address
  => not self
  => allowed by PeerTrustPolicy
  => expected Kademlia protocol support / eligible server observation
  => project routing-table/resource limits
```

Discovery of an unauthorized PeerId is still legal, but the peer is not admitted as a Kademlia routing/query peer or dialed by this provider.

## Record policy

Kademlia is peer routing only. The future driver never invokes value/provider record APIs. Incoming record/provider-record writes are filtered and not persisted. Any later record use requires a new ADR.

## Health

`healthy`: recent query/bootstrap progress and at least one eligible route peer.

`degraded`: warming, under target, intermittent query failures, or configured server mode without adequate reachability evidence.

`unavailable`: no eligible route peer after grace, repeated bounded query failure, or driver unavailable.

Provider failure never terminates other providers or existing data-plane connections.

## Configuration compatibility

A daemon may recognize this schema without containing the provider implementation. In that case:

- `enabled: false` -> valid reserved configuration;
- `enabled: true` -> hard configuration/startup failure.

A supported build still defaults to `enabled: false` and requires explicit opt-in.
