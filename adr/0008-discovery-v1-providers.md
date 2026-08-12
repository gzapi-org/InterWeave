# v1 discovery providers are cache, optional mDNS, and static bootstrap

**Status:** Superseded in part by ADR-0034 (standard v1 now includes Kademlia support/default-on configured entries); cache/mDNS/static provider roles remain accepted.

## Context

These three cover fast restart, zero-config LAN operation, and deterministic remote entry points with limited complexity.

## Decision

Historical decision: PeerCacheDiscovery, optional MdnsDiscovery, and StaticBootstrapDiscovery formed the minimum provider set. **ADR-0034 supersedes the rollout portion:** the standard v1 build now also includes Kademlia support, and configured Kademlia entries default enabled. The roles/boundaries of cache, mDNS, and static bootstrap remain unchanged.

## Alternatives considered

mDNS-only; static-only; Kademlia mandatory; central rendezvous service.

## Consequences

Provider composition remains explicit. Standard v1 includes Kademlia support, while operators can still compose/omit providers per deployment; cache/mDNS/static retain their original roles.

## Security implications

mDNS, cache, and static-bootstrap candidates are untrusted reachability input. Static configuration is not implicit trust; ConnectionManager does not establish ordinary v1 data-plane connectivity until the PeerId is separately allowlisted. Provider/resource caps apply.

## Operational implications

Remote deployments need static addresses/relay planning plus out-of-band trusted PeerIds. LAN discovery can find peers without infrastructure, but trust still gates connection admission.

## Implementation implications

Each provider gets its own crate/module behind discovery-api and must pass common conformance tests.

## Revisit conditions

Superseded rollout question is closed by ADR-0034. Revisit only if evidence requires changing the provider set or default-on Kademlia posture.
