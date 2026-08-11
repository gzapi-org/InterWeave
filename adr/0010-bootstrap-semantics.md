# Bootstrap peers are non-authoritative discovery hints

**Status:** Accepted

## Context

Centralizing meaning in bootstrap nodes would undermine P2P failure independence and violate discovery/trust separation.

## Decision

Treat every static bootstrap entry as a normal DiscoveryProvider candidate. It may help obtain initial connectivity but has no identity, trust, membership, channel-owner, coordination, storage, or broker authority.

## Alternatives considered

Bootstrap as trust root; bootstrap as membership registry; bootstrap as message broker; special-case bootstrap in ConnectionManager.

## Consequences

The network may continue after bootstrap disappearance when peers have learned sufficient alternative connectivity. Deployments may configure several independent entry points.

## Security implications

A malicious bootstrap can steer connectivity but cannot by itself authorize payload delivery. Eclipse risk remains if it is the only reachable entry point.

## Operational implications

Bootstrap health is observable separately. A dead bootstrap degrades discovery but does not kill existing connections.

## Implementation implications

Implement StaticBootstrapDiscovery through the same contract and candidate path as all other providers.

## Revisit conditions

Revisit only if an explicit higher-level membership service is introduced; that service must remain outside the generic transport.
