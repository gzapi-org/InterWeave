# ADR index

All ADRs are **Accepted** architecture decisions unless later superseded.

| ADR | Decision |
|---|---|
| [0001](./0001-system-boundaries.md) | Keep four explicit layers: Claude Code, Channel MCP bridge, generic transport runtime, and network backend. |
| [0002](./0002-claude-channel-integration.md) | Use the current Claude Code Channel contract: stdio MCP server, `claude/channel` capability, push `notifications/claude/channel`, ordinary tools for outbound actions, explicit Channel instructions, and sender/trust gating before notification delivery. |
| [0003](./0003-libp2p-backend.md) | Select rust-libp2p as the first transport backend, using TCP, Noise, Yamux, GossipSub, request-response, Identify, and pluggable discovery integration. |
| [0004](./0004-gossipsub-broadcast.md) | Map logical channels to domain-separated hashed GossipSub topics. |
| [0005](./0005-directed-messaging.md) | Use rust-libp2p `request_response` with protocol ID `/claude-p2p-channel/direct/1. |
| [0006](./0006-discovery-provider-abstraction.md) | Define an event-stream-oriented `DiscoveryProvider` contract consumed only by DiscoveryManager. |
| [0007](./0007-discovery-composition.md) | Run enabled providers concurrently under DiscoveryManager. |
| [0008](./0008-discovery-v1-providers.md) | Ship the architecture for PeerCacheDiscovery, optional MdnsDiscovery, and StaticBootstrapDiscovery as the minimum v1 provider set. |
| [0009](./0009-kademlia-role.md) | Do not require Kademlia in v1. |
| [0010](./0010-bootstrap-semantics.md) | Treat every static bootstrap entry as a normal DiscoveryProvider candidate. |
| [0011](./0011-discovery-connection-ownership.md) | DiscoveryManager owns candidate knowledge. |
| [0012](./0012-trust-vs-discovery.md) | Use a `PeerTrustPolicy` abstraction. |
| [0013](./0013-transport-security.md) | Use rust-libp2p Noise with the interoperable XX profile to authenticate PeerIds and encrypt TCP connections. |
| [0014](./0014-group-encryption.md) | v1 uses trusted data-plane peers plus Noise-encrypted links and explicitly does not promise end-to-end secrecy from GossipSub forwarding peers. |
| [0015](./0015-embedded-vs-daemon.md) | Select Architecture B: Claude MCP Channel bridge connects over local IPC to a separate Rust transport daemon. |
| [0016](./0016-multiple-instances-per-host.md) | Default to one network identity per named transport profile, not per Claude conversation and not one implicit host identity. |
| [0017](./0017-local-ipc.md) | Use Unix domain sockets on Unix-like systems and named pipes on Windows, owner-restricted. |
| [0018](./0018-delivery-semantics.md) | Define v1 as best effort. |
| [0019](./0019-duplicate-suppression.md) | Maintain a runtime-local LRU/TTL cache keyed by `(source_peer,message_id,mode/channel context)` with default 10,000 entries and 5-minute TTL. |
| [0020](./0020-no-offline-store.md) | Do not write application messages to disk for later network or Claude delivery. |
| [0021](./0021-rust-workspace.md) | Plan separate crates for neutral contracts, discovery API/providers, trust policy, runtime orchestration, libp2p backend, IPC, daemon CLI, and a bridge adapter. |
| [0022](./0022-discovery-upgradeability.md) | Support replaceability through a Rust trait, compile-time provider registry, namespaced typed config, and config-driven composition. |
| [0023](./0023-claude-tool-surface.md) | Expose `broadcast`, `send`, `reply`, `join`, `leave`, `identity`, and `status` as the minimal conceptual MCP tools. |
| [0024](./0024-reachability-scope.md) | Guarantee directly reachable TCP peers, configured/cache-discovered addresses, and optional LAN mDNS. |
| [0025](./0025-channel-id-topic-mapping.md) | Define ChannelId as 1. |
| [0026](./0026-backpressure-limits.md) | Adopt explicit defaults: 48 KiB payload; 128-byte ChannelId; 1024 backend/runtime events; 256 events per IPC client; 16 IPC clients; 64 outstanding commands/client; 128 global direct sends; 8 direct sends/peer; 4096 candidates; 16 addresses/peer. |
| [0027](./0027-peer-cache-ownership.md) | Persist known reachable peers only through PeerCacheDiscovery. |
| [0028](./0028-configuration-state-separation.md) | Use profile-specific platform directories for normal configuration, private identity key, mutable daemon state/logs, replaceable peer cache, and runtime socket/lock. |
