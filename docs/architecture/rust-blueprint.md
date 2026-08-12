# Rust workspace blueprint

This is a package/dependency plan only; no crates are implemented in this repository.

## Proposed workspace

```text
crates/
  transport-api/          # NO libp2p, NO Claude; EndpointId/DirectDestination live here
  discovery-api/          # NO libp2p, NO Claude
  trust-api/              # NO libp2p, NO Claude
  config/                 # typed schema-v2 profile/config model
  kademlia-control-api/   # INTERNAL neutral driver port; NO libp2p, NO Claude
  discovery-cache/
  discovery-static/
  discovery-mdns/
  discovery-kademlia/     # optional/default-off; discovery-api + kademlia-control-api
  transport-libp2p/       # Swarm, direct-v2 codec, endpoint-directory protocol, GossipSub, identity
  transport-runtime/      # generic orchestration + EndpointRegistry/policies/route admission
  ipc-protocol/           # IPC v2 endpoint-aware frames; NO libp2p, NO Claude
  ipc-server/             # socket/capabilities + connection-bound endpoint lease adapter
  transport-daemon/       # composition root / CLI entry
  transportctl/           # local admin/diagnostics
  claude-channel-core/    # Channel-event/tool mapping, NO libp2p
  claude-channel-bridge/  # MCP SDK + IPC v2 client
  # human-client is application/UI technology choice and need not be Rust.
```

A TypeScript bridge/human UI remains viable. The boundary matters more than language.

## Dependency direction

```text
Claude bridge --------\
Human client -----------> ipc-protocol
Admin UI/transportctl --/       |
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

## ipc-server ownership

`ipc-server` owns OS connection lifecycle and capability authorization. On IPC v2 hello it asks `endpoint_registry` to grant an exclusive lease for the configured EndpointId. Connection drop releases that lease.

It does not decide network endpoint admission policy itself.

## transport-libp2p modules

- `swarm_task` — exclusive Swarm owner;
- `connection_manager` — trust-gated connection policy, address/backoff/limits;
- `dial_admission_gate` — applies to explicit and behavior-originated dials;
- `pubsub_manager` — GossipSub;
- `direct_manager` — `/direct/2.0.0`, request/response lifecycle and bounded runtime admission round trip;
- `endpoint_directory_manager` — `/endpoints/1.0.0`, trust-gated bounded directory query/response;
- `identity_manager` — Ed25519 key/PeerId, portable key serialization boundary, rotation; offline mnemonic backup/restore remains a transportctl/identity-file workflow, not daemon IPC;
- `address_book`;
- `kademlia_driver` — optional Swarm-owned adapter.

`direct_manager` does not own local endpoint leases. It carries fields and awaits runtime route-admission decision before `AcceptedV2`.

`endpoint_directory_manager` consumes bounded runtime snapshots and keeps no application descriptors.

## Endpoint directory cache

A small in-memory runtime/backend cache stores remote advertised EndpointIds with TTL. It is not DiscoveryProvider state, not peer-cache persistence, not Kademlia, and not application identity storage.

## Human client architecture

The human client is intentionally outside transport-runtime. It uses IPC v2 exactly like Claude for data-plane operations.

A human UI may have an admin/settings adapter that opens a separately authorized administrative IPC connection. The application can store contacts/history locally; no transport crate depends on those models.

## Testing packages

- `discovery-conformance-tests`;
- runtime endpoint-registry/property tests;
- libp2p direct-v2 and endpoint-directory multi-peer integration tests;
- IPC v2 compatibility fixtures;
- human+Claude same-profile endpoint-routing integration harness;
- security tests for endpoint probing/squatting/admin separation;
- optional Kademlia tests.

## Kademlia construction order

Unchanged from prior design. `enabled: false` means no Kademlia behavior/task/protocol. Kademlia never stores EndpointIds or endpoint-directory state.
