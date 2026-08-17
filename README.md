# InterWeave

**InterWeave** is a generic peer-to-peer transport architecture for Claude Code Channels and first-party human clients. It combines payload-agnostic transport contracts, one-PeerId/many-EndpointId local routing, signed GossipSub broadcast, dedicated directed messaging, replaceable discovery, Kademlia peer routing, and mandatory Internet reachability through AutoNAT v2, Circuit Relay v2, and DCUtR.

> **Repository status:** accepted architecture, plus **Stage 0** of the construction plan. The design is under [`architecture/`](./architecture/). There is no production Rust implementation yet: the landing zones under `apps/`, `crates/`, `spikes/`, and `packaging/` are empty of it, and the root Cargo workspace has exactly two members — `xtask` and `tests/support`, neither of them product code. The next package joins when the stage that needs it opens.

## What InterWeave is

InterWeave defines the transport and local-client boundary needed for multiple local applications to share one persistent peer identity without conflating routing with identity.

```text
Desktop/server

  human-desktop -- data IPC --\
  claude-channel -- data IPC ---+--> interweave-transportd --> TransportRuntime --> libp2p
                                |         one PeerId
  settings/admin -- admin IPC --/         many EndpointIds

Android

  Slint UI --> LocalDataSession --> foreground Service --> TransportRuntime --> libp2p
                                   (same contracts, embedded deployment)

Network

  broadcast      -> signed GossipSub
  directed       -> /interweave/direct/2.0.0
  endpoint query -> /interweave/endpoints/1.0.0
  peer routing   -> private InterWeave Kademlia namespace
  reachability   -> AutoNAT v2 + Circuit Relay v2 + DCUtR
```

InterWeave is deliberately **not** an agent coordination framework, task protocol, Git workflow, social graph, human-identity system, read-receipt service, or durable transport mailbox. Higher layers may define those concepts without pushing them into the transport.

## Core invariants

- One configured transport profile owns one persistent **PeerId**.
- Model B adds configured **EndpointIds** beneath that PeerId (`human`, `claude`, `automation.build`, ...). EndpointId is routing metadata, not an identity or authorization principal.
- Broadcast uses **GossipSub only**. Directed traffic uses the dedicated direct protocol and is never tunneled through GossipSub.
- Direct v2 routes to exactly one endpoint. Omitted destination resolves the receiver's configured default endpoint; it never means fan-out.
- `AcceptedV2` means the remote endpoint's bounded local queue admitted the message. It does not mean a human or Claude processed/read it.
- Discovery is advisory and replaceable. It never grants trust.
- Data-plane trust is deny-by-default and PeerId-scoped; endpoint policy may narrow trust but never widen it.
- Kademlia is peer-routing only: no endpoint, channel, trust, membership, or application records.
- Root connection/dial admission exists before autonomous libp2p behaviours are activated.
- Standard v1 includes AutoNAT v2 client, Circuit Relay v2 client/reservations, and DCUtR.
- `TransportRuntime` never provides a durable offline mailbox.
- First-party human-client persistence is intentionally narrow: pending outbound, unread inbound, and inbound messages explicitly kept by the receiver after reading.

The accepted details live in the contracts and ADRs; this README is an orientation document, not a substitute for them.

## Repository layout

| Path | Role |
|---|---|
| [`architecture/`](./architecture/README.md) | Normative ADRs, contracts, architecture, research, configuration schema/examples, and roadmap |
| [`apps/`](./apps/README.md) | Future thin executable/platform composition roots |
| [`crates/`](./crates/README.md) | Future reusable Rust crate boundaries |
| [`tests/`](./tests/README.md) | Cross-crate, conformance, real-network, security, desktop E2E, and Android E2E suites |
| [`fixtures/`](./fixtures/README.md) | Frozen normative protocol/crypto/config vectors |
| [`test-data/`](./test-data/README.md) | Mutable non-normative scenario data |
| [`spikes/`](./spikes/README.md) | Empirical implementation investigations; never production dependencies |
| [`packaging/`](./packaging/README.md) | Future Linux/macOS/Windows/Android packaging |
| [`xtask/`](./xtask/README.md) | Future repository/test orchestration |
| [`tools/`](./tools/) | Repository tooling — PR/review scripts and tree checks, each with a self-test beside it |
| `.claude/` | Committed agent configuration and task-scoped skills; per-developer overrides stay untracked |
| [`IMPLEMENTATION.md`](./IMPLEMENTATION.md) | Implementation landing-zone and activation rules |

The root [`Cargo.toml`](./Cargo.toml) is a zero-member virtual workspace. `workspace.metadata.interweave` records intended members without making them buildable. A crate/package is added to `[workspace].members` only when its canonical implementation stage begins.

## Canonical implementation order

The governing construction order is [`architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md`](./architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md), adopted by [ADR-0046](./architecture/adr/0046-bottom-up-implementation-order.md).

The historical numbered phases remain scope/release labels. They are **not** permission to violate dependency order.

```text
Stage 0   foundation + frozen fixtures
Stage 1   neutral contracts + config
Stage 2   pure policies/state machines
Stage 3   persistence
Stage 4   minimal authenticated libp2p substrate
Stage 5   root ConnectionManager/DialAdmissionGate + pre-auth limits
Stage 6   direct v2
Stage 7   GossipSub
Stage 8   endpoint directory
Stage 9   discovery framework
Stage 10  Kademlia
Stage 11  AutoNAT + Relay + DCUtR
Stage 12  TransportRuntime integration
Stage 13  daemon + IPC
Stage 14  human application core/UI
Stage 15  desktop human client
Stage 16  Claude Channel bridge
Stage 17  Android
Stage 18  adversarial/security gate
Stage 19  packaging/release
```

