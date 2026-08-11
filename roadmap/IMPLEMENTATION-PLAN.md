# Implementation plan

## Phase 0 — empirical compatibility spikes

**Objective:** resolve version-sensitive or performance-sensitive questions without shipping production code.

**Deliverables:** reports for `SPIKE-001` through `SPIKE-004` in `roadmap/SPIKES.md`.

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
- config rejects unknown required providers and unsafe limits;
- golden IPC frames exist.

**Dependencies:** Phase 0 compatibility choices for version fields.

**Risks:** over-designing traits. Keep only Transport, DiscoveryProvider, and TrustPolicy as independently variable boundaries.

---

## Phase 2 — minimal libp2p transport

**Objective:** prove transport semantics with manually supplied peer addresses.

**Deliverables:** persistent identity manager; TCP+Noise+Yamux; signed/strict GossipSub; direct request-response; backend event normalization.

**Acceptance criteria:**

- two peers directly send and receive with explicit `Accepted` behavior;
- three peers broadcast via GossipSub;
- unauthorized source fails admission before normalized message event;
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
- known candidates can support direct send dialing within deadline;
- no-candidate direct send fails clearly without ad hoc global discovery;
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

## Phase 10 — Kademlia discovery (conditional)

**Objective:** add distributed candidate expansion only if static/cache/mDNS are insufficient.

**Deliverables:** KademliaDiscovery behind DiscoveryProvider, query bounds, diversity/poisoning diagnostics.

**Acceptance criteria:** common conformance suite; no channel provider records; failure isolation; acceptable bootstrap convergence/privacy; simulated poisoning/eclipsing tests meet agreed thresholds.

**Dependencies:** explicit ADR update after `SPIKE-003`.
