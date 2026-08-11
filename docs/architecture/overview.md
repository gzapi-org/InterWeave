# Architecture overview

## Scope

The system is a local Claude Code Channel bridge plus a generic P2P transport runtime. It transports payloads and routing metadata. It does not interpret application semantics.

## Context

```text
remote peer(s)
     ^
     | encrypted libp2p connections
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
|   `- KademliaDiscovery (deferred)        |
|                                          |
|  PeerTrustPolicy                         |
+--------------------+---------------------+
                     | local IPC
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
3. Only the ConnectionManager decides whether/when to dial.
4. Broadcast and direct traffic are distinct protocol paths.
5. Trust admission occurs before local Claude Channel delivery.
6. Every queue is bounded.
7. No persistent message mailbox exists in v1.
8. Network identity belongs to a profile/daemon, not a Claude conversation.
9. A bootstrap peer is only a reachability hint.
10. The bridge may restart without rotating PeerId or restarting the network.

## Capability statement

v1 capabilities exposed through the transport contract:

- `broadcast`: yes, realtime/best-effort;
- `direct_delivery`: yes, realtime/best-effort with transport-level acceptance response;
- `durable_delivery`: no;
- `offline_mailbox`: no.