In particular, Kademlia, AutoNAT, Relay, and DCUtR may not be activated before Stage 5's root dial/security funnel is implemented and green.

## Human message retention

The first-party human client is ephemeral by default. The durable store is not a conventional permanent conversation-history database.

| Message state | Durable local state |
|---|---:|
| Outgoing, pending/undelivered | Yes |
| Outgoing, transport-terminal | No |
| Incoming, unread | Yes |
| Incoming, read and not kept | No |
| Incoming, read and explicitly kept by receiver | Yes |

The receiver-only **Keep** action is local application state; a remote sender cannot request or force persistence. See [`architecture/clients/human/RETENTION.md`](./architecture/clients/human/RETENTION.md) and [ADR-0044](./architecture/adr/0044-human-message-retention.md).

## Project and wire namespace

[ADR-0047](./architecture/adr/0047-interweave-project-and-wire-namespace.md) freezes:

```text
Display name:       InterWeave
Machine namespace:  interweave
Direct protocol:    /interweave/direct/2.0.0
Endpoint protocol:  /interweave/endpoints/1.0.0
Kademlia prefix:    /interweave/kad/1.0.0/<network-hash>
HumanChat media:    application/vnd.interweave-human-chat+json;v=2
```

Claude-specific names such as `claude-channel` remain integration names and are not project branding.

## Start here

For a first architecture pass, read in this order:

1. [`architecture/docs/architecture/overview.md`](./architecture/docs/architecture/overview.md)
2. [`architecture/docs/architecture/components.md`](./architecture/docs/architecture/components.md)
3. [`architecture/contracts/TRANSPORT.md`](./architecture/contracts/TRANSPORT.md)
4. [`architecture/contracts/ENDPOINTS.md`](./architecture/contracts/ENDPOINTS.md)
5. [`architecture/contracts/LOCAL-CLIENT.md`](./architecture/contracts/LOCAL-CLIENT.md)
6. [`architecture/contracts/CONNECTIVITY.md`](./architecture/contracts/CONNECTIVITY.md)
7. [`architecture/contracts/DISCOVERY.md`](./architecture/contracts/DISCOVERY.md)
8. [`architecture/docs/architecture/threat-model.md`](./architecture/docs/architecture/threat-model.md)
9. [`architecture/adr/README.md`](./architecture/adr/README.md)
10. [`architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md`](./architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md)

Useful focused documents:

- Human cross-platform design: [`architecture/docs/architecture/human-client-cross-platform.md`](./architecture/docs/architecture/human-client-cross-platform.md)
- Desktop human client: [`architecture/docs/architecture/human-client-desktop.md`](./architecture/docs/architecture/human-client-desktop.md)
- Android human client: [`architecture/docs/architecture/human-client-android.md`](./architecture/docs/architecture/human-client-android.md)
- Android key custody: [`architecture/docs/architecture/android-key-custody.md`](./architecture/docs/architecture/android-key-custody.md)
- Mandatory Internet reachability: [`architecture/transport/libp2p/CONNECTIVITY.md`](./architecture/transport/libp2p/CONNECTIVITY.md)
- Kademlia design: [`architecture/discovery/KademliaDiscovery.md`](./architecture/discovery/providers/kademlia.md)
- Security review: [`architecture/docs/architecture/SECURITY-REVIEW-2026-08-12.md`](./architecture/docs/architecture/SECURITY-REVIEW-2026-08-12.md)
- Human/mobile review: [`architecture/docs/architecture/HUMAN-CLIENT-REVIEW-2026-08-12.md`](./architecture/docs/architecture/HUMAN-CLIENT-REVIEW-2026-08-12.md)
- Retention amendment review: [`architecture/docs/architecture/MESSAGE-RETENTION-REVIEW-2026-08-12.md`](./architecture/docs/architecture/MESSAGE-RETENTION-REVIEW-2026-08-12.md)

## Development policy

Until a bottom-up stage is explicitly opened, this repository remains an architecture/skeleton repository. Contributors and coding agents should follow [`CLAUDE.md`](./CLAUDE.md) and [`IMPLEMENTATION.md`](./IMPLEMENTATION.md).

When implementation begins:

- activate only the package(s) required by the current canonical stage;
- keep application binaries as thin composition roots;
- keep neutral API crates free of libp2p, Slint, Android, SQLite, and Claude-specific dependencies;
- place tests at the lowest layer that completely proves the behavior;
- use real Swarms/processes/platform tests where the contract depends on real integration behavior rather than replacing them with mocks;
- keep frozen vectors in `fixtures/` and mutable scenarios in `test-data/`;
- preserve architecture decisions by amending the relevant ADR/contract before intentionally diverging in code.

`Cargo.lock` is intentionally tracked for this application/workspace repository.

## Security

Do not commit private transport identities, recovery phrases, Android signing material, Keystore exports, local profile state, or real credentials. The `.gitignore` is a guardrail, not a secret-management boundary.

Security-sensitive implementation changes should be checked against the threat model, resource limits, security review, and the permanent `tests/security/` landing zone. Discovery, trust, connection admission, endpoint routing, and connectivity-infrastructure authorization are intentionally separate boundaries.

## License

InterWeave first-party code and documentation are licensed under the **Apache License, Version 2.0** (`Apache-2.0`). See [`LICENSE`](./LICENSE).

Third-party dependencies, copied fixtures, generated artifacts, and externally sourced material retain their own applicable licenses and notices; adding them to this repository does not relicense them as InterWeave code.
