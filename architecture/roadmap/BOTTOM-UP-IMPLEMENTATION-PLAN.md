# Canonical bottom-up implementation plan

Status: **Accepted / normative implementation order** (ADR-0046)

This document defines the required construction order for the implementation workspace at repository root.

The numbered phase documents remain useful for **scope, product milestones, and release accounting**. They are not the dependency order in which production code should be activated. This document is the canonical dependency order.

The governing rule is:

> **A higher layer may not become functional until the contracts, invariants, fixtures, and conformance tests of the layers below it are green.**

A second mandatory rule is:

> **Root connection/dial admission must exist before any autonomous libp2p behaviour capable of originating dials (Kademlia, AutoNAT, Relay, or DCUtR) is enabled.**

A third rule is:

> **Run version-sensitive spikes immediately before the implementation boundary they unlock, then convert validated behavior into permanent regression/conformance tests.**

## 1. Dependency direction

```text
Frozen contracts / schemas / fixtures
              |
              v
       neutral API crates
              |
              v
  pure policies + state machines
              |
      +-------+--------+
      |                |
      v                v
 persistence       human domain
      |                |
      +-------+--------+
              |
              v
      minimal libp2p substrate
              |
              v
  root connection / dial policy
              |
      +-------+---------+
      |                 |
      v                 v
 direct + pubsub      discovery
      |                 |
      |              Kademlia
      |                 |
      +--------+--------+
               |
               v
 mandatory connectivity
 AutoNAT + Relay + DCUtR
               |
               v
       TransportRuntime
               |
       +-------+--------+
       |                |
       v                v
   daemon/IPC      embedded session
       |                |
   +---+---+            |
   |       |            |
 Claude  desktop      Android
               |
               v
   security / packaging / release
```

The root box is concrete, not aspirational. Prose contracts are paired with JSON Schemas under `architecture/contracts/schemas/` (ADR-0049) — the prose stays normative for **behaviour**, the schemas for **shape** — and frozen vectors under `fixtures/` are recomputed from their declared algorithms by `tools/checks/verify_fixture_vectors.py` on every CI run. Each schema's `x-contract.status` is `approved`: an authoritative implementation target, never a claim that anything implements it. **The flip to `active` is part of a stage's exit gate**, because "this stage is done" and "this contract now describes the wire" are the same claim; the flips are named per stage below.

## 2. Stage 0 — implementation foundation

### Objective

Turn the repository skeleton into a reproducible Rust/test workspace without implementing product behavior.

### Activate

```text
Cargo.toml
Cargo.lock
rust-toolchain.toml
xtask/
tests/support/
fixtures/
```

The root workspace starts with only the packages needed by this stage. Do not activate all planned members at once.

### Already in place — do not rebuild

`tools/checks/` carries the tree checks — ADR index/template conformance, semantic collisions, licence headers, contract validation, fixture recomputation, documentation integrity, and guard wiring — each with a self-test beside it, and `.github/workflows/ci.yml` runs all of them. Three fixture sets are materialized and recomputing (table below). Stage-0 work builds on this rather than duplicating it.

### Landed

- the toolchain is pinned in `rust-toolchain.toml`, and edition, MSRV, inherited lints, shared dependency versions and the release profile are declared once in the root `Cargo.toml`;
- `xtask` is the workspace's first member. `cargo xtask checks` runs the tree checks, `cargo xtask ci` adds fmt, clippy, tests and every self-test. It **calls** the `tools/checks` scripts rather than reimplementing them, and a unit test reads that directory from disk so a guard added later cannot be missing from the local run;
- `tests/support` (`interweave-test-support`) is the test-only harness, with a fixture loader and strict lower-case hex. Its suite proves the frozen vectors load from Rust with no product networking — the question `verify_fixture_vectors.py` cannot answer, since that script owns whether they are *correct*;
- `tools/checks/check_docs_integrity.py` is a real guard: relative links, heading anchors, and every YAML file and `yaml` block;
- CI reports a third context, `rust` (fmt, clippy, workspace tests).

### Work remaining

Nothing. Every exit-gate item below is met; the stage closes when its final change lands.

### Required fixtures

All materialized, and recomputed on every CI run by `tools/checks/verify_fixture_vectors.py` — 88 vectors. Each declares its algorithm, is recomputed from the specification rather than from the fixture, and is anchored to its ADRs. ADR-0047 InterWeave identifiers were the inputs throughout; no former working-namespace alias is materialized anywhere.

