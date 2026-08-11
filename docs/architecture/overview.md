# Architecture overview

## Scope

The system is a local Claude Code Channel bridge plus a generic P2P transport runtime. It transports payloads and routing metadata. It does not interpret application semantics.

## Context

```text
remote trusted data-plane peer(s)
     ^
     | Noise-authenticated + PeerTrustPolicy-admitted connections
     v
+------------------------------------------+
| profile-scoped transport daemon          |
|                                          |
|  Libp2pBackend                           |
|   |- GossipSub broadcast                 |
|   |- Direct request-response             |
|   |- ConnectionManager                   |
|   |- IdentityManager                     |
|                                          |
|  DiscoveryManager                        |
|   |- PeerCacheDiscovery                  |
|   |- MdnsDiscovery (optional)            |
|   |- StaticBootstrapDiscovery            |
|   `- KademliaDiscovery (optional/default-off)        |
|                                          |
|  PeerTrustPolicy                         |
+--------------------+---------------------+
                     | owner-protected, capability-scoped local IPC
                     v
+------------------------------------------+
| Claude P2P Channel bridge                |
| Channel notifications + MCP tools        |
+--------------------+---------------------+
                     | stdio MCP
                     v
+------------------------------------------+
| Claude Code                              |
+------------------------------------------+
```

## Invariants

1. Claude receives no libp2p internal type.
2. A discovery event cannot authorize a peer.
3. Only ConnectionManager decides whether/when to dial, and v1 ordinary data-plane dialing/retention requires PeerTrustPolicy authorization.
4. Broadcast and direct traffic are distinct protocol paths.
5. Trust authorization applies before outbound direct dial, ordinary data-plane connection retention, GossipSub source propagation/delivery, and local Claude Channel delivery.
6. GossipSub objective invalidity (`Reject`) is distinct from local authorization failure (`Ignore`).
7. Every queue is bounded, and every legal max-size transport payload fits the fixed v1 IPC frame.
8. No persistent message mailbox exists in v1.
9. Network identity belongs to a profile/daemon, not a Claude conversation.
10. A bootstrap peer is only a reachability hint and is never implicit trust.
11. The bridge may restart without rotating PeerId or restarting the network.
12. A Claude Channel IPC client cannot invoke administrative daemon shutdown.

## Capability statement

v1 capabilities exposed through the transport contract:

- `broadcast`: yes, realtime/best-effort, caller must be locally joined;
- `direct_delivery`: yes, trusted target only, realtime/best-effort with transport-level acceptance response;
- `durable_delivery`: no;
- `offline_mailbox`: no;
- `max_payload_bytes`: effective active-profile value, hard ceiling 49,152 bytes.


## Optional Kademlia integration

Kademlia remains disabled by default but is now fully specified as a private, trust-bounded, peer-routing-only DiscoveryProvider. The Swarm owns the concrete Kademlia behavior; the provider owns scheduling/normalization through a bounded internal handle. See [kademlia-integration.md](kademlia-integration.md) and ADR-0009.
