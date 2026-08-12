# claude-p2p-channel

Architecture and contracts for a generic peer-to-peer **Claude Code Channel transport**, Model B local endpoint multiplexing, and first-party Rust human clients for desktop and Android.

> Status: architecture only. This repository intentionally contains no production MCP server, libp2p networking, daemon, human client, installer, system service, or Rust crate implementation.

## Purpose

`claude-p2p-channel` is a transport plugin, not an application coordination or chat protocol. It gives Claude Code and other local clients a decentralized, payload-agnostic transport. Higher-level systems may carry text, JSON, chat envelopes, or their own protocols, but this project does not define agent roles, task state, repositories, Git semantics, human identity, social graphs, read receipts, or application workflows.

## Architecture

```text
Desktop/server
  Human Slint -- IPC v2 --\
  Claude bridge -- IPC v2 ---+--> profile TransportRuntime/daemon --> libp2p
                              |        one PeerId, many EndpointIds
                              `-- admin socket (explicit local settings only)

Android
  Slint Activity --> LocalDataSession --> foreground Service host
                                          |
                                          `--> same Rust TransportRuntime --> libp2p
                                               one device PeerId / human EndpointId

Network side on both:
  GossipSub broadcast
  DirectMessageV2 PeerId + EndpointId
  Kademlia (standard v1, configured default-on)
  AutoNAT v2 + Circuit Relay v2 + DCUtR (mandatory v1)
  Trust + connectivity-infrastructure classes + root DialAdmissionGate
```

`TransportRuntime` owns the private key and all libp2p state: an external daemon hosts it on desktop/server, while the Android foreground service hosts it in-process. Presentation/application code never becomes an independent network identity unless it uses a separate profile.

## Decision summary

- Claude integration follows the official Channel pattern: stdio MCP, `claude/channel`, push notifications, explicit outbound tools, and pre-delivery admission.
- Desktop/server use a **separate, profile-scoped daemon**. Android embeds the same Rust `TransportRuntime` in a user-visible foreground-service host; both preserve the same PeerId/EndpointId/network contracts.
- One profile owns one persistent PeerId. Model B adds configured **EndpointIds** underneath that PeerId for deterministic local direct routing (`human`, `claude`, `automation.build`, etc.). EndpointId is a routing selector, not cryptographic/human/application identity.
- Initial software identities are **Ed25519**. An optional offline 24-word recovery record can reproduce the exact same PeerId by encoding the raw 32-byte Ed25519 secret with BIP-39 entropy/checksum/English-word mapping only; wallet PBKDF2/passphrase semantics are not used and recovery material never crosses IPC.
- Every direct-capable local data-plane session owns one exclusive configured endpoint lease (IPC v2 connection on desktop, embedded service session on Android). Direct messages route to exactly one endpoint; the previous architecture-only all-client fan-out is superseded by ADR-0030.
- Direct protocol target is `/claude-p2p-channel/direct/2.0.0`, carrying required source endpoint and optional destination endpoint. Omitted destination resolves the receiver's explicit `default_direct_endpoint`; it never means fan-out.
- Direct `Accepted` means the resolved endpoint's bounded local event queue accepted the message, not that Claude or a human processed it.
- An optional trust-gated endpoint-directory protocol exposes only active routes explicitly marked `advertise: true`; it returns route names only, never human names/roles/trust claims.
- Endpoint-specific ACLs may narrow profile trust but can never widen it. Remote endpoint denial is exposed as coarse `no_route` / local `RemoteEndpointUnavailable` to avoid an authorization oracle.
- Broadcast remains GossipSub and ChannelId-scoped; endpoint addressing does not alter broadcast envelopes or subscription semantics.
- `Transport`, `DiscoveryProvider`, and trust boundaries remain independent of Claude/libp2p details.
- Kademlia has a complete peer-routing integration blueprint and, per ADR-0034, configured entries are **`enabled: true` by default** in the standard v1 build. Operators may explicitly opt out. It never grants trust or stores application/channel/endpoint records.
- Per ADR-0035, standard v1 also includes the **mandatory Internet-reachability stack**: AutoNAT v2 client, Circuit Relay v2 client/reservation management, and DCUtR. Relay/AutoNAT server roles are explicit infrastructure modes. Phase 9 is a release requirement, not conditional hardening.
- Relay/AutoNAT infrastructure can be authorized through `transport.connectivity.infrastructure.allowed_peers` without entering application `trust.allowed_peers`; ADR-0036 prevents that control-plane connection from gaining GossipSub/direct/endpoint/Kademlia authority.
- Discovery only produces candidate reachability and bounded protocol observations. Data-plane connection admission remains trust-gated, including behavior-originated Kademlia dials through the root dial admission policy.
- Noise secures each admitted libp2p connection. GossipSub validation distinguishes objective invalidity (`Reject`) from valid-but-locally-unauthorized publishers (`Ignore`). Group/application E2EE remains outside v1/v2 transport.
- Delivery remains realtime/best-effort, bounded, non-durable, with no exactly-once claim or offline mailbox. A human client may persist its own local history above the transport, but `TransportRuntime` never queues messages for an offline endpoint.
- IPC v2 retains the 128 KiB JSON-body ceiling so every legal 48 KiB payload fits with endpoint metadata after base64url/JSON expansion. Data-plane and administrative IPC use separate sockets; the data socket can never grant `admin.*` based on a claimed client kind.
- GossipSub mesh duplicate identity is frozen to a SHA-256 mapping over signed publisher PeerId + GossipSub wire sequence number, preventing cross-publisher suppression without coupling mesh identity to the application envelope ID.
- Internet listeners bound unauthenticated pre-Noise handshakes and trusted-peer direct ingress; dial failure/backoff is address-scoped where appropriate so a poisoned address cannot suppress a known-good trusted route.

