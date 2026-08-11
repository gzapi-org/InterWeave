# Discovery and connection management are separate

**Status:** Accepted

## Context

Discovery mechanisms produce information; dialing policy depends on trust, current topology, limits, and retry state. Combining them makes providers control the Swarm and prevents composition.

## Decision

DiscoveryManager owns candidate knowledge. ConnectionManager alone decides dialing, reconnect, backoff, retention, and connection limits. Libp2p-specific execution lives in the backend; normalized connection state is reported upward.

## Alternatives considered

Providers dial directly; DiscoveryManager owns Swarm; Transport core implements multiaddress dialing itself.

## Consequences

There is an explicit handoff and address-book synchronization cost, but failure ownership is clear and testable.

## Security implications

Untrusted discovery cannot force unlimited dials; ConnectionManager applies limits and policy. This reduces amplification/connection-storm risk.

## Operational implications

Dial backoff and limits are consistent across providers. Provider outages do not tear down good connections.

## Implementation implications

Backend consumes normalized candidate updates and maintains a bounded dialable address book. ConnectionManager reports successful observations back for cache hints.

## Revisit conditions

Revisit if a backend requires a tightly coupled discovery behavior; adapt internally without changing the external ownership rule.
