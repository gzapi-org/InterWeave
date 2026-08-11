# Bootstrap peers are non-authoritative discovery hints

**Status:** Accepted

## Context

Centralizing meaning in bootstrap nodes would undermine P2P failure independence and violate discovery/trust separation.

## Decision

Treat every static bootstrap entry as a normal DiscoveryProvider candidate. It may help obtain initial connectivity **only when that PeerId is separately authorized by the active trust policy**; configuration alone never grants that authorization. A bootstrap peer has no identity-authority, trust-root, membership, channel-owner, coordination, storage, or broker authority.

## Alternatives considered

Bootstrap as trust root; bootstrap as membership registry; bootstrap as message broker; special-case bootstrap in ConnectionManager.

## Consequences

The network may continue after bootstrap disappearance when peers have learned sufficient alternative connectivity. Deployments may configure several independent entry points.

## Security implications

A malicious/configured bootstrap can steer candidate information but cannot by itself authorize dialing or payload delivery. Eclipse risk remains if the trusted entry set is too narrow or an authorized bootstrap is malicious.

## Operational implications

Bootstrap candidate/provider health is observable separately. Dial-time DNS/reachability failure belongs to ConnectionManager diagnostics. A dead bootstrap degrades candidate availability but does not kill existing connections.

## Implementation implications

Implement StaticBootstrapDiscovery through the same contract and candidate path as all other providers.

## Revisit conditions

Revisit only if an explicit higher-level membership service is introduced; that service must remain outside the generic transport.
