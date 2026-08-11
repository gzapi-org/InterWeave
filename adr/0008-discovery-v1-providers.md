# v1 discovery providers are cache, optional mDNS, and static bootstrap

**Status:** Accepted

## Context

These three cover fast restart, zero-config LAN operation, and deterministic remote entry points with limited complexity.

## Decision

Ship the architecture for PeerCacheDiscovery, optional MdnsDiscovery, and StaticBootstrapDiscovery as the minimum v1 provider set. Kademlia is deferred.

## Alternatives considered

mDNS-only; static-only; Kademlia mandatory; central rendezvous service.

## Consequences

Internet-scale autonomous discovery is limited in v1, but the provider contract leaves it open. Operators can compose providers per deployment.

## Security implications

mDNS, cache, and static-bootstrap candidates are untrusted reachability input. Static configuration is not implicit trust; ConnectionManager does not establish ordinary v1 data-plane connectivity until the PeerId is separately allowlisted. Provider/resource caps apply.

## Operational implications

Remote deployments need static addresses/relay planning plus out-of-band trusted PeerIds. LAN discovery can find peers without infrastructure, but trust still gates connection admission.

## Implementation implications

Each provider gets its own crate/module behind discovery-api and must pass common conformance tests.

## Revisit conditions

Revisit when remote networks cannot be operated reasonably with configured entry points or when Kademlia/rendezvous value is demonstrated.