| Fixture | Derivation source | File |
|---|---|---|
| DirectContentFingerprintV1 | `contracts/ENDPOINTS.md`; golden in ADR-0047 | `fixtures/direct-v2/direct-content-fingerprint-v1.json` |
| DirectMessageV2 request framing (byte order pinned big-endian in `transport/libp2p/DIRECT.md`) | `transport/libp2p/DIRECT.md` §Request | `fixtures/direct-v2/direct-message-v2-frame.json` |
| BIP-39 entropy/checksum mnemonic + Ed25519 secret -> public key -> PeerId | `contracts/IDENTITY-RECOVERY.md` | `fixtures/identity/ed25519-bip39-entropy-v1.json` |
| GossipSubMessageIdV1 | `transport/libp2p/PUBSUB.md`; golden re-frozen by ADR-0047 | `fixtures/gossipsub/gossipsub-message-id-v1.json` |
| GossipSub topic key | `transport/libp2p/PUBSUB.md` topic mapping; golden in ADR-0047 | `fixtures/gossipsub/gossipsub-topic-key-v1.json` |
| Kademlia network hash / protocol namespace | `docs/architecture/kademlia-integration.md`; golden in ADR-0047 | `fixtures/kademlia/kad-network-namespace-v1.json` |
| IPC v2 maximum payload/frame (payload-fit invariant) | `contracts/LOCAL-IPC.md` §Framing | `fixtures/ipc-v2/ipc-v2-payload-fit.json` |
| EndpointId grammar vectors | `contracts/ENDPOINTS.md` + `contracts/schemas/endpoints/` | `fixtures/endpoints/endpoint-id-grammar-v1.json` |
| HumanChatV2 envelope vectors | `clients/human/HUMAN-CHAT.md` + `contracts/schemas/human-chat/` | `fixtures/human-chat-v2/human-chat-v2-envelope.json` |
| configuration-v2 vectors | `architecture/config/config.schema.yaml` + examples | `fixtures/config/config-v2-cross-field.json` |

Distinctness is a per-algorithm property. Derivation vectors must not collide — two edge cases sharing a digest means they stopped distinguishing anything. Verdict sets (`endpoint-id-grammar-v1`, `human-chat-v2-envelope`, `config-v2-cross-field`) repeat `true` and `false` by design, so the collision rule is off for them.

### Exit gate

- reproducible toolchain;
- every fixture in the outstanding table above is materialized and recomputing in CI;
- fixture tests execute without product networking;
- architecture integrity checks run through `xtask`/CI;
- no product crate above this stage is active.

## 3. Spike execution policy

Spikes are **just-in-time implementation gates**, not a large front-loaded phase.

| Spike | Run before | Decision/evidence that must be converted into permanent tests |
|---|---|---|
| SPIKE-002 | Stage 6 direct v2 | rust-libp2p request/response scheduling, concurrent same-key retries, negotiation/failure behavior |
| SPIKE-003 | Stage 10 Kademlia | driver behavior, autonomous dials, client/server mode, private namespace, routing/query behavior |
| SPIKE-004 | Stage 11 mandatory connectivity | AutoNAT v2, Relay v2, DCUtR, infrastructure class, dial admission, deployment/NAT matrix |
| SPIKE-006 | identity recovery implementation in Stage 3 | exact 32-byte Ed25519 secret import/export and same-PeerId restore |
| SPIKE-001 | Stage 16 Claude bridge | current Claude Code Channel/MCP packaging and runtime contract |
| SPIKE-005 | admin hardening when enabled | stronger same-user local admin boundary |
| SPIKE-007 | optional encrypted key-at-rest feature | selected audited envelope/KDF/AEAD behavior |
| SPIKE-008 | Stage 17 Android lifecycle/packaging | foreground service, secure recovery UI, backup/D2D behavior, store policy |
| SPIKE-009 | Stage 17 Android key custody | Android Keystore wrapping/invalidation and exact-PeerId preservation |

A spike directory is evidence gathering. Production code must not depend on a spike package.

## 4. Stage 1 — neutral contracts and configuration

### Objective

Implement the stable types and validation boundaries that every higher layer consumes.

### Activate

```text
crates/api/transport-api          # ACTIVE
crates/api/discovery-api          # ACTIVE
crates/api/trust-api              # ACTIVE
crates/api/local-client-api      # ACTIVE
crates/api/ipc-protocol           # ACTIVE
crates/api/kademlia-control-api   # ACTIVE
crates/config/profile-config      # ACTIVE
```

