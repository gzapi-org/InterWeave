# KademliaDiscovery

Status: **fully specified standard-v1 provider design; configured entries default `enabled: true` per ADR-0034; implementation remains for the subsequent implementation repository**.

The end-to-end design is [../../docs/architecture/kademlia-integration.md](../../docs/architecture/kademlia-integration.md). ADR-0009 is normative for role/security; ADR-0011 is normative for dial-policy ownership.

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

It does not provide trust/authorization, channel membership, value/provider storage, durable peer registry, connection-policy ownership, or GossipSub membership management.

## Lifecycle

### start

1. Validate build support, cross-field configuration, and enabled `seed_sources` references.
2. If explicitly disabled, do not instantiate the provider or advertise the Kademlia protocol.
3. Obtain the neutral bounded Kademlia driver port from `kademlia-control-api`.
4. Set explicit client/server mode.
5. Accept eligible seed hints and fresh peer-cache protocol-capability observations.
6. Begin bootstrap only after at least one trusted/routable DHT server peer is admitted.
7. Start bounded bootstrap/targeted/random query scheduling and saturation tracking.

### add_hint

Accepted hints are normalized peer/address/capability observations from configured seed sources. A hint does not go directly into the Kademlia routing table. The provider/driver path still applies address checks, `PeerTrustPolicy`, exact protocol-support/Identify evidence, routing-table bounds, and manual insertion.

Kademlia-originated candidates are not fed back as external Kademlia seed hints.

### shutdown

Stop new queries; cancel/settle bounded in-flight work; disable Kademlia behavior/protocol participation; expire Kademlia-only provenance as appropriate; terminate deterministically.

## Query classes

- `bootstrap`: initial/self/bucket refresh;
- `targeted`: opportunistic lookup keyed by an independently trusted PeerId that has a fresh cached/Identify observation of the **exact project Kademlia server protocol** and lacks usable addresses;
- `exploration`: random 32-byte keys with bounded `get_n_closest_peers` results.

Client-mode peers are not promised to be discoverable by targeted peer routing.

All classes share global rate/concurrency budgets. Iterative Kademlia queries may cause the Swarm behavior itself to request network dials; those requests are not provider-owned dials and are still subject to ADR-0011's root `DialAdmissionGate`.

## Targeted server-capability observation

The provider does not infer remote server mode from trust or from the mere presence of a PeerId. Eligibility requires a freshness-bounded observation learned on a prior authenticated connection that the peer advertised:

```text
/interweave/kad/1.0.0/<current-network-hash>
```

The observation may be persisted by `PeerCacheDiscovery` with its timestamp and positive/negative support state. It is advisory, expires with the peer-cache record, and is superseded by fresh Identify evidence. On the candidate/hint path it travels as the exact derived protocol string `/interweave/kad/<wire_major>.0.0/<network_hash>` — `role = server` implied by presence, per the mapping in `kademlia-integration.md` §7 — so eligibility compares the full string, never a prefix.

## Routing eligibility

```text
Kademlia routing peer
  => valid PeerId/address
  => not self
  => allowed by PeerTrustPolicy
  => exact Kademlia server-protocol support observed
  => root dial/backoff/resource policy permits connections
  => project routing-table/resource limits
```

Discovery of an unauthorized PeerId is legal, but it is not retained as a route peer. If the iterative query engine attempts to dial a returned unauthorized/backed-off peer, the root Swarm dial gate denies establishment.

## Effective target and saturation

Let:

```text
remote_trusted_population = count(distinct allowed_peers excluding local PeerId)
effective_target = min(target_routing_peers, max_routing_peers, remote_trusted_population)
```

If `remote_trusted_population == 0`, Kademlia cannot become healthy and eventually reports unavailable after startup grace.

A routing view is target-satisfied when `routing_peers >= effective_target`. It may also become **saturated below target** after the configured implementation threshold of consecutive successful exploration rounds (initial design: 3) produces no new trust-admitted routing peer and there is no immediately targetable fresh server-capability observation outside the current routing set. Saturation is invalidated by trust changes, new seed/capability observations, routing loss, or provider restart.

After each no-progress exploration round, the next exploration interval increases exponentially from the configured base, capped at 15 minutes. Any newly admitted routing peer resets the interval. Thus a two-peer trusted overlay does not run a useless 60-second exploration loop forever.

Health can be `healthy` when the view is target-satisfied **or saturated**, recent queries succeed, and no stronger failure condition exists.

## Event normalization

Driver peer/query/routing observations become normal discovery events with Kademlia provenance and candidate TTL. Routing-table eviction or TTL expiry removes only Kademlia provenance; DiscoveryManager decides whether another provider still supports the aggregate peer/address.

## Record policy

Kademlia is peer routing only. The future driver never invokes value/provider record APIs. Incoming writes are filtered/not persisted. Any later record use requires a new ADR.

## Configuration compatibility

The standard v1 daemon build contains the provider implementation and configured Kademlia entries default to `enabled: true`:

- `enabled: true` (explicit or defaulted) -> start after full validation;
- `enabled: false` -> explicit opt-out with no provider/protocol/query activity;
- a reduced/custom build without implementation -> hard configuration/startup failure for an enabled/default-enabled entry;
- when enabled, every configured `seed_source` must name a provider present in the profile and itself `enabled: true`;
- cross-field invariants in `config/config.schema.yaml` are hard validation errors, not warnings.
