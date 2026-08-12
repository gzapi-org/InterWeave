# Peer cache is a discovery provider; runtime reports successful observations

**Status:** Accepted

## Context

Caching is a source of future candidate discovery. ConnectionManager knows which address/connection succeeded, while authenticated Identify also yields transport protocol observations that may be useful after restart (for example, whether a trusted peer previously advertised the exact project Kademlia server protocol). Persistence must not leak into connection or Kademlia ownership.

## Decision

Persist historical peer reachability and bounded authenticated **transport protocol observations** only through PeerCacheDiscovery. ConnectionManager/Identify adapters report successful address/connection/protocol observations to TransportRuntime, which feeds the provider as hints. DiscoveryManager owns provider lifecycle. GossipSub, Kademlia, and Claude never write cache state directly.

Protocol observations are freshness-bounded advisory facts, keyed by exact opaque protocol identifier plus observed support state/time. They do not grant trust, application roles, membership, or current liveness.

## Alternatives considered

ConnectionManager writes cache directly; Kademlia writes its own capability database; DiscoveryManager guesses connection success; global peer database; no cache.

## Consequences

There is a narrow hint path from runtime to provider. Cache format can evolve without changing connection logic. Kademlia targeted-lookup scheduling can reuse a recent server-protocol observation after restart without making the DHT routing table durable.

## Security implications

Only reachability/protocol metadata is stored; no trust decision or message payload. Cache compromise can poison advisory addresses/capability observations, but current `PeerTrustPolicy`, fresh Identify evidence, query cooldowns, and root dial admission still apply.

## Operational implications

Deleting cache is safe. Corruption degrades fast restart and capability-aware targeting only; it does not change PeerId or trust.

## Implementation implications

Define bounded `PeerHint::ObservedReachable` and `PeerHint::ObservedProtocol` style inputs. Provider debounces atomic writes, applies TTL/size caps, and supersedes stale protocol support with fresh authenticated Identify observations.

## Revisit conditions

Revisit if protocol observations become numerous or another subsystem requires durable transport capability negotiation; generalize carefully without creating a membership/trust database.