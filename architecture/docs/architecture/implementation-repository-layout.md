# Implementation repository layout

This document maps the frozen architecture onto physical implementation/test folders. It is a placement/ownership contract, not production code.

## Top-level rule

```text
claude-p2p-channel/
├── architecture/    # specifications, ADRs, contracts, research, roadmap
├── apps/            # executable/platform composition roots
├── crates/          # reusable Rust package landing zones
├── tests/           # cross-crate/network/conformance/E2E suites
├── fixtures/        # frozen normative vectors
├── test-data/       # mutable/non-normative scenario data
├── spikes/          # empirical non-production experiments
├── packaging/       # platform release/service/package material
├── xtask/           # future developer/test orchestration
├── Cargo.toml       # zero-member virtual workspace skeleton
└── IMPLEMENTATION.md
```

The root virtual Cargo workspace remains empty until implementation starts. Directories with only README files are planned package boundaries, not buildable crates.

## Applications

`apps/` contains only composition roots:

```text
apps/
├── transport-daemon/
├── transportctl/
├── claude-channel/
├── human-desktop/
└── human-android/
    └── android/
```

Application code may parse CLI/platform inputs, construct dependencies, bind lifecycle and render results. It must not own wire codecs, trust decisions, discovery algorithms, endpoint policy, dedup, retention state machines, or reusable persistence semantics.

Desktop human/Claude applications consume local IPC bindings. Android hosts the same TransportRuntime inside its foreground-service process and consumes the embedded `LocalDataSession` adapter.

## Crate grouping

```text
crates/
├── api/
│   ├── transport-api/
│   ├── discovery-api/
│   ├── trust-api/
│   ├── local-client-api/
│   ├── ipc-protocol/
│   └── kademlia-control-api/
├── config/
│   └── profile-config/
├── transport/
│   ├── runtime/
│   └── libp2p/
├── discovery/
│   ├── cache/
│   ├── static/
│   ├── mdns/
│   └── kademlia/
├── local/
│   ├── ipc-client/
│   └── ipc-server/
├── claude/
│   └── channel-core/
└── human/
    ├── core/
    ├── chat-protocol/
    ├── store/
    ├── ui-model/
    ├── ui-slint/
    └── android-platform/
```

### Dependency rules

- `crates/api/*` do not depend on libp2p, Slint, Android, SQLite, Claude SDK, or application-specific state.
- `discovery/kademlia` uses `discovery-api` + `kademlia-control-api`; the Swarm-owned driver stays in `transport/libp2p`.
- `human/core` owns application workflows/retention transitions but no SQLite/UI/network implementation.
- `human/store` implements exactly ADR-0044 durable classes (`pending_outbound`, `unread_inbound`, `kept_inbound`) and must not expose a generic permanent-history API.
- `human/ui-slint` and `human/android-platform` depend inward on human/domain contracts; transport/domain code never depends outward on UI/platform crates.
- `local/ipc-client` and `local/ipc-server` are desktop bindings. Android implements the same local-client API in-process without pretending to be IPC.

Do not create one crate for every internal module. Concrete modules such as connection manager, dial admission, direct manager, endpoint registry, relay manager, and dedup stay inside the owning runtime/backend crate until a real independent substitution/build boundary appears.

## Test placement

Put a test at the lowest layer that completely proves the behavior.

| Behavior | Placement |
|---|---|
| pure parser/state/policy function | future `#[cfg(test)]` beside crate source |
| public crate API as external consumer | future `<crate>/tests/` |
| shared Transport/Discovery/LocalClient conformance | root `tests/*-conformance` / `transport-contract` |
| real direct/GossipSub/Kademlia/AutoNAT/Relay/DCUtR interaction | root network suite |
| human retention with real SQLite/restart | `tests/human-retention/` |
| split desktop data/admin authority/process restart | `tests/desktop-e2e/` |
| Android FGS/Keystore/FLAG_SECURE/backup/lifecycle | Android instrumented tests plus `tests/android-e2e/` host orchestration |

Root suites are pre-created as landing zones:

```text
tests/
├── support/
├── transport-contract/
├── discovery-conformance/
├── local-client-conformance/
├── ipc-v2/
├── direct-v2/
├── pubsub/
├── kademlia/
├── connectivity/
├── endpoint-routing/
├── human-chat/
├── human-retention/
├── security/
├── desktop-e2e/
├── android-e2e/
└── interoperability/
```

`tests/support` is test-only. Production crates/applications must never depend on it.

## Fixtures versus test data

`fixtures/` contains normative/frozen vectors whose change is a compatibility/contract event. Initial categories are identity, direct-v2, GossipSub, Kademlia, IPC v2, endpoints, HumanChatV1, and configuration.

`test-data/` contains mutable scenario inputs such as network topologies, malformed generated cases and non-normative sample configs. Changing this data does not change a protocol.

Do not bury golden protocol vectors only inside Rust source: Android/future third-party implementations must be able to consume the same fixture corpus.

## Platform tests

Desktop E2E eventually starts real `transport-daemon`, human/Claude harnesses and `transportctl` and exercises lifecycle, admin/data separation, profile recovery and packaging.

Android domain/core tests run as ordinary host Rust tests where possible. Real Android platform guarantees (foreground service, process death, notification lifecycle, Keystore, secure recovery Activity, backup exclusion, network callbacks) belong in instrumented Android tests. `tests/android-e2e` is the host controller for cross-device/network scenarios with real desktop/relay/probe peers.

## Spikes

`spikes/spike-001` through `spike-009` correspond directly to `roadmap/SPIKES.md`. Spike code may deliberately be rough and version-specific. It cannot become a production dependency. Once a spike settles a behavior, the durable outcome is an ADR/contract decision and a permanent test/fixture in the appropriate root suite.

## CI layers

Planned ordering:

1. repository/architecture link/schema/fixture audit;
2. formatting/lints once Rust code exists;
3. unit + crate API tests;
4. contract/conformance + frozen fixtures;
5. network integration suites;
6. adversarial/security suites;
7. desktop E2E;
8. Android host + emulator/instrumented suites;
9. packaging/release checks.

The folder layout is not permission to weaken release gates. Mandatory Kademlia and Phase-9 connectivity remain standard-v1 requirements.
