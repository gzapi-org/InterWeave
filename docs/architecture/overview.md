# Architecture overview

## System shape

```text
Claude Code <--stdio--> Claude bridge --IPC v2 DATA / EndpointId--\
                                                              \
Human client -------------------IPC v2 DATA / EndpointId-------> profile daemon / one PeerId
Human settings/admin ---------------- IPC v2 ADMIN ------------>/
                                                               |
                                                          +-- EndpointRegistry
                                                          +-- PeerTrustPolicy
                                                          +-- DiscoveryManager
                                                          +-- ConnectionManager
                                                          +-- GossipSub (broadcast)
                                                          +-- Direct v2 (peer + endpoint)
                                                          +-- Endpoint directory
                                                          +-- IdentityManager
                                                          |
                                                          v
                                                       libp2p
```

## Architectural invariants

1. Claude-facing code never depends directly on libp2p/discovery implementation details.
2. Discovery yields candidate reachability; it never grants trust.
3. Connection policy is centrally enforced, including behavior-originated dials.
4. Broadcast and direct communication use separate network mechanisms.
5. One profile owns one persistent PeerId; local EndpointIds route within it and are not identities.
6. Every direct-capable local IPC client owns at most one exclusive configured EndpointId lease.
7. Direct v2 resolves exactly one local endpoint; no hidden primary, round-robin, or all-client fan-out exists.
8. Endpoint policy may narrow but never widen profile trust.
9. `AcceptedV2` means enqueue into the resolved local endpoint queue, not application processing.
10. Offline endpoints have no daemon message queue.
11. Broadcast remains ChannelId/join-reference scoped and does not carry transport EndpointId routing.
12. Endpoint directory is optional, trust-gated, bounded, opt-in, and identity-agnostic.
13. Every queue is bounded, and every legal max-size payload fits IPC v2.
14. Standard v1 includes Kademlia support; configured entries default enabled but remain opt-out, and Kademlia stores no channel/endpoint/application records.
15. Human/chat semantics stay above the transport boundary.
16. GossipSub mesh duplicate identity binds signed source PeerId + GossipSub wire sequence number; application envelope IDs are never mesh cache keys.
17. IPC data/admin sockets are separate authority domains; `client.kind` cannot grant admin authority.
18. Internet listeners bound pre-Noise handshakes before PeerId exists; trusted direct ingress is token-bucket limited.
19. Dial identity mismatches penalize/quarantine the address, not an expected trusted PeerId while an eligible known-good address exists.


## Mandatory Internet reachability

Standard v1 includes AutoNAT v2 client, Circuit Relay v2 client/reservations, and DCUtR. Direct reachability evidence and relay readiness are normalized behind the generic transport status; Model B endpoints and Claude/human clients do not depend on libp2p-specific NAT APIs.

Connectivity infrastructure uses a protocol-scoped authorization class distinct from application `PeerTrustPolicy`. Relay/probe authorization never grants GossipSub, direct-message, endpoint-directory, Kademlia, Channel, or EndpointId authority.