`transport-api` is a workspace member: identifiers, payloads, capabilities, status, and the error vocabulary, with `tests/schema_agreement.rs` holding them to the frozen schemas. `trust-api` follows it: deny-by-default `PeerTrustPolicy`, endpoint narrowing that cannot widen, and the ADR-0036 infrastructure set as a separate type. `discovery-api` completes the trio: candidates, provider descriptors, and the provider event stream, with no dependency on `trust-api` so a provider cannot reach a trust decision at all. `local-client-api` adds the session boundary: `admin.*` is not representable in a data session's capability set, and source endpoint is derived from the lease with no API accepting one. `ipc-protocol` adds the frame codec and handshake: the decoder refuses an over-ceiling declared length before allocating, and the authority domain comes from the accepting socket rather than the frame. `kademlia-control-api` adds the driver port, whose missing record and dial commands are the substance of peer-routing-only. `profile-config` completes the set: all five endpoint cross-field rules, tested against the sixteen frozen vectors in `fixtures/config/` rather than against a reading of the schema. All seven Stage 1 crates are now active; what remains for the exit gate is the `tests/transport-contract` suite.

### Hard dependency rule

These neutral contract crates must not depend on:

```text
libp2p
Slint
Android/JNI
SQLite
Claude/MCP implementation libraries
platform-specific socket/process types
```

### Implement

- PeerId representation boundary;
- EndpointId, ChannelId, MessageId and DirectDestination;
- payload/media-type models;
- transport capabilities/status/events/errors;
- ConnectivitySummary and path-neutral status types;
- discovery candidate/event/health/hint contracts;
- trust decisions and policy inputs;
- LocalDataSession / LocalAdminPort interfaces and models;
- IPC v2 request/event/error models;
- configuration-v2 structures and cross-field validation;
- Kademlia neutral control commands/results/snapshots.

### Tests

- pure unit tests beside types/validators;
- public API consumer tests where useful;
- frozen fixtures under root `fixtures/`;
- initial `tests/transport-contract` cases that do not require a backend.

### Exit gate

- all frozen limits and grammars match the architecture — checked mechanically, not by reading: serde types round-trip against the JSON Schemas under `architecture/contracts/schemas/` (an instance serialized from a Rust type validates against its schema, and every schema-valid instance deserializes), exercised in `tests/transport-contract` with a real JSON Schema validator. Note that instance conformance and definition agreement are different checks and both are needed: each crate's own suite compares enum members and bounds against the schema text, while `tests/transport-contract` validates actual serialized values;
- all config cross-field rules pass/fail exactly as specified;
- neutral crates remain free of backend/UI/platform dependencies;
- no real Swarm/networking exists yet.

Stage 1 flips **no** `x-contract.status`: types that compile are still an implementation target, not wire behaviour. The first flips come with the stages that put shapes on a wire or into a store.

## 5. Stage 2 — pure policies and state machines

### Objective

Implement security/routing/retention logic that can be exhaustively tested without sockets or libp2p.

### Activate

```text
crates/transport/runtime           # ACTIVE — pure modules first
crates/human/chat-protocol         # ACTIVE
crates/human/core                  # ACTIVE
```

`crates/transport/runtime` is a workspace member carrying its pure modules only: `endpoint_registry` first — leases, generations, deterministic default resolution, and the local/coarse failure split. The connection-policy and human-domain modules follow, each with its tests in the same change.

Backend-specific policy modules may be created under `crates/transport/libp2p` only if they remain pure and do not start a Swarm.

### Implement transport/runtime state

- EndpointRegistry state machine;
- exclusive EndpointId leases and lease generations;
- local subscription registry;
- endpoint policy intersection;
- deterministic default endpoint resolution;
- direct admission decisions;
- reply-token lifecycle;
- dedup key and positive-record semantics;
- in-flight reservation state;
- bounded direct ingress token-bucket state;
- resource accounting.

### Implement connection-policy state

- DialAdmissionGate decision logic;
- connection classes;
- per-address success/failure state;
- known-good address preference;
- address quarantine;
- peer-wide versus address-scoped backoff;
- pre-auth accounting decisions.

### Implement human domain state

- HumanChatV2 parsing/validation — envelope, markdown subset, bounded decompression (ADR-0050) — with conformance cases landing in `tests/human-chat`;
- message presentation state;
- pending/retrying/transport-terminal outbound state;
- unread/read/kept inbound state;
- Keep action as local-only post-read state transition;
- no remote input capable of selecting local retention.

### Mandatory retention transitions

