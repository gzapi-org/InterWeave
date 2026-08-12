# ADR index

All ADRs are **Accepted** architecture decisions unless later superseded.

| ADR | Decision |
|---|---|
| [0001](./0001-system-boundaries.md) | Keep four explicit layers: Claude Code, Channel MCP bridge, generic transport runtime, and network backend. |
| [0002](./0002-claude-channel-integration.md) | Use the current Claude Code Channel contract: stdio MCP server, `claude/channel`, push notifications, ordinary outbound tools, explicit instructions, and pre-delivery admission. |
| [0003](./0003-libp2p-backend.md) | Select rust-libp2p as the first transport backend behind neutral contracts. |
| [0004](./0004-gossipsub-broadcast.md) | Use signed GossipSub for broadcast with explicit application validation-result mapping from ADR-0029. |
| [0005](./0005-directed-messaging.md) | Use rust-libp2p `request_response`; endpoint-aware implementation target is `/interweave/direct/2.0.0` per ADR-0030. |
| [0006](./0006-discovery-provider-abstraction.md) | Define an event-stream-oriented `DiscoveryProvider` contract consumed only by DiscoveryManager. |
| [0007](./0007-discovery-composition.md) | Run enabled providers concurrently under DiscoveryManager and merge by PeerId/address provenance. |
| [0008](./0008-discovery-v1-providers.md) | Historical minimum provider-set rollout; superseded in part by ADR-0034, while cache/mDNS/static roles remain accepted. |
| [0009](./0009-kademlia-role.md) | Kademlia integration/security: private trust-bounded peer routing, no records, no trust bypass; default rollout superseded by ADR-0034. |
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
| [0024](./0024-reachability-scope.md) | Historical conservative reachability scope; superseded by ADR-0035 mandatory Internet reachability. |
| [0025](./0025-channel-id-topic-mapping.md) | Use 1..128-byte ASCII ChannelIds and deterministic domain-separated SHA-256 topic mapping. |
| [0026](./0026-backpressure-limits.md) | Bound payloads, endpoint/directory state, IPC frames/queues/clients, discovery state, and direct concurrency. |
| [0027](./0027-peer-cache-ownership.md) | Persist reachability observations only through PeerCacheDiscovery. |
| [0028](./0028-configuration-state-separation.md) | Keep endpoint definitions in config while endpoint leases/directory cache remain ephemeral runtime state. |
| [0029](./0029-gossipsub-validation-trust-mapping.md) | Map GossipSub `Reject` to objective invalidity, `Ignore` to valid-but-locally-unauthorized original publishers, and `Accept` to valid authorized publishers. |
| [0030](./0030-local-endpoint-addressing.md) | Model B: one PeerId per profile with exclusive configured EndpointId leases and deterministic direct v2 routing. |
| [0031](./0031-endpoint-directory.md) | Add an optional trust-gated, opt-in remote directory of currently active advertised EndpointIds. |
| [0032](./0032-human-client-boundary.md) | Keep human application semantics above transport; desktop binds through IPC, Android through an embedded local-session adapter, with separate admin authority. |
| [0033](./0033-identity-recovery-mnemonic.md) | Use Ed25519 software identities with optional offline 24-word BIP-39 entropy encoding of the exact secret seed for same-PeerId recovery; no wallet PBKDF2 or IPC exposure. |
| [0034](./0034-kademlia-default-enabled.md) | Standard v1 includes Kademlia support and configured Kademlia entries default enabled; operators may explicitly opt out. |
| [0035](./0035-mandatory-internet-reachability.md) | Standard v1 requires AutoNAT v2 client, Circuit Relay v2 client/reservations, and DCUtR; Phase 9 is a release requirement. |
| [0036](./0036-connectivity-infrastructure-peer-class.md) | Authorize relay/AutoNAT infrastructure through a protocol-scoped connection class that does not grant application data-plane trust. |
| [0037](./0037-split-local-admin-socket.md) | Split IPC data-plane and administrative authority onto separate local sockets; client.kind never grants admin authority. |
| [0038](./0038-optional-encrypted-identity-at-rest.md) | Keep v1 filesystem-only key storage while defining a SPIKE-007-gated audited passphrase-encrypted key envelope as an explicit v2.x option. |
| [0039](./0039-rust-human-client-slint.md) | First-party human clients share a Rust core and use Slint as the reference desktop/Android UI. |
| [0040](./0040-desktop-human-daemon-ipc.md) | Desktop human client uses the shared daemon, IPC v2 data socket, and separate admin socket. |
| [0041](./0041-android-embedded-runtime.md) | Android embeds TransportRuntime in a foreground-service host and uses the in-process local-session binding. |
| [0042](./0042-android-key-wrapping.md) | Android Keystore AES-GCM wraps the exact portable Ed25519 seed without changing PeerId recovery. |
| [0043](./0043-multi-device-peer-identity.md) | Concurrent desktop/Android devices use distinct PeerIds; mnemonic restore is migration/recovery, not cloning. |
| [0044](./0044-human-message-retention.md) | Human message content is durable only while outbound-pending, inbound-unread, or receiver-kept-after-read; ordinary delivered/read history evaporates. |
| [0045](./0045-implementation-repository-layout.md) | Separate specifications under `architecture/`, thin applications under `apps/`, reusable crates under grouped `crates/`, and place tests/fixtures/spikes by proof scope. |
| [0046](./0046-bottom-up-implementation-order.md) | Implement bottom-up through dependency gates: contracts/state/persistence before networking, root dial admission before autonomous libp2p behaviours, then runtime/clients/platforms. |
| [0047](./0047-interweave-project-and-wire-namespace.md) | Adopt InterWeave as the canonical project and machine namespace; replace the pre-implementation working identifiers and re-freeze affected hash vectors. |
