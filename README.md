# InterWeave

**InterWeave** is a peer-to-peer transport for agent harnesses that speak MCP and for first-party human clients; today the agent bridge receives through Claude Code's channel extension. One configured profile owns one persistent peer identity; many local applications share it through configured endpoints. On the wire it combines signed GossipSub broadcast, a dedicated directed-message protocol, an endpoint directory, replaceable discovery, Kademlia peer routing, and mandatory Internet reachability through AutoNAT v2, Circuit Relay v2 and DCUtR. The design is under [`architecture/`](./architecture/); the code is built one stage at a time in the order that design fixes.

> **Repository status: Stage 17 open** (`stage-17-android-human-client`). Stages 0–16 are complete. Stage 17's prerequisites, SPIKE-008 and SPIKE-009, are its first work; the Android human client follows them.

## Where the work stands

Every closed stage has a closing record in the [implementation plan](./architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md) that says what it proved and, by name, what it did not. The table is the orientation; the record is the truth.

| Stage | Closed | What it delivered | Record |
|---|---|---|---|
| 0–3 | — | the workspace and frozen fixtures; the neutral contract crates (types and validation, no I/O); the pure policies and state machines; the first file I/O — human store, peer cache, profile paths, Ed25519 identity | plan §2–§6 |
| 4–5 | — | the authenticated libp2p substrate (TCP, Noise, Yamux, Identify); the root connection and dial-admission funnel every outbound dial passes through | plan §7–§8 |
| 6–8 | — | directed messaging at `/interweave/direct/2.0.0` between real peers; signed GossipSub broadcast; the endpoint directory at `/interweave/endpoints/1.0.0`, with a send's source endpoint bound to the caller's lease | plan §9–§11 |
| 9–10 | — | the discovery framework (static, cache and mDNS providers under one conformance suite; the mDNS crate shipped its normalization half, the multicast mechanism being Stage 11's) and Kademlia peer routing behind the root gate | plan §12–§13 |
| 11 | 2026-09-27 | AutoNAT v2 client and server, Circuit Relay v2 client and server, DCUtR, path preference and network-change handling, each `None` by default until a profile enables it; the mDNS multicast mechanism on a vendored, bounded `libp2p-mdns`; the real-NAT matrix ran as containerised rows with four owner-deferred limits named | plan §14 |
| 12 | 2026-09-28 | the composition root: a validated profile becomes a running `TransportRuntime`; two composed nodes connect through static discovery; the in-process local-session binding passes its conformance suite on real sockets | plan §15 |
| 13 | 2026-10-01 | the profile-scoped daemon with two owner-protected sockets, the IPC client library, and `transportctl` for live administration and offline identity backup and restore; every IPC contract `active` | plan §16 |
| 14 | 2026-10-03 | the human application core: the facade that owns the client's half of retention, the store's application tables, a toolkit-free render and presentation model, the reference Slint views, and HumanChatV2 across two daemons | plan §17 |
| 15 | 2026-10-06 | the desktop human client as one binary, proved without a person against two real daemons: process kill, restart, storage failure, trust mutation over the admin socket, and the accessibility tree read over AT-SPI | plan §18 |
| 16 | 2026-10-06 | the Claude Code Channel bridge: a stdio MCP server holding one endpoint lease, turning direct and broadcast messages into channel notifications with provenance-only metadata, exchanging messages with a far peer from inside an installed Claude Code | plan §19 |
| 17 | open | the Android human client, after SPIKE-008 and SPIKE-009 | plan §20 |

Stages 18 (the adversarial and security gate) and 19 (packaging and release) follow.

## What runs today

- **`transport-daemon`** (`apps/transport-daemon`; the diagram's `interweave-transportd`): the profile-scoped daemon. One process per profile, two owner-protected Unix sockets, a data plane for applications and an admin plane for settings.
- **`transportctl`** (`apps/transportctl`): administers a live daemon and backs up, verifies and restores an identity offline.
- **`human-desktop`** (`apps/human-desktop`): the first-party desktop client, a Slint window over a headless application core.
- **`claude-channel`** (`apps/claude-channel`): the Claude Code Channel bridge, a stdio MCP server over the daemon's data socket; the plugin that starts it is `packaging/claude-plugin/interweave`.

`apps/human-android` is a landing zone until Stage 17 lands the Android client.

Trust changes made through the admin socket persist in the profile's state directory, never in its configuration file, and survive a daemon restart.

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
- Data-plane trust is deny-by-default and PeerId-scoped; endpoint policy may narrow trust but never widen it. An operator's trust change over the admin socket is persisted beside the profile, and the configuration file is never written by the daemon.
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
| [`apps/`](./apps/README.md) | The thin executables: the daemon, `transportctl`, the desktop client, the Claude Channel bridge; the Android landing zone |
| [`crates/`](./crates/README.md) | The Rust crates, one directory per compile-time boundary: neutral `api`, `config`, `identity`, `discovery`, `transport`, `local` (IPC), `human`, `claude` |
| [`tests/`](./tests/README.md) | Cross-crate, conformance, real-network, security, desktop E2E, and Android E2E suites |
| [`fixtures/`](./fixtures/README.md) | Frozen normative protocol/crypto/config vectors |
| [`test-data/`](./test-data/README.md) | Mutable non-normative scenario data |
| [`spikes/`](./spikes/README.md) | Empirical investigations and their evidence harnesses; never production dependencies |
| [`third_party/`](./third_party/README.md) | Vendored dependency sources, each under its own licence and each with an ADR; a patched one also records its diff |
| [`packaging/`](./packaging/README.md) | The Claude Code plugin that starts the Channel bridge (`claude-plugin/`); Linux/macOS/Windows/Android landing zones, implemented with Stage 19 |
| [`xtask/`](./xtask/README.md) | Repository/test orchestration — `cargo xtask checks` / `cargo xtask ci` |
| [`tools/`](./tools/) | Repository tooling — PR/review scripts and tree checks, each with a self-test beside it |
| `.claude/` | Committed agent configuration and task-scoped skills; per-developer overrides stay untracked |
| [`IMPLEMENTATION.md`](./IMPLEMENTATION.md) | Implementation landing-zone and activation rules |

The root [`Cargo.toml`](./Cargo.toml) is a virtual workspace, and `[workspace].members` is the authoritative list of what builds. It is deliberately not repeated here: a list written in prose went stale across two stages while the manifest stayed correct. `workspace.metadata.interweave` records the remaining planned members without making them buildable, and its `.status` records the open stage. A crate or package is added only when its canonical implementation stage begins.

## Building and testing

The workspace builds with the Rust toolchain pinned in [`rust-toolchain.toml`](./rust-toolchain.toml). The tree checks and the suites run through `xtask`:

```text
cargo xtask checks       # the tree checks under tools/checks/
cargo xtask selftests    # every check's own self-test
cargo xtask fmt --check
cargo xtask clippy
cargo xtask test
cargo xtask ci           # all of the above
```

Nothing short-circuits: one run reports everything that is wrong. Suites that need real sockets or two daemons say so in their READMEs under [`tests/`](./tests/README.md); the desktop client's end-to-end suites under `tests/desktop-e2e/tests/human_app/` need a display and run under `tools/ci/with_display.sh`.

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

The receiver-only **Keep** action is local application state; a remote sender cannot request or force persistence. Released content leaves the store's own files, not only its rows. See [`architecture/clients/human/RETENTION.md`](./architecture/clients/human/RETENTION.md) and [ADR-0044](./architecture/adr/0044-human-message-retention.md).

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
- Kademlia design: [`architecture/discovery/providers/kademlia.md`](./architecture/discovery/providers/kademlia.md)
- Security review: [`architecture/docs/architecture/SECURITY-REVIEW-2026-08-12.md`](./architecture/docs/architecture/SECURITY-REVIEW-2026-08-12.md)
- Human/mobile review: [`architecture/docs/architecture/HUMAN-CLIENT-REVIEW-2026-08-12.md`](./architecture/docs/architecture/HUMAN-CLIENT-REVIEW-2026-08-12.md)
- Retention amendment review: [`architecture/docs/architecture/MESSAGE-RETENTION-REVIEW-2026-08-12.md`](./architecture/docs/architecture/MESSAGE-RETENTION-REVIEW-2026-08-12.md)

## Development policy

Implementation proceeds one canonical bottom-up stage at a time; `[workspace].members` in [`Cargo.toml`](./Cargo.toml) is the authoritative roster and `workspace.metadata.interweave.status` records the open stage. Contributors and coding agents follow [`CLAUDE.md`](./CLAUDE.md) and [`IMPLEMENTATION.md`](./IMPLEMENTATION.md).

In every stage:

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