```text
outbound pending
  -> transport terminal acceptance/publication
  -> remove durable pending copy

incoming
  -> unread
  -> durable

unread
  -> read + not kept
  -> remove durable copy

unread/read
  -> read + receiver Keep
  -> durable kept
```

### Exit gate

All state machines are deterministic and covered without real networking. In particular:

- endpoint ACL can only narrow trust;
- remote source endpoint cannot claim identity/authority;
- same message ID with different body is a conflict;
- default-endpoint changes do not reroute an already accepted retry key;
- remote content cannot set `Keep`;
- admin/data authority intersection cannot be widened by client-kind claims.

## 6. Stage 3 — persistence and identity/config storage

### Objective

Make durable state correct before network events can depend on it.

### Activate

```text
crates/human/store                 # ACTIVE
crates/discovery/cache             # ACTIVE
crates/config/profile-config       # ACTIVE — persistence/storage portions
```

`tests/human-retention` is a workspace member alongside them, carrying the `RETENTION.md` §9 conformance cases. Case 13 — Android system backup excludes the human store — is an `allowBackup` packaging property and stays open until Stage 17; the suite names it as uncovered rather than leaving its absence to be discovered.

Identity storage may be implemented in the lowest appropriate runtime/identity crate after SPIKE-006 validates the portability boundary. **SPIKE-006 passed** ([`spikes/spike-006/`](../../spikes/spike-006/README.md)) and `crates/identity/profile-identity` is that crate — the lowest one permitted to know about libp2p, translating to `TransportIdentity` at its own boundary so the PeerId type stops there. The derivation itself is already pinned: `fixtures/identity/ed25519-bip39-entropy-v1.json` recomputes entropy -> word indexes -> Ed25519 public key -> PeerId against the contract's golden on every CI run, so SPIKE-006's open question is narrowed to the libp2p API boundary — extracting and re-importing the exact 32-byte seed without transformation.

### Human store

Use purpose-specific durable tables/state such as:

```text
pending_outbound
unread_inbound
kept_inbound
contacts
preferences
```

Do **not** introduce a generic durable `messages` or `conversation_history` table.

### Discovery/peer cache

Implement bounded advisory persistence for:

- PeerId/address observations;
- TTLs and timestamps;
- successful-address observations;
- protocol capability observations;
- optional negative capability observations.

The cache is safe to delete and never contains trust authority or application messages.

### Identity/config persistence

Implement:

- profile path/state separation; **landed**;
- atomic config/state writes; **landed**;
- owner-only key storage for standard v1; **landed**, and `load` refuses a key whose mode has been widened rather than repairing it;
- exact Ed25519 identity persistence; **landed** in `crates/identity/profile-identity`, after SPIKE-006 established the seed boundary;
- optional mnemonic backup/verify/restore; **landed** — SPIKE-006 passed, and `verify` is a read-only path that touches no file;
- no mnemonic/private-key material in logs/IPC/network; **landed** structurally — `RecoveryPhrase` has no `Display` and no `Serialize`, and both it and `RecoveryRecord` redact their `Debug`.

### Tests

`tests/human-retention` must exercise real durable-store reopen/crash behavior:

```text
pending outbound survives restart
unread inbound survives restart
read-unkept is absent after transition/restart
receiver-kept survives restart
transport-terminal outbound is deleted
storage failure prevents accepting new human delivery
```

### Exit gate

Persistence invariants survive process restart and failure injection before any networking is allowed to rely on them.

With the mnemonic backup/verify/restore path implemented, flip `contracts/schemas/identity` from `approved` to `active` (ADR-0049) — the record shape stops being a target and starts describing real backup files. **Done**: `RecoveryRecord` produces and consumes that shape, and it is a boundary in the negative-conformance suite, so the claim is checked rather than asserted.

## 7. Stage 4 — minimal libp2p substrate

### Objective

Create the authenticated transport substrate only.

### Activate

```text
crates/transport/libp2p            # ACTIVE
```

### Implement first

```text
Swarm task ownership
Ed25519 PeerId
TCP
Noise
Yamux
Identify
bounded internal command/event channels
deterministic shutdown
```

Do not enable GossipSub, direct v2, Kademlia, AutoNAT, Relay or DCUtR yet.

They are **absent from the `libp2p` feature list** rather than merely unused, so none can be switched on by a `use` statement or a stray builder call. A behaviour that is not compiled cannot be enabled by accident, which is the cheapest way to keep §3's promise that admission policy is never retrofitted.

