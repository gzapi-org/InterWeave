# ADR index

All ADRs are **Accepted** architecture decisions unless later superseded.

| ADR | Decision |
|---|---|
| [0001](./0001-system-boundaries.md) | Keep four explicit layers: Claude Code, Channel MCP bridge, generic transport runtime, and network backend. |
| [0002](./0002-claude-channel-integration.md) | Use the current Claude Code Channel contract: stdio MCP server, `claude/channel`, push notifications, ordinary outbound tools, explicit instructions, and pre-delivery admission. |
| [0003](./0003-libp2p-backend.md) | Select rust-libp2p as the first transport backend behind neutral contracts. |
| [0004](./0004-gossipsub-broadcast.md) | Use signed GossipSub for broadcast with explicit application validation-result mapping from ADR-0029. |
| [0005](./0005-directed-messaging.md) | Use rust-libp2p `request_response` at `/claude-p2p-channel/direct/1.0.0` for one-to-one delivery with transport acceptance responses. |
| [0006](./0006-discovery-provider-abstraction.md) | Define an event-stream-oriented `DiscoveryProvider` contract consumed only by DiscoveryManager. |
| [0007](./0007-discovery-composition.md) | Run enabled providers concurrently under DiscoveryManager and merge by PeerId/address provenance. |
| [0008](./0008-discovery-v1-providers.md) | Minimum v1 discovery is PeerCacheDiscovery, optional MdnsDiscovery, and StaticBootstrapDiscovery. |
| [0009](./0009-kademlia-role.md) | Kademlia is fully designed as optional peer-routing discovery, remains disabled by default, and never bypasses trust or stores channel/application records. |
| [0010](./0010-bootstrap-semantics.md) | Treat static bootstrap entries as reachability candidates only, never authority or implicit trust. |
| [0011](./0011-discovery-connection-ownership.md) | Discovery owns candidates; ConnectionManager owns connection policy, enforced for explicit and behaviour-originated Swarm dials. |
| [0012](./0012-trust-vs-discovery.md) | Use deny-by-default static PeerId trust for v1 data-plane connection, inbound source, and outbound direct-send admission. |
| [0013](./0013-transport-security.md) | Use rust-libp2p Noise XX to authenticate PeerIds and encrypt TCP connections. |
| [0014](./0014-group-encryption.md) | Defer group E2EE; plaintext broadcast confidentiality is bounded by the trusted data-plane peer set, and trusted forwarding peers can read payloads. |
| [0015](./0015-embedded-vs-daemon.md) | Use a separate profile-scoped Rust transport daemon behind local IPC. |
| [0016](./0016-multiple-instances-per-host.md) | Use one network identity per explicit transport profile; sharing is opt-in by selecting the same profile/socket. |
| [0017](./0017-local-ipc.md) | Use owner-protected UDS/named pipe, length-prefixed JSON, a 128 KiB v1 JSON-body ceiling, and capability-scoped administrative IPC. |
| [0018](./0018-delivery-semantics.md) | Define v1 as realtime/best-effort with no stronger delivery guarantees. |
| [0019](./0019-duplicate-suppression.md) | Use bounded ephemeral dedup keyed by mode/source/channel context/message ID. |
| [0020](./0020-no-offline-store.md) | Do not persist application messages for later network or Claude delivery. |
| [0021](./0021-rust-workspace.md) | Separate neutral contracts, discovery, trust, runtime, libp2p, IPC, daemon/CLI, and bridge layers. |
| [0022](./0022-discovery-upgradeability.md) | Use compile-time provider registration plus typed namespaced configuration; no dynamic shared-library loading in v1. |
| [0023](./0023-claude-tool-surface.md) | Expose `broadcast`, `send`, `reply`, `join`, `leave`, `identity`, and `status`; enforce current trust/subscription policy on outbound operations. |
| [0024](./0024-reachability-scope.md) | Guarantee directly reachable TCP/configured/cache paths and optional LAN mDNS; defer universal NAT traversal. |
| [0025](./0025-channel-id-topic-mapping.md) | Use 1..128-byte ASCII ChannelIds and deterministic domain-separated SHA-256 topic mapping. |
| [0026](./0026-backpressure-limits.md) | Bound payloads, IPC frames, queues, clients, candidates, addresses, and direct concurrency; report effective payload capability. |
| [0027](./0027-peer-cache-ownership.md) | Persist reachability observations only through PeerCacheDiscovery. |
| [0028](./0028-configuration-state-separation.md) | Separate profile config, private identity key, mutable state/logs, replaceable peer cache, and runtime endpoint. |
| [0029](./0029-gossipsub-validation-trust-mapping.md) | Map GossipSub `Reject` to objective invalidity, `Ignore` to valid-but-locally-unauthorized original publishers, and `Accept` to valid authorized publishers. |
