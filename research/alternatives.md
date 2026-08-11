# Alternatives analysis

The comparison is about a **generic Claude transport layer**, not whether each system is useful in its own domain.

| Option | Broadcast | Identity/security | Decentralization | Discovery/NAT | Offline delivery | Infra / operations | Rust fit | Assessment |
|---|---|---|---|---|---|---|---|---|
| libp2p + GossipSub | native mesh pub/sub | peer keys + Noise | high | flexible but complex | no by default | no required broker | strong | **selected initial backend** |
| Nostr | relay fanout/subscriptions | signed public-key events | federated relays | relay discovery, no direct NAT problem | relay-dependent | relay infrastructure | viable | event/relay model adds semantics and central relay dependencies we do not need |
| Matrix | rooms/federation | server/user identity, optional E2EE | federated | homeserver routing | strong history/sync | substantial homeserver stack | viable clients | much heavier application/membership/history model than transport requires |
| NATS Core | excellent subjects/pubsub | server auth + TLS | server/cluster based | DNS/server discovery | Core: best-effort/at-most-once | NATS servers | strong | operationally simple but introduces broker authority/failure domain |
| Redis Pub/Sub | simple channels | Redis auth/TLS | central/sharded service | service endpoint | at-most-once, no backlog | Redis infrastructure | strong | simple but centralized and no P2P identity/discovery |
| MQTT | broker topics/QoS | broker auth + TLS | brokered | broker endpoint | QoS/session features | MQTT broker | strong | mature but broker-centric and semantics exceed/reshape P2P requirement |
| custom WebSocket broker | can implement | must design | centralized | server endpoint | custom | build/operate broker | strong | needless custom protocol + central service |
| Telegram-style central channel | group/DM built in | platform identity/TLS | centralized platform | platform solves | platform history varies | third-party platform | SDK ecosystem | excellent reference for Claude integration, wrong network architecture |

## Why libp2p despite complexity

The required system explicitly needs decentralized peer identity, direct 1:1 transport, mesh broadcast, and replaceable discovery. libp2p packages those concerns as interoperable protocols rather than forcing a custom overlay. Its main cost is operational/network complexity: NAT behavior, peer-quality attacks, and discovery tuning remain real work.

The architecture contains that cost below a generic `Transport` contract so a future backend can replace libp2p without changing the Claude Channel contract.