The dial path runs through the Stage 2 `ConnectionPolicy` from the first line of substrate code. Stage 5 owns making that gate **root** — behaviour-originated dials, the ConnectionManager, the retry scheduler, and feeding connection outcomes back into the policy so backoff has something to act on. What Stage 4 declines to do is ship a dial path with no gate and add one later.

### Exit gate

Two local test peers can:

- listen/dial through a temporary minimal harness;
- authenticate the expected PeerIds with Noise;
- exchange Identify state;
- shut down without leaked tasks;
- preserve identity across restart.

**Met.** `crates/transport/libp2p/tests/two_peers.rs` runs all five clauses over loopback TCP rather than a mock, and `shutdown` awaits the task's join handle — "without leaked tasks" is only checkable if something waited for the task to end.

## 8. Stage 5 — root connection and dial admission

### Objective

Create the mandatory security/control funnel before autonomous network behaviours are activated.

### Implement

```text
ConnectionManager
DialAdmissionGate
address book
known-good address selection
per-address quarantine/backoff
peer-scoped punitive backoff where justified
connection limits
pre-Noise pending-handshake limits
handshake timeout
per-source/pre-auth rate accounting
connection class reconciliation
```

Every future dial origin must be representable and observable:

```text
manual/direct
discovery reconnect
kademlia-query
autonat
relay
dcutr
```

### Required poisoning test

For a trusted PeerId with both a known-good address and an attacker-supplied wrong-key address, the wrong address must be quarantined without suppressing the known-good route through peer-wide punitive backoff.

### Exit gate

- root admission is the only policy authority for outbound Swarm dials;
- denied autonomous-behaviour dial attempts cannot reset backoff;
- pre-Noise work is bounded;
- address poisoning cannot peer-wide suppress a healthy trusted route.

No Kademlia/AutoNAT/Relay/DCUtR is enabled before this gate passes.

## 9. Stage 6 — direct protocol v2

### Prerequisite

Run and close **SPIKE-002** first.

### Implement

```text
/interweave/direct/2.0.0
DirectMessageV2 codec
AcceptedV2 / rejected status mapping
source/destination EndpointId handling
media_type absence encoding
48 KiB payload limit
DirectContentFingerprintV1
in-flight reservations
direct dedup
per-trusted-peer/global ingress rate limits
```

Initially route to an in-process EndpointRegistry/LocalDataSession implementation; desktop IPC is not required yet.

The codec is built against frozen bytes, not re-derived: multi-byte integers are big-endian — pinned in `transport/libp2p/DIRECT.md`, a gap the fixtures forced — and `fixtures/direct-v2/direct-message-v2-frame.json` carries the six framing vectors, including default-destination (`destination_endpoint_len = 0`), absent media, empty payload, and both endpoints at the 64-byte ceiling.

### Required real-network tests

Under `tests/direct-v2` and `tests/endpoint-routing`:

- explicit destination endpoint;
- omitted destination -> configured default;
- offline/unknown/policy-denied -> coarse `no_route`;
- Accepted only after exact endpoint queue admission;
- concurrent same-key retransmission -> one enqueue;
- same ID/different body -> conflict;
- retry after default endpoint change returns original accepted route;
- 48 KiB payload boundary;
- direct ingress rate limiting.

### Exit gate

Direct v2 is correct end-to-end between real Rust peers before IPC or UI integration exists.

Flip to `active`: `contracts/schemas/common`, `contracts/schemas/direct`, and the direct-routing shapes of `contracts/schemas/endpoints` (ADR-0049).

## 10. Stage 7 — GossipSub broadcast

### Implement

- signed GossipSub;
- strict validation;
- frozen `GossipSubMessageIdV1` based on signed source PeerId + GossipSub wire sequence;
- ChannelId -> topic derivation;
- ADR-0029 `Accept` / `Ignore` / `Reject` mapping;
- join/leave and local subscription state;
- resource/backpressure limits.

### Required tests

Under `tests/pubsub`:

- two authenticated publishers using the same application-envelope MessageId remain distinct at mesh dedup;
- invalid-signature traffic cannot poison the duplicate cache against later authentic traffic;
- unauthorized original publisher maps to Ignore without application delivery;
- objectively malformed/invalid traffic maps to Reject;
- authorized traffic propagates and delivers according to the frozen policy.

### Exit gate

Broadcast and direct semantics are independently functional and do not substitute for each other.

## 11. Stage 8 — endpoint directory

### Implement

```text
/interweave/endpoints/1.0.0
```

Requirements:

