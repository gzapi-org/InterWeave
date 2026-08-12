# Implementation plan

No production implementation belongs in this architecture repository. This roadmap is for the subsequent implementation project.

## Phase 0 — empirical compatibility spikes

**Objective:** resolve version-sensitive/runtime-sensitive details.

**Deliverables:** SPIKE-001..006 results. SPIKE-002 validates direct v2 endpoint framing/acceptance, endpoint-directory behavior, and concurrent dedup reservation behavior; SPIKE-006 validates the exact Ed25519 recovery portability boundary.

**Acceptance:** exact Claude Channel package contract known; rust-libp2p direct v2 failure/negotiation semantics measured; Kademlia/NAT assumptions measured as separately specified; the exact-key mnemonic recovery boundary is empirically verified before production backup/restore is enabled.

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
- IPC endpoint-claim errors are exact (`EndpointUnknown`, `EndpointDisabled`, `EndpointClientKindDenied`, `EndpointInUse`, `CapabilityDenied`);
- identity types fix software v1 to Ed25519 and identity-recovery golden fixtures reproduce the expected PeerId;
- effective max payload capability is correct;
- IPC v2 max-payload fixtures with maximum endpoint metadata fit 131072-byte body;
- Kademlia cross-field/seed-source validation still passes;
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

Unchanged core objective: DiscoveryManager + cache/mDNS/static, provider conformance, no trust/dial ownership. Endpoint directory is **not** a DiscoveryProvider and never enters this layer.

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
- optional IPC keepalive releases wedged endpoint leases after bounded misses;
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

Add service integration, config-v2 migrations, endpoint diagnostics, human/Claude client compatibility matrix, reliable identity-preserving update/rollback, and offline `transportctl identity backup/restore` UX for the ADR-0033 recovery format. Recovery words never transit daemon IPC.

---

## Phase 9 — connectivity hardening (conditional)

Relay/AutoNAT/DCUtR only as deployment evidence requires.

---

## Phase 10 — optional Kademlia peer-routing discovery

Implement the existing optional Kademlia blueprint only after SPIKE-003. Default remains `enabled: false`. EndpointId/presence must never be stored in Kademlia records.