## Human client Model B

The first-party human client uses a shared Rust core and Slint UI. Desktop is an IPC v2 consumer of the shared daemon and can share that PeerId with Claude via a separate EndpointId. Android embeds the same Rust runtime behind the neutral local-session contract rather than launching a standalone daemon. Concurrent physical devices use distinct PeerIds.

See:

- [Human client Model B](docs/architecture/human-client-model-b.md)
- [Cross-platform human client](docs/architecture/human-client-cross-platform.md)
- [Desktop human client](docs/architecture/human-client-desktop.md)
- [Android human client](docs/architecture/human-client-android.md)
- [Android key custody](docs/architecture/android-key-custody.md)
- [Human-client UI/interaction design](docs/architecture/human-client-ui.md)
- [Human-client platform packaging](docs/architecture/human-client-packaging.md)
- [Local client session contract](contracts/LOCAL-CLIENT.md)
- [Endpoint contract](contracts/ENDPOINTS.md)
- [Libp2p endpoint protocols](transport/libp2p/ENDPOINTS.md)
- [ADR-0030 local endpoint addressing](adr/0030-local-endpoint-addressing.md)
- [ADR-0031 endpoint directory](adr/0031-endpoint-directory.md)
- [ADR-0032 human client boundary](adr/0032-human-client-boundary.md)
- [Human + Claude profile example](config/examples/human-and-claude.yaml)

## Start here

- [Architecture overview](docs/architecture/overview.md)
- [Component boundaries](docs/architecture/components.md)
- [Data flows](docs/architecture/data-flows.md)
- [Transport contract](contracts/TRANSPORT.md)
- [Connectivity contract](contracts/CONNECTIVITY.md)
- [Endpoint contract](contracts/ENDPOINTS.md)
- [Local IPC contract](contracts/LOCAL-IPC.md)
- [Discovery contract](contracts/DISCOVERY.md)
- [ADR index](adr/README.md)
- [Threat model](docs/architecture/threat-model.md)
- [Rust blueprint](docs/architecture/rust-blueprint.md)
- [Mandatory Internet reachability design](transport/libp2p/CONNECTIVITY.md)
- [Implementation plan](roadmap/IMPLEMENTATION-PLAN.md)
- [Final architecture review](docs/architecture/FINAL-REVIEW.md)
- [Adversarial security review closure](docs/architecture/SECURITY-REVIEW-2026-08-12.md)

