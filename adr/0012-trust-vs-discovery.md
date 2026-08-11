# Deny-by-default static PeerId trust policy in v1

**Status:** Accepted

## Context

Authenticated PeerId is necessary but not sufficient authorization. A small static model is safer than inventing distributed membership in the transport layer.

## Decision

Use a `PeerTrustPolicy` abstraction. v1 data admission is a static allowlist of transport PeerIds, deny by default. Discovery never mutates it. Trust administration is a local privileged path, not a Claude Channel tool triggered by remote content.

## Alternatives considered

AllowAll default; trust every discovered/bootstrap peer; TOFU default; project secret as implicit identity; DHT membership.

## Consequences

Operators must distribute PeerIds out-of-band. This is less convenient at scale but clear and auditable. Future policy implementations remain possible.

## Security implications

Strongly reduces rogue-peer injection. Key theft still impersonates that PeerId; application role binding is out of scope.

## Operational implications

Trust changes can be reloaded locally and audited. Removing a peer can trigger connection/subscription eviction according to policy.

## Implementation implications

Define trust-core without discovery dependencies. Enforce admission before MessageReceived and before Channel event generation.

## Revisit conditions

Revisit for enterprise scale or usability after designing signed membership/enterprise policy with explicit revocation semantics.
