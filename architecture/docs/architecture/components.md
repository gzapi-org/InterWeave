# Component boundaries

| Component | Owns | Must not own |
|---|---|---|
| Claude Channel bridge | MCP Channel capability, tools, event translation, instructions, one local EndpointId lease, reply tokens | discovery, dialing, keys, GossipSub mesh, endpoint/trust administration |
| Human client shared core | Rust contacts/conversations/HumanChatV2/ADR-0044 retention/Slint view model | trust grants, transport key, discovery/connectivity policy |
| Desktop human binding | IPC v2 data session + optional separate admin connection | libp2p Swarm, transport private key |
| Android human binding | embedded LocalDataSession in foreground-service-owned Rust runtime; platform lifecycle bridge | independent second Swarm, hidden durable mailbox |
| Human/admin settings path | explicit local trust/config/endpoint administration (desktop admin socket; Android LocalAdminPort) | automatic actions triggered by network payloads |
| IPC server | separate data/admin socket acceptors, authority-domain tagging, local handshake/capability grants, endpoint lease lifecycle on data socket, bounded per-client queues | peer discovery, application semantics |
| EndpointRegistry (runtime) | configured endpoint set, exclusive leases, default route, endpoint policy intersection, local route admission | human/application identity, libp2p protocol mechanics |
| Transport runtime | neutral command/event semantics, orchestration, endpoint-aware direct admission, health | Claude-specific prompts/tools |
| DiscoveryManager | provider lifecycle, candidate aggregation/provenance/expiry | trust grants, dialing, endpoint discovery |
| ConnectionManager | candidate/path selection, connection class, dial-origin admission, backoff/connection limits/retention, direct-vs-relay preference | peer discovery, application payloads |
| PeerTrustPolicy | application data-plane PeerId admission decision | discovery, connectivity-infrastructure roles, endpoint naming, human identity |
| PubSubManager | ChannelId/topic mapping, GossipSub publish/validation/subscriptions | directed endpoint routing |
| DirectManager | request-response v2 lifecycle, codec, transport acceptance | local endpoint ownership policy, application acknowledgement |
| Endpoint directory manager | trust-gated advertised route snapshot/query/cache | app labels, human identity, DHT records |
| ReachabilityManager | AutoNAT-v2 evidence aggregation, direct/relay reachability status, reservation target | peer trust, discovery, endpoint routing |
| RelayManager | authorized relay candidates, reservations/failover, ephemeral relay addresses, optional server-role capacity | trust grants, discovery, application routing |
| DCUtRManager | bounded relayed-to-direct upgrade attempts/cooldown and success handoff | application retry, trust grants |
| ConnectivityInfrastructurePolicy | protocol-scoped relay/AutoNAT infrastructure authorization | GossipSub/direct/endpoint/Kademlia application trust |
| IdentityManager | persistent Ed25519 profile key, PeerId, rotation, exact-key recovery boundary | endpoint identities, application identity, online mnemonic export |
| rust-libp2p backend | Swarm, TCP/Noise/Yamux, Identify, mandatory AutoNAT-v2/relay-v2/DCUtR, GossipSub, direct/endpoints protocols, Kademlia driver | Claude semantics |

## Endpoint ownership split

Endpoint routing crosses three internal layers deliberately:

```text
IPC connection
   |
   | claims configured EndpointId
   v
EndpointRegistry / transport-runtime
   |
   | local route admission
   v
DirectManager <----> direct v2 network protocol
```

The libp2p backend carries EndpointIds on direct frames but does not decide which local process owns them. IPC server owns socket connections but does not decide remote endpoint policy. EndpointRegistry is the single local routing authority.

## Human client boundary

A human client may implement application-level contacts, display names, avatars, pending/unread/receiver-kept application retention, read state, reactions, or a richer chat payload protocol. None of those concepts belongs in `transport-api` or EndpointId.

## Reachability ownership split

```text
Discovery candidates / Identify observations
              |
              v
       AddressRegistry
              |
   +----------+-----------+
   |          |           |
AutoNAT   RelayManager  ConnectionManager
   |          |           |
   +------ Reachability ---+
              |
            DCUtR
              |
              v
          Swarm paths
```

Relay/AutoNAT service authorization is a control-plane policy, not a DiscoveryProvider capability and not application `PeerTrustPolicy`.
