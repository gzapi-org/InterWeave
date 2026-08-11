# Rust workspace blueprint

This is a package/dependency plan only; no crates are implemented in this repository.

## Proposed workspace

```text
crates/
  transport-api/          # NO libp2p, NO Claude
  discovery-api/          # NO libp2p, NO Claude
  trust-api/              # NO libp2p, NO Claude
  config/                 # typed profile/config model, NO libp2p where possible
  discovery-cache/        # discovery-api
  discovery-static/       # discovery-api
  discovery-mdns/         # discovery-api + libp2p mdns adapter
  discovery-kademlia/     # deferred; discovery-api + libp2p kad
  transport-libp2p/       # libp2p backend, connection/pubsub/direct/identity
  transport-runtime/      # transport-api + discovery-api + trust-api
  ipc-protocol/           # NO libp2p, NO Claude
  ipc-server/             # runtime adapter
  transport-daemon/       # composition root / CLI entry
  transportctl/           # local diagnostics/admin CLI
  claude-channel-core/    # Channel-event/tool mapping, NO libp2p
  claude-channel-bridge/  # MCP SDK + IPC client
```

A TypeScript bridge remains a viable implementation choice; if chosen, the `claude-channel-*` conceptual layers live outside Cargo while preserving the same dependency boundary.

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
        discovery-api trust-api transport-libp2p
          ^   ^   ^                 |
          |   |   |                 v
       cache static mdns          rust-libp2p
                 \
                  kademlia (deferred)
```

No arrows point from `transport-api` to provider/backend crates. No neutral contract crate imports `libp2p`.

## Major internal modules, not public traits by default

Within `transport-libp2p`:

- `swarm_task` — exclusive Swarm owner;
- `connection_manager` — trust-gated dial/inbound-retain decisions, address/backoff/limits;
- `pubsub_manager` — GossipSub topic/subscription state plus ADR-0029 validation-result reporting;
- `direct_manager` — request-response lifecycle;
- `identity_manager` — key loading/PeerId;
- `address_book` — normalized discovery observations -> libp2p addresses.

These remain concrete modules until a second implementation requires independent substitution.

## Testing packages

- `discovery-conformance-tests`: reusable behavioral harness;
- runtime integration tests with in-memory/temp profiles;
- libp2p multi-peer integration tests;
- IPC compatibility fixtures shared with bridge implementation, including exact 49,152-byte payload request/event fixtures under the 131,072-byte JSON-body ceiling and client-capability authorization cases.
