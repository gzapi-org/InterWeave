# Conservative v1 reachability; advanced NAT traversal deferred

**Status:** Accepted

## Context

Enabling every libp2p reachability mechanism immediately multiplies state machines and testing. The transport architecture should establish correctness/security before universal NAT traversal.

## Decision

Guarantee directly reachable TCP peers, configured/cache-discovered addresses, and optional LAN mDNS. Treat Circuit Relay v2 as an optional near-term deployment feature after a spike. Defer AutoNAT and DCUtR/hole punching until remote connectivity evidence requires them.

## Alternatives considered

all NAT features mandatory v1; LAN-only product; central relay required; bootstrap peer automatically acts as relay.

## Consequences

Some Internet deployments require public endpoints or explicit relay infrastructure. Bootstrap and relay roles stay separate.

## Security implications

Fewer listeners/protocols reduce initial attack surface. Relay operators can observe metadata and become availability dependencies but not trust authorities.

## Operational implications

Deployment docs must state reachability prerequisites honestly. NAT failures surface as diagnostics, not hidden retries forever.

## Implementation implications

SPIKE-004 measures NAT/relay behavior. Add features behind backend capabilities/configuration without changing transport API.

## Revisit conditions

Revisit when target deployment matrices demand consumer-NAT operation or relay availability becomes a hard requirement.
