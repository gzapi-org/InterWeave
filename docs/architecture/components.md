# Component boundaries

| Component | Owns | Must not own |
|---|---|---|
| Claude Channel bridge | MCP Channel capability, tools, event translation, instructions, reply tokens | discovery, dialing, keys, GossipSub mesh |
| Transport API | neutral commands/events/errors/capabilities | libp2p types, Claude types |
| Transport runtime | command/event orchestration, admission pipeline, bounded queues | application meaning |
| Libp2p backend | Swarm, connections, GossipSub, direct protocol, Noise, Identify, optional Kademlia driver/behavior slot | Claude MCP, app roles |
| DiscoveryManager | provider lifecycle, candidate merge/provenance/expiry/health | dialing, trust, pubsub |
| DiscoveryProvider | source-specific candidate discovery; optional Kademlia provider owns only scheduling/normalization/health | dialing, trust, messaging, Swarm ownership |
| ConnectionManager | trust-gated dial/inbound-retain decisions, reconnect/backoff/limits/address selection | discovery mechanism, payload interpretation |
| PeerTrustPolicy | authorize PeerIds for v1 data-plane connection/message/send decisions | discovery, Swarm execution |
| IdentityManager | persistent private key, PeerId, rotation workflow | app identity claims |
| Peer cache writer | persist successful/recent observations as advisory hints | authority/trust |
| IPC server | local multiplexing, framing, versioning, OS-level client checks, capability grants | Claude semantics |
| Diagnostics | health/counters/sanitized events | secrets or payload logging by default |

## Why these abstractions exist

- `Transport`: the Claude-facing consumer must vary independently from networking backends.
- `DiscoveryProvider`: discovery sources are explicitly required to vary independently and compose.
- `PeerTrustPolicy`: trust mechanisms are expected to evolve without changing discovery; v1 consumes the same policy at connection admission, outbound direct dispatch, GossipSub source validation, and local delivery.

`ConnectionPolicy` and `PubSub` are **not public traits in v1**. They are internal modules of the libp2p backend because no second implementation consumer exists yet. Promote them only if a real independent variation point appears.


## Optional Kademlia split

Kademlia spans two components without collapsing their ownership boundary: `KademliaDiscovery` is a `DiscoveryProvider`, while `transport-libp2p` owns the concrete `libp2p::kad::Behaviour` inside the single Swarm task. A bounded backend-internal `KadControlHandle` connects them. This is a mechanism-specific adapter, not a new generic API.
