# Rust workspace blueprint

This is a package/dependency plan only; none of the product crates below are implemented in this repository.

## Physical implementation workspace

ADR-0045 materializes the planned implementation landing zones at repository root. The root `Cargo.toml` is a virtual workspace, and Stage 0 opened it with two non-product members — `xtask` and `tests/support`. Every directory below is still a landing zone, not a crate, until its stage adds a manifest/source and adds the path to workspace members in the same change.

```text
apps/
  transport-daemon/
  transportctl/
  claude-channel/
  human-desktop/
  human-android/

crates/
  api/
    transport-api/
    discovery-api/
    trust-api/
    local-client-api/
    ipc-protocol/
    kademlia-control-api/
  config/
    profile-config/
  transport/
    runtime/
    libp2p/
  discovery/
    cache/
    static/
    mdns/
    kademlia/
  local/
    ipc-client/
    ipc-server/
  claude/
    channel-core/
  human/
    core/
    chat-protocol/
    store/
    ui-model/
    ui-slint/
    android-platform/
```

`apps/*` are thin composition roots. Do not create a crate for every internal manager/module; connection/dial/direct/pubsub/endpoint/Kademlia/connectivity managers remain internal modules of the owning runtime/libp2p package unless a real independent boundary emerges.

See `docs/architecture/implementation-repository-layout.md` for test/fixture/platform placement.

## Dependency direction

```text
Claude bridge --------------------> ipc-protocol
Desktop human -> ipc-client --------> local-client-api
Android human -> embedded adapter --> local-client-api
Admin UI/transportctl --------------> platform admin binding
                                          |
                                          v
                                    transport-api
                                ^
                                |
                         transport-runtime
                         /      |       \
                        v       v        v
                discovery-api trust-api transport-libp2p ---> rust-libp2p
                  ^   ^   ^       ^           ^
                  |   |   |       |           |
               cache static mdns  |     kademlia-control-api
                                  |           ^
                                  |           |
                            discovery-kademlia
```

No neutral/public contract imports libp2p. `transport-libp2p` and `discovery-kademlia` share only the narrow internal Kademlia control crate.

## transport-api types

Model B adds neutral types:

```text
EndpointId
DirectDestination { peer, endpoint? }
EndpointLeaseInfo { endpoint, lease_epoch }
RemoteEndpointDirectory { peer, endpoints, expires_at }
```

EndpointId must remain free of libp2p/Application/Claude-specific meaning.

## transport-runtime internal modules

- `endpoint_registry`
  - configured endpoint definitions;
  - exclusive live leases keyed by EndpointId;
  - default endpoint resolution;
  - endpoint inbound/outbound policy intersection;
  - active advertised endpoint snapshot;
  - endpoint lease epochs/events.
- `direct_admission`
  - profile trust + endpoint route + queue admission;
  - endpoint-aware dedup key;
  - bounded response to backend direct manager.
- `subscription_registry`
  - per-IPC-client ChannelId join references;
- `transport_coordinator`
  - command/event orchestration.

Do not turn EndpointRegistry into a public trait in v2 unless a second implementation actually needs substitution.

## IPC server ownership

`crates/local/ipc-server` owns both OS socket acceptors and tags each accepted connection with immutable `DataPlane` or `Admin` authority before hello parsing. Only data-plane IPC v2 hello may ask `endpoint_registry` for an exclusive EndpointId lease; only admin-socket sessions may receive `admin.*`. Connection drop releases any data-plane lease.

It does not decide network endpoint admission policy itself.

## transport-libp2p modules (`crates/transport/libp2p`)

- `swarm_task` — exclusive Swarm owner;
- `connection_manager` — trust-gated connection policy, address/backoff/limits;
- `dial_admission_gate` — applies to explicit and behavior-originated dials;
- `pubsub_manager` — GossipSub;
- `direct_manager` — `/direct/2.0.0`, request/response lifecycle and bounded runtime admission round trip;
- `endpoint_directory_manager` — `/endpoints/1.0.0`, trust-gated bounded directory query/response;
- `identity_manager` — Ed25519 key/PeerId, portable key serialization boundary, rotation; offline mnemonic backup/restore remains a transportctl/identity-file workflow, not daemon IPC;
- `address_book`;
- `kademlia_driver` — standard Swarm-owned adapter when the configured default-enabled Kademlia provider is present.

`direct_manager` does not own local endpoint leases. It carries fields and awaits runtime route-admission decision before `AcceptedV2`.

`endpoint_directory_manager` consumes bounded runtime snapshots and keeps no application descriptors.

## Endpoint directory cache

A small in-memory runtime/backend cache stores remote advertised EndpointIds with TTL. It is not DiscoveryProvider state, not peer-cache persistence, not Kademlia, and not application identity storage.

## Human client architecture

First-party human clients remain outside transport policy but share a Rust application core. Desktop uses IPC v2 exactly like Claude. Android uses an in-process `local-client-api` adapter because the foreground service embeds `TransportRuntime`; it does **not** create a second libp2p stack.

`human-core`, `human-store`, `human-ui-model`, and `human-ui-slint` never depend on `transport-libp2p`. Desktop admin/settings opens the separate admin socket. Android settings receives a distinct in-process `LocalAdminPort` only from explicit local UI composition. Contacts and ADR-0044 pending/unread/receiver-kept retention remain application state.
`human-store` implements ADR-0044 state transitions (`pending_outbound`, `unread_inbound`, `kept_inbound`) and must not expose a general durable conversation-history repository to first-party UI code.

## Testing packages

- `discovery-conformance-tests`;
- runtime endpoint-registry/property tests;
- libp2p direct-v2 and endpoint-directory multi-peer integration tests;
- IPC v2 compatibility fixtures;
- human+Claude same-profile endpoint-routing integration harness;
- security tests for endpoint probing/squatting/admin separation;
- standard-v1 Kademlia conformance/integration tests, including explicit opt-out zero-activity tests.

## Kademlia construction order

Per ADR-0034 the standard v1 build includes Kademlia support and configured entries default enabled. `enabled: false` remains an explicit opt-out meaning no Kademlia behavior/task/protocol. Kademlia never stores EndpointIds or endpoint-directory state.


## Mandatory connectivity modules inside `transport-libp2p`

Keep these concrete/backend-owned rather than adding unnecessary public traits:

- `address_registry` — merges verified direct and active relay-derived advertised addresses;
- `reachability_manager` — AutoNAT-v2 evidence aggregation and normalized `ConnectivitySummary`;
- `relay_manager` — authorized candidate selection, redundant reservations, failover and server-role quotas;
- `dcutr_manager` — bounded relayed-to-direct upgrade attempts, cooldown and stability handoff;
- `dial_admission` — root origin/class/backoff/resource gate shared by all Swarm behaviours;
- `connectivity_infrastructure_policy` — computes protocol-scoped infrastructure authorization from neutral config.

No new public `ConnectivityProvider` abstraction is required in v1: unlike discovery, these mechanisms jointly own one libp2p Swarm's path/reachability state and do not need independent backend replacement behind the Claude/human contracts. Neutral consumers see only `ConnectivitySummary` and path metadata through `transport-api`.
