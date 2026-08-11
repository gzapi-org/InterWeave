# Peer cache is a discovery provider; runtime reports successful observations

**Status:** Accepted

## Context

Caching is a source of future candidate discovery, but only ConnectionManager knows which dial/address succeeded. This split preserves ownership without leaking persistence into dialing.

## Decision

Persist known reachable peers only through PeerCacheDiscovery. ConnectionManager reports successful address/connection observations to TransportRuntime, which feeds the provider as hints. DiscoveryManager owns provider lifecycle. GossipSub/Claude never write cache state.

## Alternatives considered

ConnectionManager writes cache directly; DiscoveryManager guesses connection success; global peer database; no cache.

## Consequences

There is a narrow hint path from runtime to provider. Cache format can change without changing connection logic.

## Security implications

Only reachability metadata is stored; no trust decision or message payload. Cache compromise can poison addresses but trust still gates messages.

## Operational implications

Deleting cache is safe. Corruption degrades fast restart only and is recoverable.

## Implementation implications

Define `PeerHint::ObservedReachable` style event. Provider debounces atomic writes and applies TTL/size caps.

## Revisit conditions

Revisit if another provider needs shared successful-observation data; generalize hints carefully without creating a second connection manager.
