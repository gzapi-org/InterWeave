# claude-p2p-channel

Architecture and contracts for a generic peer-to-peer **Claude Code Channel transport**.

> Status: architecture only. This repository intentionally contains no production MCP server, libp2p networking, daemon, installer, system service, or Rust crate implementation.

## Purpose

`claude-p2p-channel` is a transport plugin, not an application coordination protocol. It gives Claude Code a message channel whose network backend is decentralized and payload-agnostic. Higher-level systems may carry text, JSON, or their own application protocols, but this project does not define agent roles, task state, repositories, Git semantics, issue tracking, branch ownership, or merge policy.

The primary boundaries are:

```text
+-----------------------------+
|         Claude Code         |
| Channel / MCP integration   |
+-------------+---------------+
              | stdio MCP
              v
+-----------------------------+
| P2P Channel MCP bridge      |
| events / tools / guidance   |
+-------------+---------------+
              | local IPC
              v
+-----------------------------+
| P2P transport daemon        |
| transport-neutral runtime   |
+-------+-----------+---------+
        |           |
        |           +---- direct request/response protocol (1:1)
        +---------------- GossipSub (1:many)
        |           +---- DiscoveryManager -> DiscoveryProvider(s)
        |           +---- PeerTrustPolicy
        |           +---- ConnectionManager
        v
+-----------------------------+
| rust-libp2p backend         |
| TCP + Noise + Yamux         |
+-----------------------------+
```

## Decision summary

- Claude integration follows the official Channel contract: stdio MCP, `claude/channel`, push notifications, explicit reply tools, and a pre-delivery admission gate.
- The network runtime is a **separate, profile-scoped daemon** so Claude session restarts do not redefine transport identity or tear down P2P connectivity.
- `Transport` and `DiscoveryProvider` are stable contracts. Claude-facing code does not depend on libp2p, GossipSub, mDNS, Kademlia, or multiaddresses.
- Broadcast is GossipSub. Directed messaging is a dedicated libp2p request-response protocol; directed messages are never emulated by broadcasting and discarding at unrelated peers.
- v1 discovery is composable: peer cache + optional mDNS + static bootstrap. Kademlia has a complete integration blueprint but remains optional and `enabled: false` by default; it is peer-routing-only, uses capability-aware targeting/effective-target saturation, and never grants trust or stores channel/application records.
- Discovery only produces **candidate reachability and bounded transport protocol observations**. It never grants trust. v1 uses a deny-by-default static PeerId allowlist for connection admission, inbound source admission, and outbound direct sends; all Swarm dials, including Kademlia behaviour-originated requests, pass the same ConnectionManager policy gate.
- Noise secures each admitted libp2p connection. GossipSub validation distinguishes objective invalidity (`Reject`) from valid-but-locally-unauthorized publishers (`Ignore`); trusted forwarding peers can still read plaintext, so group/application encryption remains deferred.
- Delivery is realtime/best-effort, no global ordering, no durable mailbox, and no exactly-once claim. Broadcast requires the calling local client to be joined; direct send requires the destination to be trusted.
- Multiple local Claude sessions share a daemon only when explicitly configured to use the same profile/socket; independent profiles never share keys accidentally. Same-profile direct inbound messages fan out to every connected event-capable local client, while broadcast delivery is filtered by per-client join references. IPC v1 uses a 128 KiB JSON-body ceiling so every legal 48 KiB transport payload fits after base64url/JSON expansion; Claude Channel clients cannot invoke administrative daemon shutdown.

## Start here

- [Architecture overview](docs/architecture/overview.md)
- [Component boundaries](docs/architecture/components.md)
- [Data flows](docs/architecture/data-flows.md)
- [Transport contract](contracts/TRANSPORT.md)
- [Discovery contract](contracts/DISCOVERY.md)
- [ADR index](adr/README.md)
- [Telegram implementation research](research/telegram-plugin-implementation.md)
- [Threat model](docs/architecture/threat-model.md)
- [Rust blueprint](docs/architecture/rust-blueprint.md)
- [Implementation plan](roadmap/IMPLEMENTATION-PLAN.md)
- [Amendment review memo](docs/architecture/AMENDMENT-REVIEW-2026-08-11.md)
- [Kademlia design review memo](docs/architecture/KAD-REVIEW-2026-08-11.md)
- [Kademlia/shared-profile second review closure](docs/architecture/KAD-REVIEW-2026-08-12.md)
- [Final architecture review](docs/architecture/FINAL-REVIEW.md)

## Source snapshot

Claude/Telegram research was refreshed 2026-08-11; the Kademlia/Swarm/Identify source pass was extended 2026-08-12. The inspected `anthropics/claude-plugins-official` `main` commit was `920824c3e9509890fbec03ba6097014222393022` (2026-08-10). See [research/SOURCES.md](research/SOURCES.md).

## Repository name

The working name **claude-p2p-channel** is retained because it describes the Claude integration boundary and transport class without implying an application protocol, project, team, or coordination model.


## Optional Kademlia blueprint

The repository includes a complete, implementation-ready Kademlia integration design while deliberately leaving it disabled. See [docs/architecture/kademlia-integration.md](docs/architecture/kademlia-integration.md), [discovery/providers/kademlia.md](discovery/providers/kademlia.md), and [ADR-0009](adr/0009-kademlia-role.md). The first integration is a private/trust-bounded peer-routing overlay, not the public IPFS DHT.
