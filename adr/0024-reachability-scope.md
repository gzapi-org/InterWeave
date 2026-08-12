# Conservative v1 reachability; advanced NAT traversal deferred

**Status:** Superseded by ADR-0035

## Context

The original architecture intentionally limited v1 reachability while transport/trust contracts were still unstable. Subsequent product direction requires ordinary consumer-NAT Internet operation in the standard v1 release.

## Decision

Historical decision only: directly reachable TCP/LAN/static-address operation was once considered sufficient for core v1, with relay/AutoNAT/DCUtR deferred.

**ADR-0035 supersedes this decision.** The standard v1 release now requires AutoNAT v2 client, Circuit Relay v2 client/reservation management, and DCUtR, with explicit infrastructure server roles and a mandatory SPIKE-004 release gate.

## Alternatives considered

all NAT features mandatory v1; LAN-only product; central relay required; bootstrap peer automatically acts as relay.

## Consequences

This ADR is retained for decision history only. Do not use it as current implementation guidance.

## Security implications

The earlier claim that fewer reachability protocols reduce initial attack surface remains historically true, but current architecture instead bounds the larger mandatory surface through ADR-0035/0036 resource and protocol-admission controls.

## Operational implications

See `transport/libp2p/CONNECTIVITY.md` and ADR-0035.

## Implementation implications

Do not implement the conditional/deferred model described by the original revision.

## Revisit conditions

None independently; current reachability changes require revisiting ADR-0035.