- trusted peer only;
- explicit `advertise: true` only;
- active endpoint leases only;
- at most 32 entries;
- grammar validation;
- TTL clamping and local-receipt aging;
- local sorting of valid unsorted responses;
- no identity or authorization semantics.

### Exit gate

Remote route discovery works without entering peer discovery, GossipSub, or Kademlia state.

Flip to `active`: the directory-response shape in `contracts/schemas/endpoints` (ADR-0049).

## 12. Stage 9 — discovery framework excluding Kademlia

### Activate

```text
crates/discovery/static
crates/discovery/cache
crates/discovery/mdns
```

Implement `DiscoveryManager` first, then providers one by one.

### Required common conformance

Every DiscoveryProvider implementation must pass `tests/discovery-conformance` for:

- start/shutdown;
- bounded event stream;
- normalized candidate output;
- duplicate/update/expiry behavior;
- health reporting;
- no trust grants;
- no application messaging;
- no ownership of dial policy.

### Exit gate

Static, cache and mDNS providers compose correctly and cannot bypass trust/ConnectionManager.

Flip to `active`: `contracts/schemas/discovery` (ADR-0049).

## 13. Stage 10 — Kademlia

### Prerequisite

Run and close **SPIKE-003**.

### Activate

```text
crates/api/kademlia-control-api   # ACTIVE
crates/discovery/kademlia
```

The Swarm-owned driver remains in `crates/transport/libp2p`.

### Implement

- private project-specific protocol namespace;
- client/server modes;
- manual trusted routing-table admission;
- Identify capability bridging;
- bootstrap/query progress;
- targeted lookup only with locally computable fresh server-capability evidence;
- bounded random exploration;
- effective target bounded by trusted population/max routing peers;
- no-progress saturation/backoff;
- Kademlia-originated dials through root DialAdmissionGate.

### Explicitly do not implement

```text
provider/value records
ChannelId records
EndpointId records
trust/membership records
application messages
```

### Tests

SPIKE-003 evidence converts into permanent cases under `tests/kademlia`: namespace derivation against the frozen golden, manual routing admission, bounded exploration/saturation, and dial-gate obedience for query-originated dials.

### Exit gate

- standard build supports Kademlia and configured entries default on;
- explicit `enabled: false` produces zero Kademlia protocol/query activity;
- small trusted overlays can become healthy/saturated;
- autonomous query dials obey root dial policy.

## 14. Stage 11 — mandatory Internet connectivity

### Prerequisite

Run and close **SPIKE-004**.

### Implement in this order

1. AutoNAT v2 client;
2. AutoNAT v2 server role;
3. Circuit Relay v2 client reservations;
4. Relay server role;
5. relayed inbound/outbound peer paths;
6. DCUtR;
7. direct-versus-relayed path preference/stability;
8. network-change invalidation/recovery.

### Mandatory invariants

- all behavior-originated dials pass DialAdmissionGate;
- connectivity-infrastructure peers never gain GossipSub/direct/endpoint/Kademlia authority merely by being connected;
- AutoNAT server dial-back candidate is literal IP, matches requester observed source IP, and rejects prohibited address classes;
- statically configured infrastructure is preferred; Identify-learned relay/probe promotion remains explicit opt-in;
- relayed pre-Noise accounting is charged to authenticated relay connection/PeerId plus global limits when original IP is unavailable;
- relayed destination trust is evaluated against the authenticated end PeerId, not the relay;
- DCUtR upgrade emits path change, not false logical reconnect;
- relay path remains fallback until direct stability rules permit preference switch.

### Tests

Under `tests/connectivity` and `tests/security`:

```text
public <-> public
private -> relay -> public
private -> relay -> private
multiple relay reservations
relay failure/failover
AutoNAT abuse/SSRF cases
DCUtR success/failure
network change
infrastructure peer protocol exclusion
```

### Exit gate

The mandatory standard-v1 NAT/relay/hole-punch matrix passes. At this point the low-level network engine is complete.

Flip to `active`: `contracts/schemas/connectivity` (ADR-0049).

## 15. Stage 12 — full TransportRuntime composition

### Objective

Combine the already-tested components behind neutral APIs.

### Implement

```text
TransportRuntime
├── TrustPolicy
├── EndpointRegistry
├── SubscriptionRegistry
├── DirectAdmission/dedup/rate limits
├── DiscoveryManager
├── ConnectionManager/DialAdmissionGate
└── libp2p backend
```

No libp2p types cross the transport/local-client boundary.

### Required suites

```text
tests/transport-contract
tests/local-client-conformance
tests/endpoint-routing
tests/interoperability
```

