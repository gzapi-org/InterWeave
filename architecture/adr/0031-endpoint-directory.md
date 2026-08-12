# Trust-gated remote endpoint directory

**Status:** Accepted

## Context

Endpoint-addressed direct delivery is usable with out-of-band endpoint names, but human clients benefit from learning which routes a trusted peer currently exposes. Automatically publishing all local endpoints would leak application presence and risk turning route names into accidental identity claims.

## Decision

Add an optional request-response endpoint-directory protocol `/interweave/endpoints/1.0.0`.

A query is accepted only from a profile-trusted peer. The response contains at most 32 lexicographically sorted EndpointIds that are simultaneously:

- enabled/configured locally;
- actively leased by a local IPC client;
- configured `advertise: true`;
- admissible for the querying peer under the endpoint's inbound narrowing policy.

No display names, roles, avatars, client kinds, payload schemas, prompts, trust claims, or application metadata are exposed.

Directory results are advisory, in-memory, short-lived cache entries. An explicit endpoint send does not require a prior directory query. Queries are independently rate/concurrency bounded (initial defaults: 12/minute/remote PeerId and 16 in-flight/profile; ceilings 60 and 64).

## Alternatives considered

No directory protocol; GossipSub endpoint announcements; persistent DHT endpoint records; Identify extension metadata; advertise configured-but-offline endpoints; expose client type/display labels.

## Consequences

Human UI can offer route selection without conflating discovery with identity. Presence becomes observable to trusted peers for endpoints that explicitly opt in. Stale results remain possible and direct send handles them through normal `no_route` responses.

## Security implications

Trust gating and opt-in advertisement reduce endpoint enumeration. Endpoint directory data is authenticated only as a statement from the remote PeerId; it does not prove who or what owns an endpoint. Endpoint-specific denial is not distinguished over the wire.

## Operational implications

Directory can be disabled independently of direct endpoint routing. Operators choose advertisement per endpoint. The daemon exposes directory query/cache diagnostics without payload logging.

## Implementation implications

Add a small separate request-response behavior/control path, bounded response codec, runtime snapshot of active advertised endpoints, and a bounded in-memory remote-directory cache. It reuses ConnectionManager trust/dial policy.

## Revisit conditions

Revisit if application capability negotiation, cryptographically signed endpoint descriptors, privacy-preserving presence, or large endpoint populations become requirements.
