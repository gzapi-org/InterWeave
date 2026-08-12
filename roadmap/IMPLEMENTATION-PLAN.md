# Implementation plan

No production implementation belongs in this architecture repository. This roadmap is for the subsequent implementation project.

## Phase 0 — empirical compatibility spikes

**Objective:** resolve version-sensitive/runtime-sensitive details.

**Deliverables:** SPIKE-001..006 results. SPIKE-002 validates direct v2 endpoint framing/acceptance, endpoint-directory behavior, and concurrent dedup reservation behavior; **SPIKE-003 is the Kademlia release gate; SPIKE-004 is the mandatory Internet-reachability release/tuning gate**; SPIKE-006 validates the exact Ed25519 recovery portability boundary.

**Acceptance:** exact Claude Channel package contract known; rust-libp2p direct v2 failure/negotiation semantics measured; Kademlia assumptions measured; mandatory AutoNAT-v2/Relay-v2/DCUtR behavior, dial-origin admission and deployment matrix validated by SPIKE-004; the exact-key mnemonic recovery boundary is empirically verified before production backup/restore is enabled.

---

## Phase 1 — neutral contracts and configuration v2

**Deliverables:** `transport-api`, `discovery-api`, `trust-api`, `ipc-protocol`, schema-v2 config/event/error models.

**Acceptance:**

- neutral crates import no libp2p/MCP;
- EndpointId and DirectDestination models compile;
- endpoint config uniqueness/default/subset/advertisement invariants pass;
- endpoint policy provably cannot widen profile trust;
- MessageId remains exactly 128 bits;
- DirectContentFingerprintV1 canonicalization/golden fixture and bounded reservation limits are frozen;
- Direct v2 `media_type_len=0` is frozen as absent media type / `media_present=0`;
- IPC endpoint-claim errors are exact (`EndpointUnknown`, `EndpointDisabled`, `EndpointClientKindDenied`, `EndpointInUse`, `CapabilityDenied`);
- identity types fix software v1 to Ed25519 and identity-recovery golden fixtures reproduce the expected PeerId;
- effective max payload capability is correct;
- IPC v2 max-payload fixtures with maximum endpoint metadata fit 131072-byte body;
- Kademlia default-on parsing plus cross-field/seed-source validation passes;
- mandatory connectivity config parses with AutoNAT-v2/relay-v2/DCUtR fixed on for standard v1; relay target/rate/concurrency cross-field constraints pass; every static probe/relay PeerId is authorized by data-plane trust or connectivity-infrastructure policy;
- enabled unsupported providers fail startup/config.

---

## Phase 2 — minimal libp2p transport v2

**Deliverables:** persistent identity, TCP+Noise+Yamux, signed GossipSub, `/direct/2.0.0`, `/endpoints/1.0.0`, backend event normalization.

**Acceptance:**

- two peers exchange DirectMessageV2 with explicit endpoint;
- omitted endpoint resolves receiver default;
- direct AcceptedV2 includes resolved endpoint;
- no_route is coarse across unknown/offline/policy-denied routes;
- 48 KiB and endpoint field limits enforce pre-allocation;
- endpoint directory is trust-gated/bounded/optional;
- three trusted peers broadcast through GossipSub with ADR-0029 validation mapping;
- no offline/durable claim.

---

## Phase 3 — discovery framework

