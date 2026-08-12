# Component boundaries

| Component | Owns | Must not own |
|---|---|---|
| Claude Channel bridge | MCP Channel capability, tools, event translation, instructions, one local EndpointId lease, reply tokens | discovery, dialing, keys, GossipSub mesh, endpoint/trust administration |
| Human client data plane | human UI transport operations, one local EndpointId lease, application-local history/rendering | libp2p Swarm, transport private key, implicit trust mutation |
| Human/admin settings path | explicit local trust/config/endpoint administration with granted admin capability | automatic actions triggered by network payloads |
| IPC server | local connection auth/handshake, capability grants, endpoint lease connection lifecycle, bounded per-client queues | peer discovery, application semantics |
| EndpointRegistry (runtime) | configured endpoint set, exclusive leases, default route, endpoint policy intersection, local route admission | human/application identity, libp2p protocol mechanics |
| Transport runtime | neutral command/event semantics, orchestration, endpoint-aware direct admission, health | Claude-specific prompts/tools |
| DiscoveryManager | provider lifecycle, candidate aggregation/provenance/expiry | trust grants, dialing, endpoint discovery |
| ConnectionManager | candidate addresses, dial/backoff/connection limits/retention | peer discovery, application payloads |
| PeerTrustPolicy | profile peer admission decision | discovery, endpoint naming, human identity |
| PubSubManager | ChannelId/topic mapping, GossipSub publish/validation/subscriptions | directed endpoint routing |
| DirectManager | request-response v2 lifecycle, codec, transport acceptance | local endpoint ownership policy, application acknowledgement |
| Endpoint directory manager | trust-gated advertised route snapshot/query/cache | app labels, human identity, DHT records |
| IdentityManager | persistent Ed25519 profile key, PeerId, rotation, exact-key recovery boundary | endpoint identities, application identity, online mnemonic export |
| rust-libp2p backend | Swarm, TCP/Noise/Yamux, Identify, GossipSub, direct/endpoints protocols | Claude semantics |

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

A human client may implement application-level contacts, display names, avatars, local message history, unread state, reactions, or a richer chat payload protocol. None of those concepts belongs in `transport-api` or EndpointId.
