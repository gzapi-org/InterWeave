# Implementation plan

## Phase 0 — empirical compatibility spikes

**Objective:** resolve version-sensitive or performance-sensitive questions without shipping production code.

**Deliverables:** reports for `SPIKE-001` through `SPIKE-004` in `roadmap/SPIKES.md`; architecture-fixed GossipSub validation-result semantics are not reopened by a spike.

**Acceptance criteria:** target Claude Code release/channel manifest syntax is proven; MCP SDK choice is known; libp2p direct and GossipSub limits are measured; relay/NAT scope is confirmed.

**Dependencies:** none beyond isolated prototypes.

**Risks:** prototypes accidentally become production code. Keep them throwaway/outside shipping crates.

---

## Phase 1 — contracts and pure models

**Objective:** make semantic boundaries compile before networking exists.

**Deliverables:** `transport-api`, `discovery-api`, `trust-api`, `ipc-protocol`, typed configuration and pure event/error models.

**Acceptance criteria:**

- no dependency on libp2p/MCP in neutral crates;
- ChannelId/payload/error/capability tests pass;
- discovery conformance harness can run against a fake provider;
- config rejects unknown required providers, explicitly enabled known-but-unsupported providers, and unsafe limits;
- `TransportCapabilities.max_payload_bytes` reports the effective configured profile limit;
- transport v1 MessageId is exactly 128 bits;
- golden IPC request/event fixtures carrying exactly 49,152 opaque payload bytes plus maximal bounded metadata fit within the 131,072-byte JSON-body limit;
- over-limit IPC frames are rejected before dispatch.

**Dependencies:** Phase 0 compatibility choices for version fields.

**Risks:** over-designing traits. Keep only Transport, DiscoveryProvider, and TrustPolicy as independently variable boundaries.

---

## Phase 2 — minimal libp2p transport

**Objective:** prove transport semantics with manually supplied peer addresses.

**Deliverables:** persistent identity manager; TCP+Noise+Yamux; signed/strict GossipSub with explicit `Accept|Ignore|Reject` application validation; direct request-response; backend event normalization.

**Acceptance criteria:**

- two peers directly send and receive with explicit `Accepted` behavior;
- three trusted peers broadcast via GossipSub;
- objectively invalid GossipSub message maps to `Reject`;
- valid message from a locally unauthorized original publisher maps to `Ignore` with no local delivery/forwarding and no invalidity attribution solely for trust mismatch;
- authorized valid publisher maps to `Accept`;
- 48 KiB limit is enforced before large allocation;
- daemon restart fixture preserves PeerId;
- no claim of offline/durable delivery appears in API/tests.

**Dependencies:** Phase 1.

**Risks:** protocol ID/codec or GossipSub tuning invalidates assumptions; use Phase 0 evidence.

---

## Phase 3 — discovery framework

**Objective:** make candidate sources independently replaceable.

**Deliverables:** DiscoveryManager, PeerCacheDiscovery, MdnsDiscovery, StaticBootstrapDiscovery, common conformance suite.

**Acceptance criteria:**

- duplicate PeerId/address observations merge with provenance;
- expiry removes only the contributing source;
- a provider can fail/restart without transport shutdown;
- no provider dials or mutates trust;
- corrupt cache is quarantined and transport continues;
- mDNS can be disabled entirely.

**Dependencies:** Phases 1–2.

**Risks:** rust-libp2p behavior tempting provider->Swarm ownership leakage; enforce adapter boundary in tests/review.

---

## Phase 4 — connection management

**Objective:** turn candidate information into bounded, recoverable connectivity.

**Deliverables:** backend address book, dial scheduler, exponential backoff+jitter, reconnect policy, connection/global limits, successful-address observations to peer cache.

**Acceptance criteria:**

- network partition recovers without restart;
- repeated poisoned candidates cannot create an unbounded dial storm;
- untrusted candidates are not dialed for ordinary data-plane connectivity;
- inbound unauthorized PeerIds are closed before direct/GossipSub data-plane participation;
- outbound direct send to an untrusted PeerId fails locally as `UnauthorizedPeer`;
- known authorized candidates can support direct send dialing within deadline;
- no-candidate authorized direct send fails as `PeerUnknown` without ad hoc global discovery;
- candidate-present dial/protocol failure maps to `PeerUnreachable`;
- successful address observation reaches cache through the defined hint path.

**Dependencies:** Phase 3.

**Risks:** connection-manager scope expands into discovery or trust. Reject such coupling in code review.

---

## Phase 5 — local daemon and IPC

**Objective:** separate network/identity lifetime from Claude session lifetime.

**Deliverables:** profile lock, daemon supervisor, owner-protected UDS/named pipe, IPC handshake/framing, multi-client command/event fan-out, local control CLI skeleton.

**Acceptance criteria:**

- two local bridge-like test clients can attach to one explicit profile;
- independent profiles have independent PeerIds/sockets;
- bridge disconnect does not stop network;
- slow client does not stall Swarm or other clients;
- IPC major mismatch is rejected explicitly;
- exact max-payload request/event fixtures fit the 128 KiB JSON-body ceiling;
- a `claude-channel` IPC client is denied `admin.shutdown`;
- an authorized local control client can invoke administrative shutdown;
- cross-user unauthorized IPC is denied on supported OS test environments.