DiscoveryManager + cache/mDNS/static/**Kademlia**, provider conformance, no trust/dial ownership. The standard v1 build implements the Kademlia provider/driver only after SPIKE-003 evidence passes; configured entries default enabled, while explicit `enabled: false` must yield zero protocol/query activity. Endpoint directory is **not** a DiscoveryProvider and never enters this layer.

---

## Phase 4 — connection management

Unchanged trust/backoff/dial ownership. Direct and endpoint-directory dials both use the same ConnectionManager/root admission policy. Kademlia behavior-originated dial handling remains as designed.

---

## Phase 5 — daemon, EndpointRegistry, and IPC v2

**Deliverables:** profile lock/service, EndpointRegistry, endpoint policy/default route, owner-protected IPC v2, endpoint lease handshake, per-client joins/queues, admin capability separation, transportctl skeleton.

**Acceptance:**

- one profile/PeerId supports simultaneous `human` and `claude` endpoint leases;
- same EndpointId double-claim -> EndpointInUse;
- direct event routes to exactly one resolved endpoint client;
- no local direct all-client fan-out;
- endpoint queue overload rejects before AcceptedV2;
- endpoint disconnect makes route unavailable immediately with no buffer;
- config disable revokes lease with no auto-rebind;
- remote directory snapshot reflects active advertise=true endpoints only;
- broadcast remains join-filtered and `channels.desired` remains no-buffer prewarm;
- slow client does not stall Swarm/other endpoints;
- IPC major mismatch clear;
- Claude/human data-plane clients lack admin.endpoints/admin.shutdown;
- claude-channel lacks endpoints.query by default; human-client receives it only when endpoint directory is enabled;
- EndpointId leases require negotiated IPC keepalive by default; missed probes release wedged leases, and explicit compatibility opt-out is tested;
- data/admin connections count independently toward max IPC clients.

---

## Phase 6A — Claude Code Channel bridge

**Deliverables:** MCP bridge, configured EndpointId IPC claim, Channel notifications with endpoint metadata, endpoint-aware `send`, exact-route `reply`, other tools/instructions.

**Acceptance:**

- direct Channel event includes source_endpoint/destination_endpoint;
- `send(peer, endpoint?)` routes correctly;
- source endpoint never comes from Claude tool input;
- reply token binds remote source endpoint + local lease epoch;
- stale token after reconnect fails without fallback;
- status reports local endpoint/lease + joined channels;
- no endpoint/trust/admin mutation tools;
- broadcast semantics unchanged.

---

## Phase 6H — human client data plane

**Objective:** provide a human-facing consumer without embedding libp2p or weakening transport boundaries.

**Deliverables:** desktop/TUI/CLI client architecture of choice; IPC v2 data-plane adapter; EndpointId claim; contact/routing UI; channel UI; optional application-local history; endpoint-directory route selection.

**Acceptance:**

- human and Claude can share one PeerId without duplicate direct delivery;
- UI distinguishes PeerId trust from EndpointId route label;
- direct send can target explicit route or remote default;
- offline human endpoint creates no daemon backlog;
- local history stores only app-observed messages and never claims network durability;
- network content cannot automatically invoke trust/endpoint/daemon administration;
- endpoint-directory labels are displayed as unverified routes unless separately app-verified.

The settings/admin UX may live in the same executable but uses a separately authorized IPC connection/capability path.

---

## Phase 7 — security hardening

Add rate limits/fuzzing and endpoint-specific regressions: route probing, directory enumeration, local lease squatting/conflicts, stale route tokens, admin confused-deputy behavior, and same-user residual boundary.

---

## Phase 8 — operational packaging

Add service integration, config-v2 migrations, endpoint diagnostics, human/Claude client compatibility matrix, reliable identity-preserving update/rollback, and offline `transportctl identity backup/verify/restore` UX for the ADR-0033 recovery format. Operational guidance states that complete profile disaster recovery requires both the recovery phrase and a separate `config.yaml` backup. Recovery words never transit daemon IPC.

---

## Phase 9 — mandatory Internet reachability

**Objective:** make standard v1 usable across consumer/enterprise NATs without weakening PeerId/application trust boundaries.

**Deliverables:**

- AutoNAT v2 client with multi-observer evidence aggregation and optional explicitly configured server role;
- Circuit Relay v2 client with redundant reservation manager, ephemeral relay-derived listen addresses and optional bounded server role;
- DCUtR manager with global/per-peer bounds, cooldown and direct-path stability handoff;
- address registry and normalized `ConnectivitySummary`;
- root `DialAdmissionGate` coverage for `autonat-probe`, `relay-reservation`, `relay-circuit`, and `dcutr-hole-punch`;
- connectivity-infrastructure peer authorization from ADR-0036;
- direct-first/relay-fallback path selection and network-change reconciliation;
- required diagnostics, metrics, resource limits, security regressions and deployment matrix.

**Acceptance:**

- SPIKE-004 evidence passes on the pinned rust-libp2p version;
- verified-public classification requires the configured distinct authorized AutoNAT observers and expires correctly;
- private/not-verified profiles maintain redundant relay reservations and advertise only active relay addresses;
- loss of one relay fails over; loss of all relays is surfaced without trust widening or hidden broker/storage fallback;
- relayed application peers are authenticated/authorized by their own PeerId, independent of relay authorization;
- infrastructure-only peers cannot participate in GossipSub/direct/endpoint/Kademlia data plane;
- direct-first path racing and DCUtR success/failure behavior match `CONNECTIVITY.md`;
- behaviour-originated dials obey global/per-peer backoff and connection ceilings;
- optional relay/AutoNAT server roles enforce all quotas;
- Model B endpoint/direct semantics are path-independent;
- standard-v1 release is blocked if these tests fail unless ADR-0035 is explicitly superseded.

**Dependencies:** Phases 1-8 contracts/runtime/security/operations plus SPIKE-004.

**Risks:** relay/provider availability and metadata exposure, NAT diversity, library-version behavior, hole-punch success variability, and extra Swarm resource pressure.


---