Run LocalDataSession conformance first against the direct in-process binding.

### Exit gate

A complete backend satisfies transport/local-session contracts without desktop IPC, Claude, Slint, or Android.

## 16. Stage 13 — daemon and desktop IPC v2

### Activate

```text
crates/local/ipc-server
crates/local/ipc-client
apps/transport-daemon
apps/transportctl
```

### Implement in order

1. IPC frame codec and maximum body enforcement;
2. hello/version negotiation;
3. data socket authority domain;
4. EndpointId lease/session binding;
5. event/command queues;
6. keepalive;
7. admin socket authority domain;
8. admin operations;
9. daemon lifecycle/profile lock;
10. transportctl.

### Tests

- `tests/ipc-v2` for wire/authority/error fixtures;
- `tests/local-client-conformance` against desktop IPC adapter;
- initial `tests/desktop-e2e` daemon lifecycle cases.

### Exit gate

IPC is proven to be only a serialization/process binding of LocalDataSession semantics, not a second behavior model.

Flip to `active`: `contracts/schemas/ipc` (ADR-0049).

## 17. Stage 14 — first-party human application core/UI

This work may proceed in parallel with Stages 4-13 after Stages 1-3 are stable, but it may not claim network completeness until Stage 12 exists.

### Activate/complete

```text
crates/human/core
crates/human/chat-protocol
crates/human/store
crates/human/ui-model
crates/human/ui-slint
```

### UI/domain states

At minimum:

```text
pending
retrying
transport accepted/published
unread
read
kept
connectivity/path state
PeerId trust state
EndpointId route label
```

No UI state may imply remote human read/processing without a future application-level receipt protocol.

### Development rule

Build and test UI against a fake/in-memory LocalDataSession first. UI code must not wait for or directly depend on libp2p. Envelope-level conformance stays in `tests/human-chat`; UI tests assert presentation state only.

With HumanChatV2 sent and received between first-party clients, flip `contracts/schemas/human-chat` to `active` (ADR-0049).

## 18. Stage 15 — desktop human client

### Activate

```text
apps/human-desktop
```

Compose:

```text
human-core
human-store
ui-model/ui-slint
ipc-client
```

The same executable may expose settings/admin UX, but the data connection and admin connection remain separate IPC authority domains.

### Required desktop E2E

- human + Claude can share one daemon PeerId under different EndpointIds;
- exact direct routing; no duplicate fan-out;
- unread persistence;
- read-unkept evaporation;
- receiver Keep persistence;
- pending outbox survives restart and disappears at transport terminal state;
- daemon restart/reconnect;
- admin/data socket separation;
- storage failure disables human endpoint/local channel delivery rather than accepting unread content unsafely.

## 19. Stage 16 — Claude Code Channel bridge

### Prerequisite

Run and close **SPIKE-001** against the target Claude Code release.

### Activate

```text
crates/claude/channel-core
apps/claude-channel
```

### Rule

The bridge consumes only transport-neutral local-client/IPC models. It must not depend on libp2p/discovery internals.

### Required integration tests

- incoming direct -> Channel event with source/destination endpoint metadata;
- endpoint-aware `send`;
- exact-route `reply` and stale-token failure;
- broadcast join/publish/reply semantics;
- human and Claude endpoints under same PeerId remain independently routed;
- no endpoint/trust/admin mutation tools.

## 20. Stage 17 — Android human client

### Prerequisites

Run and close **SPIKE-008** and **SPIKE-009**.

### Activate

```text
crates/human/android-platform
apps/human-android
```

### Composition

```text
Slint Activity
      |
 human-core/store
      |
 LocalDataSession
      |
 foreground Service host
      |
 embedded TransportRuntime
      |
     libp2p
```

Android does not add a localhost daemon/IPC transport just to imitate desktop.

### Implement in order

1. embedded LocalDataSession adapter;
2. Activity/service lifecycle;
3. foreground service;
4. notifications;
5. Android network-change binding;
6. Android Keystore wrapping of exact Ed25519 secret;
7. secure recovery Activity;
8. secure mnemonic UI/picker/no-clipboard path;
9. Android backup/device-transfer exclusions;
10. package/store metadata.

### Platform tests

Host Rust tests cover domain/session logic. Android instrumented tests cover actual OS behavior:

```text
Keystore
FLAG_SECURE / recents exclusion
foreground-service lifecycle
process death/restart
notification behavior
network callbacks
backup/device-transfer exclusion
user-presence restart diagnostic
```