**Dependencies:** Phases 1–4.

**Risks:** same-user IPC attack remains residual; document and consider capability-token hardening if deployment requires it.

---

## Phase 6 — Claude Code Channel bridge

**Objective:** implement the official Channel integration without leaking networking internals.

**Deliverables:** MCP Channel bridge, push notification mapping, `broadcast/send/reply/join/leave/identity/status`, instructions, package metadata validated by SPIKE-001.

**Acceptance criteria:**

- external broadcast/direct event becomes exactly one valid Channel event under normal conditions;
- content/meta separation matches the Channel reference;
- reply token routes correctly for both modes;
- broadcast/reply without a caller-owned join returns `ChannelNotJoined` and never implicitly rejoins;
- untrusted direct destination returns `UnauthorizedPeer` before dial;
- transport `media_type` maps explicitly to Claude `content_type`;
- no Multiaddr/Swarm/connection ID appears in Claude tool schemas;
- ordinary assistant transcript is never represented as remote delivery;
- trust administration is absent from Channel tool surface;
- bridge restart leaves PeerId/network unchanged.

**Dependencies:** Phase 5 and current Claude compatibility result.

**Risks:** research-preview Channel API may change; isolate adaptation in bridge crate/package.

---

## Phase 7 — security hardening

**Objective:** make abuse/resource/identity failure behavior deliberate.

**Deliverables:** static allowlist administration, key initialization/rotation, per-peer rate limits, fuzz targets for IPC/direct codecs, log redaction, security regression suite.

**Acceptance criteria:** threat-model v1 mitigations have executable tests where practical; malformed input never panics daemon; private key never appears in logs/IPC; identity corruption fails closed.

**Dependencies:** end-to-end stack.

**Risks:** usability pressure to weaken trust defaults. Keep deny-by-default unless an ADR supersedes it.

---

## Phase 8 — operational packaging

**Objective:** support reliable install/update/diagnosis without changing architecture.

**Deliverables:** platform installers/service integration, status/diagnostics CLI, config migrations, documentation, update compatibility matrix.

**Acceptance criteria:** restart/update preserves identity; old/new compatible bridge-daemon combinations behave according to IPC matrix; rollback does not corrupt config/cache.

**Dependencies:** security-stable daemon/bridge.

**Risks:** service manager differences, Windows ACL/path behavior.

---

## Phase 9 — connectivity hardening (conditional)

**Objective:** meet actual remote deployment reachability targets.

**Deliverables:** Circuit Relay v2 client and/or AutoNAT/DCUtR only if approved after evidence.

**Acceptance criteria:** defined NAT matrix passes; relay unavailability degrades predictably; bootstrap and relay roles remain separate.

**Dependencies:** `SPIKE-004` and production deployment needs.

---

## Phase 10 — optional Kademlia peer-routing discovery

**Objective:** implement the already-specified Kademlia integration while preserving `enabled: false` as the default and all discovery/trust/connection boundaries.

### Phase 10A — backend driver

**Deliverables:** optional Swarm-owned Kademlia behavior slot, custom protocol derivation, explicit client/server mode, manual K-bucket insertion, Identify/address bridge, record filtering/no-record policy, bounded `KadControlHandle`.

**Acceptance criteria:** disabled config causes zero Kademlia protocol/query activity; unsupported build + enabled config fails; driver never owns trust or emits generic discovery events directly.

### Phase 10B — provider/scheduler

**Deliverables:** `KademliaDiscovery` behind `DiscoveryProvider`, seed-hint ingestion, bootstrap scheduler, targeted trusted server-peer lookup, random exploration, TTL/provenance normalization, health.

**Acceptance criteria:** common provider conformance suite; no ChannelId/application query keys; no direct provider dialing; query concurrency/rate/cooldown limits pass fake-time tests.

### Phase 10C — security/failure hardening

**Deliverables:** disjoint query paths, manual trust-gated routing insertion, routing-table/resource caps, record/provider-write rejection diagnostics, trust-revocation eviction, bootstrap/query failure isolation.

**Acceptance criteria:** poisoning/Sybil/eclipse simulations stay within resource bounds; no untrusted Kademlia routing/query connections; direct/GossipSub remain usable when Kademlia fails.

### Phase 10D — optional support qualification

**Deliverables:** 3/10/20-node integration matrix, protocol/config golden fixtures, operator docs for client/server deployment and seed diversity.

**Acceptance criteria:** all enablement criteria in `docs/architecture/kademlia-integration.md` pass. Shipping configuration and examples still keep `enabled: false`; operators must opt in.

**Dependencies:** SPIKE-003, rust-libp2p version revalidation, existing trust/ConnectionManager/Identify implementation.

**Risks:** trust-bounded DHT may not provide enough wide-area expansion; compromised trusted routers can still bias results; client/server deployment complexity; future request for open discovery-only DHT connectivity would require a new ADR and multiplexed-protocol admission design.
