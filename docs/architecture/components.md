# Component boundaries

| Component | Owns | Must not own |
|---|---|---|
| Claude Channel bridge | MCP Channel capability, tools, event translation, instructions, reply tokens | discovery, dialing, keys, GossipSub mesh |
| Transport API | neutral commands/events/errors/capabilities | libp2p types, Claude types |
| Transport runtime | command/event orchestration, admission pipeline, bounded queues | application meaning |
| Libp2p backend | Swarm, connections, GossipSub, direct protocol, Noise, Identify | Claude MCP, app roles |
| DiscoveryManager | provider lifecycle, candidate merge/provenance/expiry/health | dialing, trust, pubsub |
| DiscoveryProvider | source-specific candidate discovery | dialing, trust, messaging |
| ConnectionManager | dial/reconnect/backoff/limits/address selection | discovery mechanism, payload interpretation |
| PeerTrustPolicy | admit transport PeerIds / optional channel-scope decision | discovery, network control |
| IdentityManager | persistent private key, PeerId, rotation workflow | app identity claims |
| Peer cache writer | persist successful/recent observations as advisory hints | authority/trust |
| IPC server | local multiplexing, framing, versioning, client auth by OS permissions | Claude semantics |
| Diagnostics | health/counters/sanitized events | secrets or payload logging by default |

## Why these abstractions exist

- `Transport`: the Claude-facing consumer must vary independently from networking backends.
- `DiscoveryProvider`: discovery sources are explicitly required to vary independently and compose.
- `PeerTrustPolicy`: trust mechanisms are expected to evolve without changing discovery.

`ConnectionPolicy` and `PubSub` are **not public traits in v1**. They are internal modules of the libp2p backend because no second implementation consumer exists yet. Promote them only if a real independent variation point appears.
