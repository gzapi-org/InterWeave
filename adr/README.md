# ADR index

All ADRs are **Accepted** architecture decisions unless later superseded.

| ADR | Decision |
|---|---|
| [0001](./0001-system-boundaries.md) | Keep four explicit layers: Claude Code, Channel MCP bridge, generic transport runtime, and network backend. |
| [0002](./0002-claude-channel-integration.md) | Use the current Claude Code Channel contract: stdio MCP server, `claude/channel`, push notifications, ordinary outbound tools, explicit instructions, and pre-delivery admission. |
| [0003](./0003-libp2p-backend.md) | Select rust-libp2p as the first transport backend behind neutral contracts. |
| [0004](./0004-gossipsub-broadcast.md) | Use signed GossipSub for broadcast with explicit application validation-result mapping from ADR-0029. |
| [0005](./0005-directed-messaging.md) | Use rust-libp2p `request_response`; endpoint-aware implementation target is `/claude-p2p-channel/direct/2.0.0` per ADR-0030. |
| [0006](./0006-discovery-provider-abstraction.md) | Define an event-stream-oriented `DiscoveryProvider` contract consumed only by DiscoveryManager. |
| [0007](./0007-discovery-composition.md) | Run enabled providers concurrently under DiscoveryManager and merge by PeerId/address provenance. |
| [0008](./0008-discovery-v1-providers.md) | Minimum v1 discovery is PeerCacheDiscovery, optional MdnsDiscovery, and StaticBootstrapDiscovery. |
| [0009](./0009-kademlia-role.md) | Kademlia is fully designed as optional peer-routing discovery, remains disabled by default, and never bypasses trust or stores channel/application records. |
| [0010](./0010-bootstrap-semantics.md) | Treat static bootstrap entries as reachability candidates only, never authority or implicit trust. |
| [0011](./0011-discovery-connection-ownership.md) | Discovery owns candidates; ConnectionManager owns connection policy, enforced for explicit and behaviour-originated Swarm dials. |
| [0012](./0012-trust-vs-discovery.md) | Use deny-by-default static PeerId trust for data-plane connection/source/outbound admission; endpoint policy may only narrow it. |
| [0013](./0013-transport-security.md) | Use rust-libp2p Noise XX to authenticate PeerIds and encrypt TCP connections. |
| [0014](./0014-group-encryption.md) | Defer group E2EE; plaintext broadcast confidentiality is bounded by the trusted data-plane peer set, and trusted forwarding peers can read payloads. |
| [0015](./0015-embedded-vs-daemon.md) | Use a separate profile-scoped Rust transport daemon behind local IPC. |
| [0016](./0016-multiple-instances-per-host.md) | Historical profile identity decision retained; v1 direct fan-out semantics are superseded by ADR-0030 endpoint routing. |
| [0017](./0017-local-ipc.md) | Use owner-protected UDS/named pipe, length-prefixed JSON, 128 KiB IPC v2 bodies, exclusive endpoint leases, and capability-scoped administration. |
| [0018](./0018-delivery-semantics.md) | Realtime/best-effort only; direct v2 acceptance means admission to one resolved endpoint queue, not application processing. |
| [0019](./0019-duplicate-suppression.md) | Use bounded ephemeral dedup; direct v2 keys include source endpoint and destination selector and retain first accepted route. |
| [0020](./0020-no-offline-store.md) | Do not persist application messages for later network, endpoint, Claude, or human delivery. |
| [0021](./0021-rust-workspace.md) | Separate neutral endpoint-aware contracts, runtime/EndpointRegistry, libp2p, IPC, daemon/CLI, and application adapters. |
| [0022](./0022-discovery-upgradeability.md) | Use compile-time provider registration plus typed namespaced configuration; no dynamic shared-library loading in v1. |
| [0023](./0023-claude-tool-surface.md) | Keep seven Claude tools; `send` gains optional remote EndpointId and bridge source route comes from IPC lease. |
| [0024](./0024-reachability-scope.md) | Guarantee directly reachable TCP/configured/cache paths and optional LAN mDNS; defer universal NAT traversal. |
| [0025](./0025-channel-id-topic-mapping.md) | Use 1..128-byte ASCII ChannelIds and deterministic domain-separated SHA-256 topic mapping. |
| [0026](./0026-backpressure-limits.md) | Bound payloads, endpoint/directory state, IPC frames/queues/clients, discovery state, and direct concurrency. |
| [0027](./0027-peer-cache-ownership.md) | Persist reachability observations only through PeerCacheDiscovery. |
| [0028](./0028-configuration-state-separation.md) | Keep endpoint definitions in config while endpoint leases/directory cache remain ephemeral runtime state. |
| [0029](./0029-gossipsub-validation-trust-mapping.md) | Map GossipSub `Reject` to objective invalidity, `Ignore` to valid-but-locally-unauthorized original publishers, and `Accept` to valid authorized publishers. |
| [0030](./0030-local-endpoint-addressing.md) | Model B: one PeerId per profile with exclusive configured EndpointId leases and deterministic direct v2 routing. |
| [0031](./0031-endpoint-directory.md) | Add an optional trust-gated, opt-in remote directory of currently active advertised EndpointIds. |
| [0032](./0032-human-client-boundary.md) | Keep the human client above transport as an IPC v2 endpoint consumer with separate administrative authority and application-owned human/chat state. |
