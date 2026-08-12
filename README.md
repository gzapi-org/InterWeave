# claude-p2p-channel

Architecture and contracts for a generic peer-to-peer **Claude Code Channel transport**, including Model B local endpoint multiplexing and a human-client architecture.

> Status: architecture only. This repository intentionally contains no production MCP server, libp2p networking, daemon, human client, installer, system service, or Rust crate implementation.

## Purpose

`claude-p2p-channel` is a transport plugin, not an application coordination or chat protocol. It gives Claude Code and other local clients a decentralized, payload-agnostic transport. Higher-level systems may carry text, JSON, chat envelopes, or their own protocols, but this project does not define agent roles, task state, repositories, Git semantics, human identity, social graphs, read receipts, or application workflows.

## Architecture

```text
                         one transport profile / one PeerId
                                      |
                        +-------------+-------------+
                        |  P2P transport daemon     |
                        |  EndpointRegistry          |
                        +------+--------------+------+
                               |              |
                      IPC v2   |              | IPC v2
                    endpoint   |              | endpoint
                    = human    |              | = claude
                               |              |
                               v              v
                         Human client   Claude Channel bridge
                                             |
                                             v
                                         Claude Code

Network side:
  GossipSub = broadcast by ChannelId
  request-response direct v2 = PeerId + EndpointId routing
  endpoint-directory = optional trusted route discovery
  DiscoveryManager / Kademlia(default-enabled when configured; explicit opt-out)
  PeerTrustPolicy / ConnectionManager / Noise
```

The daemon owns the private key and all libp2p state. Local applications never become independent network identities unless they use separate profiles.

## Decision summary

- Claude integration follows the official Channel pattern: stdio MCP, `claude/channel`, push notifications, explicit outbound tools, and pre-delivery admission.
- The network runtime is a **separate, profile-scoped daemon** so Claude/human-client restarts do not redefine transport identity or tear down P2P connectivity.
- One profile owns one persistent PeerId. Model B adds configured **EndpointIds** underneath that PeerId for deterministic local direct routing (`human`, `claude`, `automation.build`, etc.). EndpointId is a routing selector, not cryptographic/human/application identity.
- Initial software identities are **Ed25519**. An optional offline 24-word recovery record can reproduce the exact same PeerId by encoding the raw 32-byte Ed25519 secret with BIP-39 entropy/checksum/English-word mapping only; wallet PBKDF2/passphrase semantics are not used and recovery material never crosses IPC.
- Every direct-capable IPC v2 client owns one exclusive configured endpoint lease. Direct messages route to exactly one endpoint; the previous architecture-only all-client fan-out is superseded by ADR-0030.
- Direct protocol target is `/claude-p2p-channel/direct/2.0.0`, carrying required source endpoint and optional destination endpoint. Omitted destination resolves the receiver's explicit `default_direct_endpoint`; it never means fan-out.
- Direct `Accepted` means the resolved endpoint's bounded local event queue accepted the message, not that Claude or a human processed it.
- An optional trust-gated endpoint-directory protocol exposes only active routes explicitly marked `advertise: true`; it returns route names only, never human names/roles/trust claims.
- Endpoint-specific ACLs may narrow profile trust but can never widen it. Remote endpoint denial is exposed as coarse `no_route` / local `RemoteEndpointUnavailable` to avoid an authorization oracle.
- Broadcast remains GossipSub and ChannelId-scoped; endpoint addressing does not alter broadcast envelopes or subscription semantics.
- `Transport`, `DiscoveryProvider`, and trust boundaries remain independent of Claude/libp2p details.
- Kademlia has a complete peer-routing integration blueprint and, per ADR-0034, configured entries are **`enabled: true` by default** in the standard v1 build. Operators may explicitly opt out. It never grants trust or stores application/channel/endpoint records.
- Discovery only produces candidate reachability and bounded protocol observations. Data-plane connection admission remains trust-gated, including behavior-originated Kademlia dials through the root dial admission policy.
- Noise secures each admitted libp2p connection. GossipSub validation distinguishes objective invalidity (`Reject`) from valid-but-locally-unauthorized publishers (`Ignore`). Group/application E2EE remains outside v1/v2 transport.
- Delivery remains realtime/best-effort, bounded, non-durable, with no exactly-once claim or offline mailbox. A human client may persist its own local history above the transport, but the daemon never queues messages for an offline endpoint.
- IPC v2 retains the 128 KiB JSON-body ceiling so every legal 48 KiB payload fits with endpoint metadata after base64url/JSON expansion. Claude Channel clients cannot invoke endpoint administration or daemon shutdown.

## Human client Model B

The human client is another IPC v2 consumer, not a second libp2p implementation. It can share the same PeerId as Claude while owning a separate EndpointId.

See:

- [Human client Model B](docs/architecture/human-client-model-b.md)
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
- [Endpoint contract](contracts/ENDPOINTS.md)
- [Local IPC contract](contracts/LOCAL-IPC.md)
- [Discovery contract](contracts/DISCOVERY.md)
- [ADR index](adr/README.md)
- [Threat model](docs/architecture/threat-model.md)
- [Rust blueprint](docs/architecture/rust-blueprint.md)
- [Implementation plan](roadmap/IMPLEMENTATION-PLAN.md)
- [Final architecture review](docs/architecture/FINAL-REVIEW.md)

## Source snapshot

Claude/Telegram research was refreshed 2026-08-11; libp2p/Kademlia and endpoint-protocol research was extended 2026-08-12. See [research/SOURCES.md](research/SOURCES.md) and [research/endpoint-addressing.md](research/endpoint-addressing.md).

## Repository name

The working name **claude-p2p-channel** is retained because it describes the Claude integration boundary and transport class without implying an application protocol, project, team, human identity system, or coordination model.


## Human-readable recovery and application guidance

- Identity recovery: [`contracts/IDENTITY-RECOVERY.md`](./contracts/IDENTITY-RECOVERY.md), [`docs/architecture/identity-recovery.md`](./docs/architecture/identity-recovery.md), and [`ADR-0033`](./adr/0033-identity-recovery-mnemonic.md).
- Current Kademlia default-on amendment: [`docs/architecture/KAD-DEFAULT-ON-REVIEW-2026-08-12.md`](./docs/architecture/KAD-DEFAULT-ON-REVIEW-2026-08-12.md) and [`ADR-0034`](./adr/0034-kademlia-default-enabled.md).
- Non-normative first-party broadcast author hint: [`docs/architecture/application-envelope-guidance.md`](./docs/architecture/application-envelope-guidance.md). Transport still treats broadcast authorship as PeerId-only.