## Source snapshot

Claude/Telegram research was refreshed 2026-08-11; libp2p/Kademlia, endpoint-protocol, and mandatory NAT/relay/DCUtR research was extended 2026-08-12. See [research/SOURCES.md](research/SOURCES.md), [research/endpoint-addressing.md](research/endpoint-addressing.md), and [research/nat-traversal.md](research/nat-traversal.md).

## Repository name

The working name **claude-p2p-channel** is retained because it describes the Claude integration boundary and transport class without implying an application protocol, project, team, human identity system, or coordination model.


## Human-readable recovery and application guidance

- Identity recovery: [`contracts/IDENTITY-RECOVERY.md`](./contracts/IDENTITY-RECOVERY.md), [`docs/architecture/identity-recovery.md`](./docs/architecture/identity-recovery.md), and [`ADR-0033`](./adr/0033-identity-recovery-mnemonic.md).
- Current Kademlia default-on amendment: [`docs/architecture/KAD-DEFAULT-ON-REVIEW-2026-08-12.md`](./docs/architecture/KAD-DEFAULT-ON-REVIEW-2026-08-12.md) and [`ADR-0034`](./adr/0034-kademlia-default-enabled.md).
- Non-normative first-party broadcast author hint: [`docs/architecture/application-envelope-guidance.md`](./docs/architecture/application-envelope-guidance.md). Transport still treats broadcast authorship as PeerId-only.

## Mandatory Internet reachability

- [`ADR-0035`](./adr/0035-mandatory-internet-reachability.md) makes Phase 9 required for standard v1.
- [`ADR-0036`](./adr/0036-connectivity-infrastructure-peer-class.md) separates reachability infrastructure authorization from application trust.
- [`contracts/CONNECTIVITY.md`](./contracts/CONNECTIVITY.md) freezes the backend-neutral connectivity states/path semantics.
- [`transport/libp2p/CONNECTIVITY.md`](./transport/libp2p/CONNECTIVITY.md) defines the integrated state machine and ownership.
- [`transport/libp2p/AUTONAT.md`](./transport/libp2p/AUTONAT.md), [`RELAY.md`](./transport/libp2p/RELAY.md), and [`DCUTR.md`](./transport/libp2p/DCUTR.md) are the detailed backend blueprints.
- [`docs/architecture/connectivity-deployment.md`](./docs/architecture/connectivity-deployment.md) defines client/infrastructure deployment, redundancy, outage, and rollout topology.
- [`config/examples/internet-reachability.yaml`](./config/examples/internet-reachability.yaml) shows a two-relay/probe-server Internet profile.
- [`config/examples/connectivity-infrastructure.yaml`](./config/examples/connectivity-infrastructure.yaml) shows explicit AutoNAT/relay server roles with protocol-scoped authorization.

## Security freeze addendum

The 2026-08-12 adversarial security pass is recorded in [`docs/architecture/SECURITY-REVIEW-2026-08-12.md`](./docs/architecture/SECURITY-REVIEW-2026-08-12.md). It freezes source+wire-sequence GossipSub message identity, split data/admin IPC sockets, pre-Noise admission limits, address-scoped identity-mismatch quarantine, hostile remote endpoint-metadata validation, direct trusted-peer token buckets, and 128-bit IPC keepalive nonces. ADR-0038 also records an explicit optional v2.x encrypted software-key path gated by SPIKE-007; standard v1 remains filesystem-only at rest.

## Current human/mobile review

See [`docs/architecture/HUMAN-CLIENT-REVIEW-2026-08-12.md`](docs/architecture/HUMAN-CLIENT-REVIEW-2026-08-12.md) for closure of AutoNAT/relay/connectivity review V1-V6 and the desktop/Android deployment decisions.