Then `tests/android-e2e` proves Android <-> desktop P2P interoperability through direct/relay paths.

## 21. Stage 18 — full adversarial/security gate

Security tests are added continuously at each lower stage. This stage runs the complete release matrix together.

Under `tests/security`, cover at minimum:

```text
pre-Noise handshake floods
trusted-peer direct floods
GossipSub duplicate/signature poisoning
EndpointId probing/squatting/metadata abuse
address poisoning of trusted PeerIds
Kademlia poisoning/eclipsing bounds
relay smuggling/protocol-class violations
AutoNAT dial-back SSRF
admin/data socket spoofing
human-store/storage failure
identity recovery failure/tamper
Android key/backup/recovery failure cases
```

### Exit gate

No standard-v1 release while any threat-model regression test is failing.

## 22. Stage 19 — packaging, migration, and release

### Activate

```text
packaging/linux
packaging/macos
packaging/windows
packaging/android
```

### Validate

```text
fresh install
upgrade
rollback
profile/config migration
PeerId preservation
service/autostart lifecycle
uninstall semantics
recovery drill
Android update/reinstall behavior
```

The packaging layer must not invent new trust/network/application semantics.

## 23. Parallel workstreams

After Stages 1-3 are stable, implementation can proceed in parallel without violating dependency direction.

### Track A — network

```text
minimal libp2p
connection policy
direct v2
GossipSub
discovery
Kademlia
AutoNAT/Relay/DCUtR
TransportRuntime composition
```

### Track B — human application

```text
HumanChatV2
retention
SQLite store
ui-model
Slint
```

### Track C — integrations/platform

```text
IPC
Claude Channel
desktop composition/packaging
Android platform binding
```

The tracks converge at the frozen `TransportRuntime` and `LocalDataSession` boundaries.

## 24. Canonical implementation milestones

### M1 — contracts and deterministic domain

```text
neutral contracts compile
configuration validation passes
pure policies/state machines pass
all frozen fixtures pass
SQLite retention/restart tests pass
zero network product behavior required
```

### M2 — authenticated local-network transport

```text
Noise/Identify substrate
root DialAdmissionGate
direct v2
GossipSub
EndpointId routing/directory
static/cache/mDNS discovery
```

### M3 — complete network engine

```text
Kademlia
AutoNAT v2
Circuit Relay v2
DCUtR
complete TransportRuntime
transport/local-client conformance green
```

### M4 — desktop product integrations

```text
daemon + IPC v2
transportctl
desktop human client
Claude Channel bridge
shared-PeerId multi-endpoint E2E
```

### M5 — Android and standard-v1 release

```text
Android embedded runtime
Keystore/recovery/backup hardening
Android/desktop interoperability
full adversarial matrix
platform packaging/migration
standard-v1 release gate green
```

## 25. Test placement rule

Use ADR-0045's placement model and this proof rule:

> **Put a test at the lowest layer that can completely prove the behavior.**

Examples:

```text
pure retention transition
  -> crate unit test

SQLite crash/restart retention
  -> tests/human-retention

Direct v2 concurrent retry over real libp2p
  -> tests/direct-v2

Desktop admin/data authority separation
  -> tests/ipc-v2 + desktop-e2e

Android Keystore / FLAG_SECURE / backup policy
  -> Android instrumented tests
```

Do not replace a required real-network/platform test with a mock-only unit test.

## 26. Stage activation rules

When a stage starts:

1. add only the relevant crate/test manifests to the root Cargo workspace;
2. keep application roots thin;
3. add/update fixtures and permanent tests in the same change as protocol behavior;
4. do not enable a higher-stage behavior behind a default feature flag before its lower-stage gate passes;
5. do not silently contradict an accepted ADR/contract to make implementation easier;
6. when empirical spike evidence invalidates architecture, amend the ADR/contract first;
7. keep the Git tree green at every stage boundary.

## 27. Release interpretation of the old numbered phases

The historical phase numbers remain useful as scope labels:

- contract scope;
- minimal libp2p scope;
- discovery scope;
- connection-policy scope;
- daemon/IPC scope;
- Claude/human-client scope;
- security scope;
- operations scope;
- mandatory Internet-reachability scope.

However, **construction follows Stages 0-19 in this document**. In particular, connection/dial policy (Stage 5) precedes Kademlia (Stage 10) and mandatory Internet reachability (Stage 11), even though the older product phase table groups those concerns differently.

This ordering is normative because it ensures autonomous libp2p behaviours are introduced only after their security and resource-control boundary already exists.
