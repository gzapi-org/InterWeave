# Architecture requirement traceability

This matrix is an audit aid, not a replacement for the source task.

| Requirement | Primary location |
|---|---|
| Claude/transport/backend boundaries | `README.md`, `overview.md`, ADR-0001 |
| official Telegram architecture + implementation research | `research/telegram-plugin-*.md` |
| Telegram-to-P2P mapping | `research/telegram-to-p2p-mapping.md` |
| current Channel capability/notifications | `research/claude-code-channels.md`, `plugin/CLAUDE-CODE-CHANNEL.md` |
| broadcast = GossipSub | ADR-0004, `transport/libp2p/PUBSUB.md` |
| directed = dedicated direct protocol | ADR-0005, `transport/libp2p/DIRECT.md` |
| transport-neutral interface | `contracts/TRANSPORT.md` |
| DiscoveryProvider | `contracts/DISCOVERY.md`, ADR-0006 |
| discovery composition/priority | `discovery/COMPOSITION.md`, ADR-0007 |
| peer cache/mDNS/static/Kademlia | `discovery/providers/*`, ADR-0008/0009 |
| discovery != connection | ADR-0011, `components.md` |
| bootstrap non-authority | ADR-0010, static provider doc |
| trust != discovery | ADR-0012, threat model |
| persistent identity/rotation | `IDENTITY.md`, configuration docs |
| Noise boundary | ADR-0013, `SECURITY.md` |
| group encryption decision | ADR-0014 |
| embedded vs daemon | ADR-0015 |
| multi-instance host model | ADR-0016 |
| local IPC | `contracts/LOCAL-IPC.md`, ADR-0017 |
| delivery guarantees | ADR-0018/0019/0020 |
| Rust workspace | `rust-blueprint.md`, ADR-0021 |
| discovery upgradeability | ADR-0022 |
| Channel plugin surface | ADR-0023, `plugin/TOOL-SURFACE.md` |
| NAT/reachability | ADR-0024, `CONNECTIVITY.md` |
| ChannelId/topic model | ADR-0025 |
| backpressure/limits | ADR-0026, `resource-limits.md` |
| config/state/key/cache separation | ADR-0028, `configuration.md` |
| observability/metrics | `observability.md` |
| failure scenarios | `failure-model.md` |
| threat model | `threat-model.md` |
| incoming message safety / no automatic local actions | `plugin/SECURITY.md`, `plugin/INSTRUCTIONS.md` |
| alternatives | `research/alternatives.md` |
| provider conformance tests | `contracts/DISCOVERY-CONFORMANCE.md`, `testing.md` |
| phased roadmap/spikes | `roadmap/*` |
| final CTO-style review | `FINAL-REVIEW.md` |
