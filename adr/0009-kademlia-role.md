# Defer Kademlia from the minimum v1 discovery build

**Status:** Accepted

## Context

Kademlia can provide distributed peer discovery but adds bootstrap dependence, routing convergence, poisoning/Sybil/eclipsing exposure, traffic/privacy cost, and additional state. The initial requirements can be exercised with cache, mDNS, and static bootstrap.

## Decision

Do not require or implement Kademlia in the minimum v1 build. Preserve a `KademliaDiscovery` design behind `DiscoveryProvider` for a later evidence-driven phase. If later implemented, use bounded namespace-independent `get_closest_peers`-style candidate expansion rather than ChannelId provider records or membership records.

The architecture schema may reserve the `kademlia` provider type, but a minimum-v1 binary that does not contain the provider must reject `kademlia.enabled: true` as a hard configuration validation/startup error. `enabled: false` is accepted as a reserved disabled entry. Known-but-unimplemented does not mean silently ignored.

## Alternatives considered

Mandatory Kademlia; DHT provider records keyed by ChannelId; Kademlia as trust/membership database; omit Kademlia permanently.

## Consequences

Minimum v1 has weaker automatic Internet-scale discovery but a smaller attack/operational surface. Configuration cannot accidentally imply a capability the binary does not implement.

## Security implications

Deferral reduces DHT attack surface. Future implementation still needs peer diversity, bounds, and distrust of DHT results. Failing hard when enabled avoids false confidence that a requested discovery mechanism is active.

## Operational implications

Controlled deployments use cache/static hints and optional mDNS. Operators enabling a provider absent from the build get an explicit startup/configuration error rather than degraded-but-misleading operation.

## Implementation implications

Keep Kademlia provider config/schema reserved but disabled/unsupported in the minimum implementation until SPIKE-003 and an ADR update. The config parser/provider registry must distinguish `known_disabled`, `known_supported`, and `known_but_unsupported_when_enabled` states.

## Revisit conditions

Revisit after SPIKE-003 or when wide-area candidate discovery requirements cannot be met acceptably by simpler providers.
