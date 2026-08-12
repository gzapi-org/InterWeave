# Rust workspace blueprint

This is a package/dependency plan only; no crates are implemented in this repository.

## Proposed workspace

```text
crates/
  transport-api/          # NO libp2p, NO Claude
  discovery-api/          # NO libp2p, NO Claude
  trust-api/              # NO libp2p, NO Claude
  config/                 # typed profile/config model, NO libp2p where possible
  kademlia-control-api/   # tiny INTERNAL neutral driver port; NO libp2p, NO Claude
  discovery-cache/        # discovery-api
  discovery-static/       # discovery-api
  discovery-mdns/         # discovery-api + libp2p mdns adapter
  discovery-kademlia/     # optional/default-off provider scheduler; discovery-api + kademlia-control-api
  transport-libp2p/       # libp2p backend, connection/pubsub/direct/identity + kademlia-control-api impl
  transport-runtime/      # transport-api + discovery-api + trust-api
  ipc-protocol/           # NO libp2p, NO Claude
  ipc-server/             # runtime adapter
  transport-daemon/       # composition root / CLI entry
  transportctl/           # local diagnostics/admin CLI
  claude-channel-core/    # Channel-event/tool mapping, NO libp2p
  claude-channel-bridge/  # MCP SDK + IPC client
```

A TypeScript bridge remains a viable implementation choice; if chosen, the `claude-channel-*` conceptual layers live outside Cargo while preserving the same dependency boundary.

`kademlia-control-api` is deliberately **not** a new public transport/discovery abstraction. It is an internal workspace seam needed because the optional provider scheduler and the Swarm-owned driver live in different crates. Its types are backend-neutral opaque transport identifiers/addresses plus Kademlia-specific commands/events; it must not expose `libp2p::PeerId`, `Multiaddr`, `QueryId`, or `kad::*` types.

## Dependency direction

```text
Claude bridge
    |
    v
ipc-protocol ----> transport-api
                       ^
                       |
                transport-runtime
                 /     |      \
                v      v       v
        discovery-api trust-api transport-libp2p ------> rust-libp2p
          ^   ^   ^       ^           ^
          |   |   |       |           |
       cache static mdns  |     kademlia-control-api
                          |           ^
                          |           |
                    discovery-kademlia
```

More explicitly:

```text
discovery-kademlia ---> discovery-api
         |
         +-----------> kademlia-control-api <----------- transport-libp2p
                                                        |
                                                        +--> rust-libp2p
```

No arrows point from `transport-api` or `discovery-api` to provider/backend crates. No neutral/public contract crate imports `libp2p`. `transport-libp2p` does **not** depend on `discovery-kademlia`, and `discovery-kademlia` does **not** depend on `transport-libp2p`.

## Major internal modules, not public traits by default

Within `transport-libp2p`:

- `swarm_task` — exclusive Swarm owner;
- `connection_manager` — trust-gated connection policy, address/backoff/limits;
- `dial_admission_gate` — synchronous root-level enforcement for all Swarm dials, including behaviour-originated Kademlia dials;
- `pubsub_manager` — GossipSub topic/subscription state plus ADR-0029 validation-result reporting;
- `direct_manager` — request-response lifecycle;
- `identity_manager` — key loading/PeerId;
- `address_book` — normalized discovery observations -> libp2p addresses;
- `kademlia_driver` — optional `libp2p::kad::Behaviour` owner/adapter inside the Swarm task, implementing `kademlia-control-api`.

Within `discovery-kademlia`:

- provider lifecycle / `DiscoveryProvider` implementation;
- query scheduler/budgets/cooldowns;
- capability-aware targeted-lookup eligibility;
- result normalization/TTL;
- Kademlia provider health and saturation state.

These remain concrete modules until a second implementation requires independent substitution.

## Testing packages

- `discovery-conformance-tests`: reusable behavioral harness;
- runtime integration tests with in-memory/temp profiles;
- libp2p multi-peer integration tests;
- IPC compatibility fixtures shared with bridge implementation, including exact 49,152-byte payload request/event fixtures under the 131,072-byte JSON-body ceiling, client-capability authorization cases, and shared-profile direct-message fan-out cases.

## Kademlia construction order

When a build includes Kademlia support, the composition root constructs `transport-libp2p` with the optional driver slot and obtains an implementation of the neutral `kademlia-control-api` port. It injects that port into `discovery-kademlia`. The shared tiny API crate prevents either concrete crate from depending on the other.

If configuration says `enabled: false`, the behavior slot is inactive and no Kademlia protocol is advertised or queried.
