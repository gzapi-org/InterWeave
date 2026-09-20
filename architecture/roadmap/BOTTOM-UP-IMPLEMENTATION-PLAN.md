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

All materialized, and recomputed on every CI run by `tools/checks/verify_fixture_vectors.py` — 100 vectors. Each declares its algorithm, is recomputed from the specification rather than from the fixture, and is anchored to its ADRs. ADR-0047 InterWeave identifiers were the inputs throughout; no former working-namespace alias is materialized anywhere.

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
| Endpoint directory v1 framing (byte order pinned big-endian in `transport/libp2p/ENDPOINTS.md`) | `transport/libp2p/ENDPOINTS.md` §Endpoint directory protocol | `fixtures/endpoints/endpoint-directory-v1-frame.json` |
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
| SPIKE-002 | Stage 6 direct v2 | **CLOSED 2026-08-24, PASS** — rust-libp2p request/response scheduling, concurrent same-key retries, negotiation/failure behavior |
| SPIKE-003 | Stage 10 Kademlia | **CLOSED 2026-08-30, PASS for the stage; v1 release gate still open** — driver behavior, autonomous dials, client/server mode, private namespace, routing/query behavior |
| SPIKE-004 | Stage 11 mandatory connectivity | **PHASE A CLOSED 2026-09-01, PASS for implementation; the exit gate's NAT row was ruled satisfied by the containerised matrix on 2026-09-09 with three deferrals, and phase B's other five items are required before stage closure** — AutoNAT v2, Relay v2, DCUtR, infrastructure class, dial admission, deployment/NAT matrix |
| SPIKE-006 | identity recovery implementation in Stage 3 | **CLOSED 2026-08-19, PASS** — exact 32-byte Ed25519 secret import/export and same-PeerId restore |
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

Stage 4 does not enable GossipSub, direct v2, Kademlia, AutoNAT, Relay or DCUtR.

At Stage 4 they were **absent from the `libp2p` feature list** rather than merely unused, so none could be switched on by a `use` statement or a stray builder call. A behaviour that is not compiled cannot be enabled by accident, which is the cheapest way to keep §3's promise that admission policy is never retrofitted.

Each later stage added its own and only its own: `request-response` at Stage 6, `gossipsub` at Stage 7, `kad` at Stage 10, and `autonat`/`relay`/`dcutr` at Stage 11. **That list is now empty of behaviours this stage builds, so from Stage 11 on the promise is kept by the gate and its tests rather than by the compiler** (`mdns` and `dns` remain absent — see this stage's own section, which owns both) — which is the reason Stage 11 spends two whole steps on attribution and on SPIKE-004's D1/D2/D3 before it touches the manifest.

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

**Met.** `tests/connectivity/tests/stage5_dial_admission.rs` proves all
four over loopback TCP, and the structural half is in the type system
rather than in a convention:

- **Root admission.** The raw `Swarm` is private to `GatedSwarm`, whose
  `dial` takes an `AdmittedDial` that can only be *derived* from a
  `DialTicket` — so a call site that forgets to ask does not misbehave,
  it does not compile. The behaviour path is closed too:
  `OutboundAdmission` refuses any dial whose connection id the root
  admission did not just ticket, which is the hook Kademlia, AutoNAT,
  Relay and DCUtR will each go through.
- **Denied autonomous dials.** A refusal produces no ticket, and every
  path that records an outcome requires one. Proved for each autonomous
  origin in turn.
- **Pre-Noise work.** `PreAuthAdmission` answers
  `handle_pending_inbound_connection`, which runs *before* the upgrade
  and whose `Err` aborts it; the handshake timeout comes from the same
  limits, so the accounting and the transport agree about when a
  handshake is over.
- **Address poisoning.** A wrong-key address is quarantined and the
  expected peer's own backoff is untouched, so the route that was
  working stays dialable. Both halves are mutation-checked.

Every claim above was verified by breaking the code and watching the
test fail, which is the only evidence that distinguishes a test of the
behaviour from a test written from the same belief as the code.

Stage 6 did not open on this alone: its prerequisite was SPIKE-002.
That spike was run and closed **PASS** on 2026-08-23 — see
[`spikes/spike-002/`](../../spikes/spike-002/README.md) — and Stage 6
was opened on 2026-08-25, once that record had merged, by moving
`workspace.metadata.interweave.status` to `stage-6-direct-v2`.

## 9. Stage 6 — direct protocol v2

### Prerequisite

Run and close **SPIKE-002** first. **Closed 2026-08-23: PASS** —
[`spikes/spike-002/`](../../spikes/spike-002/README.md). It cleared the
withheld-`AcceptedV2` pattern, the bounded reservation map under real
request-response scheduling, and the GossipSub authenticity-before-cache
ordering, and it left four findings this stage inherits:

1. timeout attribution is a race — a responder that times out first
   leaves the requester reading `Io`, not `OutboundFailure::Timeout`;
2. a `ResponseChannel` held across an await may no longer be answerable
   when the answer is ready, and producing a response is not evidence
   the peer heard it;
3. `OutboundFailure::UnsupportedProtocols` is the major-version signal;
4. one connection serves both protocol families.

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
- concurrent same-key retransmission -> one enqueue (proven in process;
  the WIRE test is owed from the stage whose admission yields — see
  below);
- same ID/different body -> conflict;
- retry after default endpoint change returns original accepted route;
- 48 KiB payload boundary;
- direct ingress rate limiting.

### Exit gate

Direct v2 is correct end-to-end between real Rust peers before IPC or UI integration exists.

Flip to `active` (ADR-0049): `contracts/schemas/direct`, and the
direct-routing shapes of `contracts/schemas/endpoints` —
`direct-destination` and `endpoint-id`. From `contracts/schemas/common`,
`message-id` and `peer-id` only.

`endpoints/message-received` stays `approved`. It describes a JSON IPC
event with a required `mode` field; the in-process `DirectEvent` this
stage delivers has no such field and is not serialized at all, and the
direct wire carries the frozen binary frames instead. Marking it `active`
would tell a consumer that an unimplemented Stage 13 shape is current
behaviour.

`common/channel-id` stays `approved`, and Stage 7 closing did **not**
change that. ADR-0049 defines `active` as describing the **current
wire**, and no wire carries a ChannelId string. Stage 7 derives a topic
from one — `sha256("interweave/topic/v1\0" ‖ channel)`, transmitted as
the hex of that hash — and the `BroadcastMessageV1` envelope carries no
ChannelId at all, deliberately, so that a publisher cannot assert a
channel that disagrees with the topic it published on. Join, Leave and
Publish are in-process commands, not wire documents.

Flipping it would tell a consumer enumerating active schemas that raw
ChannelId strings are an implemented interoperable shape. They are not:
what crosses the wire is a hash, and the string exists only either side
of it. This is the same reasoning that keeps `endpoints/message-received`
approved above, for the same reason — the difference between a shape the
project intends and a shape something transmits.

The original wording named the whole of `common`, which was the mistake
worth recording: a family-wide flip would have carried schemas whose wire
does not exist.

`endpoints/endpoint-config` stays `approved`: the config shape is not a
wire at all. (`endpoints/directory-response` was `approved` here at Stage
6 because the directory exchange was still Stage 8 work; Stage 8
implemented `/interweave/endpoints/1.0.0` and flipped it to `active`.)

**Met.** Every clause of the implement list is exercised over loopback
TCP between two real peers, and the frozen framing is byte-compared
rather than re-derived.

- **Routing.** `an_explicit_destination_reaches_exactly_that_endpoint`
  and `an_omitted_destination_reaches_the_configured_default` cover the
  two selectors; `stage6_model_b_over_the_wire.rs` proves ADR-0030
  Model B as an invariant — each endpoint receives only what was
  addressed to it, and an endpoint name this stage never heard of routes
  like any other.
- **Coarse refusal.** `an_unknown_endpoint_is_indistinguishable_no_route`,
  `a_destination_endpoints_inbound_policy_is_coarse_no_route` and
  `every_resolve_failure_is_no_route_on_the_wire` hold unknown, disabled,
  unleased, missing-default and policy-denied to one wire code.
- **The acceptance point.**
  `a_full_endpoint_queue_is_overloaded_and_never_falsely_accepted` proves
  `AcceptedV2` follows queue admission rather than preceding it.
- **Dedup and retry.** `the_same_id_with_a_different_body_is_refused`
  and `a_matching_retry_replays_the_stored_route_after_the_default_moves`
  cover conflict and cached-route replay.
- **Payload boundary.** `a_payload_at_the_ceiling_survives_the_wire`,
  `an_over_ceiling_payload_is_answered_too_large` and
  `a_declared_payload_past_the_ceiling_is_too_large` cover 48 KiB from
  both sides, including a declared length that never arrives.
- **Ingress limits.** `stage6_ingress_rate_limits.rs` proves the per-peer
  burst is spent, that inventing source endpoints mints no allowance, and
  that a flooding peer does not spend a quiet peer's. The GLOBAL bucket
  needed its own case: the other three spend 64 and 96 against a burst of
  256, so a regression disabling the shared bucket would have passed all
  of them. `the_global_bucket_bounds_peers_that_are_each_within_their_own`
  puts sixteen peers at exactly their own allowance, so no per-peer bucket
  refuses anything and the 512 attempts are still cut down.

Verified by breaking the code and watching the specific test fail, not by
reading the tests and agreeing with them.

**The concurrent same-key retransmission clause is met by SCOPE, not by a
wire test**, and the distinction is recorded rather than glossed: the
ADR-0019 amendment of 2026-08-27 binds waiter retention from the first
stage whose admission yields while holding a reservation, and this
stage's admission does not yield. See below for what that leaves
unimplemented and what it does not excuse. Every other clause of the
implement list and the required-test list is proven above.

#### Met by scope: the concurrent-retransmission clause

Two separate things are true here, and an earlier draft of this section
conflated them into a claim of proof that does not exist.

**1. The runtime does not retain a waiter's channel, and is not yet required to.**
`handle_direct` passes `AttachedAsWaiter` straight to `waiter_response`,
which reads the dedup cache: a record means the owner already finished
and the waiter is answered with the stored route, and, **as this gap was found**, no record meant
the waiter was answered `overloaded` — a waiter attaching while the owner
was still in flight was refused rather than held. The reply is corrected
(the helper returns `None` and the caller asserts the branch is
unreachable); what remains unimplemented is the retention itself.

`contracts/ENDPOINTS.md` still requires that "an attached waiter holds a
response channel until the owner's admission resolves", and
`transport/libp2p/DIRECT.md` still says matching concurrent duplicates
"attach as waiters and receive the same eventual response". Both
sentences now carry the amendment's scoping beside them: the retention
binds from the stage whose admission yields, and a synchronous admission
may treat the branch as unreachable provided it does not answer it as
exhaustion.

So this is no longer a contract-to-code gap. It is a requirement with a
stated start, and this stage is before it — which is why the clause
above reads met by scope rather than met by proof. An earlier draft of
this section called it a gap, and that was accurate until the amendment
landed.

**2. Nothing tests it, and the in-process test does not.**
`a_concurrent_matching_copy_attaches_instead_of_enqueuing` asserts that
`admit_structured` returns `AttachedAsWaiter` and that nothing was
enqueued. It never calls `waiter_response`, so it cannot observe the
refusal above. Citing it as proof of the clause was wrong.

**Why no wire test exists either.** `handle_direct` is synchronous and
`admit_structured` acquires, resolves, enqueues and releases inside one
call without yielding, so two admissions cannot overlap and a second
arrival is always a dedup cache hit. SPIKE-002/A11 reached the waiter
path only because its harness parks the owner's `ResponseChannel` and
defers admission by a synthetic 600 ms — it models an admission that
yields, which is what admission becomes at the IPC boundary.

So the path is unreachable today AND unimplemented. Before the amendment
the second fact was the one that blocked this gate — an unreachable path
that is wrong is still wrong. The amendment changed which of the two
matters: the retention is not owed until admission yields, so leaving it
unimplemented here is no longer a gate failure.

What the amendment does NOT excuse is answering the branch wrongly, and
that half was fixed rather than scoped away — `waiter_response` returns
`None` and the caller asserts unreachability instead of replying
`overloaded`. The wire test becomes owed the moment admission yields.

**One thing this does NOT license.** The reservation map's waiter
accounting must not be removed as dead weight — A11 measured the
unbounded version as a memory-exhaustion vector, 40 copies attaching 39
waiters with zero refusals, and charging waiters against the same budgets
as owners is the fix. The bound is correct; what is missing is the
channel retention above it.

**Settled by the ADR-0019 amendment of 2026-08-27**, which scopes when the
rule binds rather than weakening it: waiter retention takes effect at the
first stage whose admission yields while holding a reservation — the
local-client IPC boundary — and until then the branch may be treated as
unreachable. The bound on waiters is untouched and mandatory in every
stage.

Retention was not implemented now on purpose. It would be a parking
mechanism for a path that cannot execute for several stages,
unexercisable end to end, and that is the shape — implemented,
unit-tested, called by nothing — that produced two P1s on PR #38 and
motivated `tools/checks/check_domain_fns_are_called.sh`.

**The code now says so rather than answering.** `waiter_response` returns
`Option`, so a missing owner outcome is an absence the caller must handle
instead of a refusal the function invents, and `handle_direct` asserts
the branch is unreachable rather than replying `overloaded`. If admission
ever yields without the retention being built, that assertion fires in
test builds; in release the exchange is left for the peer's retry, which
a settled owner then answers from cache — recoverable, where a wrong
answer would be final.

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

**Met.** Every clause of the implement list is exercised over loopback TCP
between real peers, and the frozen values are byte-compared against
`fixtures/gossipsub/` rather than re-derived.

- **Signed GossipSub, strict validation.** The behaviour is built
  `MessageAuthenticity::Signed` with `ValidationMode::Strict` and
  `validate_messages()`, so every message reaching the application has an
  authenticated source and must be reported exactly once.
  `invalid_signature_traffic_cannot_poison_the_cache_for_authentic_traffic`
  drives a peer that signs nothing while claiming another's identity, from
  a raw backend with validation disabled — the only way to emit what a
  conforming node cannot.
- **Frozen `GossipSubMessageIdV1`.** `mesh_message_id` is tested against
  the frozen vector without a Swarm, and
  `the_frozen_vectors_keep_two_publishers_and_two_sequences_apart` holds
  the two inputs distinct. The application envelope ID is never an input:
  two publishers may legitimately choose the same 128 bits.
- **ChannelId → topic derivation.** `topic_key_v1` reproduces every frozen
  vector, and `the_frozen_case_twin_is_a_different_topic` pins case
  sensitivity. The reverse map is total for every topic this node
  subscribed to, so a channel is never guessed.
- **ADR-0029 mapping.** Reject:
  `a_signed_but_malformed_envelope_is_reject_and_does_not_wedge_later_valid_traffic`.
  Ignore: `an_unauthorized_publisher_is_ignored_not_delivered_and_not_relayed_further`,
  four peers because the claim has three parts — not delivered at the
  neighbour, not forwarded to the peer behind it, and the honest relay not
  penalised. Accept: the same test's positive control.
- **Join/leave and local subscription state.**
  `the_last_leave_on_an_undesired_channel_drops_the_backend_subscription`
  and `leaving_a_desired_channel_keeps_the_mesh_warm` are twins, each
  failing the other's mutation;
  `only_the_joined_session_of_two_is_delivered_to` and
  `a_desired_channel_with_no_join_delivers_nothing_and_replays_nothing`
  hold delivery to explicit joins.
- **Resource and backpressure limits.** `a_broadcast_flood_does_not_wedge_the_direct_path`,
  `a_broadcast_to_many_sessions_cannot_overrun_the_outbox`,
  `a_full_session_queue_drops_for_that_session_and_the_mesh_still_forwards`,
  `repeated_unreachable_publishes_cannot_grow_the_outbox` and
  `a_final_leave_closes_the_session_queue_and_a_partial_one_does_not`.
- **Exit gate.** `broadcast_and_direct_are_independently_functional`.

**Three limits, stated because a `Met.` block that omits them is worse
than no block.**

- **The demotion layer is not isolable end to end.** `set_trust` closes a
  demoted peer's connection, blacklists it, and updates the broadcast
  trust copy; removing the third leaves
  `revoking_trust_stops_broadcast_delivery` passing, which mutation
  confirmed. The test proves the OUTCOME, not which layer produced it.
- **A literal `(source, sequence)` cache collision is not constructible.**
  `sequence_number` is assigned inside the backend, so no publisher
  chooses it and the exact pair a genuine publisher will next use cannot
  be forged from outside. What is observable — and tested — is that
  forged traffic bearing a publisher's identity does not stop that
  publisher's real message being delivered.

  The MECHANISM that makes this hold is upstream of the cache entirely:
  signature verification runs in the GossipSub codec's decoder, so a
  message that fails it becomes an invalid-message event with no source
  and no sequence number, and the behaviour that owns the duplicate cache
  is never reached. A forgery therefore cannot occupy an entry under ANY
  id — stronger than an ordering, and the reason the wire test cannot see
  it. Since that is a property of a dependency's internals, no test of
  ours can assert it and no version pin describes it:
  `tools/checks/check_gossipsub_rejects_bad_signatures_at_decode.sh`
  fails if an upgrade moves it.
- **Only the Accept arm of the validation report is verified.**
  Suppressing the report on Accept fails the four-peer control, because
  forwarding is what reporting Accept releases; suppressing it on Reject
  or Ignore is invisible end to end, since the unreported message occupies
  backend cache and blocks nothing.

**Deferred, with the stage that owns each.** The broadcast
`message-received` local delivery shape, `broadcast_reachability`, and
session-disconnect cleanup — `SubscriptionRegistry::release_session` —
all go to **Stage 13**, the daemon and desktop IPC v2. That is where a
client session first exists to disconnect and an admin surface first
exists to read a counter; Stage 8 is the endpoint-directory protocol and
has neither. `testing.md`'s reply-after-leave case goes to Stage 16 with
the bridge: `ReplyRoute::Broadcast` needs a session field before that
question can be asked.

## 11. Stage 8 — endpoint directory

### Inherited obligation: bind the source endpoint to the caller's lease

Carried forward from Stage 6 by an explicit maintainer decision recorded on PR #38, not by oversight.

Stage 6 enforces that a frame's `source_endpoint` names a lease the node actually holds, so an invented label is refused. It does **not** derive the source from the *caller's* lease: `configure_direct` leases every enabled endpoint, so a caller may name any configured one, have that endpoint's outbound policy applied, and be observed by the remote as that sender.

That gap is unreachable in Stage 6 — the runtime handle is the only caller and owns every endpoint, so "caller A names endpoint B" has no second party — and becomes real the moment IPC sessions exist, which is here. The property already exists one layer up: `local-client-api` derives the source endpoint from the lease and offers no API accepting one (Stage 1). This stage wires that boundary to the transport.

The shape: `send_direct` takes session or lease context and constructs or overwrites `source_endpoint` from it, rather than trusting the supplied frame. `contracts/ENDPOINTS.md` outbound step 1 and CLAUDE.md §5 are the governing text.

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

**Met.** Both halves shipped over loopback TCP between real peers under
`tests/endpoint-routing`, and the directory frame is byte-compared against
`fixtures/endpoints/endpoint-directory-v1-frame.json` rather than
re-derived.

- **The inherited obligation, closed as an unforgeable capability.**
  `send_direct` takes the `EndpointLease` that `claim_endpoint` returned,
  and `EndpointRegistry::holds_lease` verifies its 128-bit epoch against
  the live lease, so a caller sends only as an endpoint it actually
  claimed — `ENDPOINTS.md`'s "callers cannot spoof another local
  endpoint". `configure_direct` no longer auto-leases; a session claims
  one exclusively. `a_send_is_as_the_leases_endpoint_never_the_frames`,
  `a_lease_with_the_wrong_epoch_cannot_send`,
  `an_enabled_unleased_endpoint_is_no_route_until_claimed`, and
  `release_frees_the_endpoint_and_invalidates_its_lease` cover it, each
  with its mutation.
- **Trusted peer only, active advertised admissible routes only.**
  `advertised_for` lists an endpoint only when enabled, `advertise:
  true`, actively leased, and admissible for the querier under its inbound
  narrowing —
  `a_trusted_peer_learns_only_active_advertised_admissible_routes`, one
  test per conjunct. The query is refused locally for an untrusted peer
  (`querying_a_peer_you_do_not_trust_is_refused_locally`) and the rate is
  charged for every trust-admitted query
  (`a_disabled_directory_still_charges_the_query_rate`).
- **At most 32, grammar-validated, sorted, TTL-clamped from local
  receipt.** The codec refuses a bad grammar or an over-count frame
  (`endpoints_codec` unit tests); `validate_response` refuses more than 32
  or a duplicate and sorts an unsorted unique list; `clamp_ttl` is
  `min(remote, local, 300000)` from receipt, `generated_at_ms` never an
  input. `the_largest_legal_directory_crosses_the_wire`,
  `a_hostile_response_is_a_violation_and_an_unsorted_one_is_sorted`,
  `generated_at_ms_is_wall_clock_not_monotonic`.
- **Bounded, and configurable.** The requester bounds outbound queries
  (64 total, 4 per peer); the responder bounds concurrent responses at the
  configured in-flight ceiling, reserving a slot for every queued response
  including a refusal. The profile's `max_queries_per_minute_per_peer`,
  `max_inflight_queries` and `cache_ttl` are parsed, validated and
  applied, and a reload updates the budget and re-clamps cached entries in
  place. `the_configured_query_rate_is_honoured`,
  `the_profile_cache_ttl_reaches_the_requester_cache`.
- **Exit gate.** `route_discovery_touches_no_broadcast_or_discovery_state`
  and `the_directory_never_originates_a_dial`.

**Two limits, stated because a `Met.` block that omits them is worse than
no block.**

- **The responder's coarse `Unauthorized` arm is not reachable end to
  end.** An untrusted or infrastructure-only peer cannot hold an inbound
  connection at this stage, so the socket closes before a query — the
  connection layer performs the directory's exclusion for it (ADR-0036).
  Disclosure is prevented regardless by `advertised_for`'s own trust
  filter, which IS unit-tested
  (`an_untrusted_querier_is_shown_nothing`). The infrastructure-peer path
  that would exercise the responder's rate charge needs the relay stack
  (Stage 11).
- **The per-peer query rate is verified only through a served or disabled
  directory.** The 12/minute bound is unit-tested in `transport-runtime`;
  end to end the requester cache answers repeat queries to one responder,
  so a burst reaches the responder only when it does not cache — which is
  what `a_disabled_directory_still_charges_the_query_rate` and
  `the_configured_query_rate_is_honoured` use.

**Deferred, with the stage that owns each.** The IPC session and admin
surfaces — `LocalDataSession`, `LocalAdminPort`, `EndpointRegistry`'s
`default_endpoint`/`set_default`/`set_enabled`, and the directory cache's
admin introspection — go to **Stage 13**, the daemon and desktop IPC v2,
where a client session and an `admin.*` surface first exist. Stage 8
wired the lease boundary through the neutral `EndpointLease` capability,
not through `LocalDataSession`, which is why that type is still unwired.

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

**Met.** The three providers ship under `crates/discovery/{static,cache,mdns}`,
the manager is `transport-runtime`'s pure `discovery` module, and every
claim below is a named test under `tests/discovery-conformance` or beside
its source.

- **Every provider passes the shared suite, and the suite catches a
  provider that does not.** All fourteen `DISCOVERY-CONFORMANCE.md` tests
  are written once over a `Subject` trait and applied to all three —
  `every_provider_passes_the_shared_suite`. That alone would be worth
  little: a generic suite passes for a stub. So the crate also carries a
  `MisbehavingProvider` that emits before start, ignores the batch bound
  and keeps emitting after shutdown, and asserts the suite CATCHES each —
  `the_suite_catches_a_provider_that_emits_before_start`,
  `…that_ignores_the_batch_bound`, `…that_emits_after_shutdown`. The
  suite's own mutation check is part of the suite.
- **The suite requires an emission rather than validating one if it
  happens.** This is a correction, and it is recorded because the earlier
  version of this block cited the suite as evidence it could not carry.
  Each subject owned a `Box<dyn DiscoveryProvider>` and a function
  pointer, and mDNS learns exclusively through `push_discovered` on its
  concrete type — so its `observe` was a no-op, it emitted nothing, and
  every assertion nested inside `for event in drain_events(..)` was
  reached zero times. **An mDNS provider emitting no candidates at all
  passed all fourteen checks**, while "normalized candidate output" is a
  mandatory guarantee. Each subject is now a concrete type behind a
  `Subject` trait with a working input adapter — including
  static-bootstrap, whose reload path through `set_entries` was equally
  inert — and every check asserts that the event it is about actually
  occurred **and was about the observation**. `observe` returns the
  input tuple rather than a bare boolean, because knowing an observation
  happened proved only that an emission was required, not that it
  concerned the thing observed: a provider could turn an observation of
  P1 at one address into a valid, correctly-attributed candidate for P2
  at another and pass the entire suite.
  `the_suite_catches_a_provider_that_fabricates_an_unrelated_candidate`
  and `…that_expires_the_wrong_peer` make both permanent, and restoring
  the no-op adapter fails **four** of the fourteen —
  `provider_emits_normalized_candidate`,
  `provider_handles_duplicate_observation`,
  `provider_handles_candidate_update` and
  `provider_expires_when_semantics_support_ttl`.

  **The count is measured, and it was wrong here once.** This block
  claimed three, and at the closure it was ONE: `StaticSubject::observe`
  returned the very address `new()` had seeded and `start()` had already
  emitted, so `set_entries` diffed to empty and queued nothing, and the
  checks were satisfied by the leftover start event. The static half of
  the repair was therefore inert in exactly the way the mDNS half had
  been — the defect this block describes, surviving inside the sentence
  claiming it was fixed. Giving `observe` an address that differs from
  the seed, and matching an emission to its observation by peer AND
  address rather than peer alone, is what takes it from one to four.
  Anyone changing this suite should re-measure rather than trust the
  number: run each check against a subject whose adapter has been
  reverted to a no-op and count what fails.
- **Composition merges by PeerId and keeps provenance.**
  `the_three_providers_compose_into_one_candidate_set`;
  `a_candidate_survives_one_providers_retraction_when_another_still_vouches`
  is the address-lifetime rule — an address dies when no live source
  supports it, not when one withdraws;
  `a_long_running_node_keeps_its_configured_and_announcing_candidates`
  covers the configured-entry retention;
  `one_provider_cannot_speak_for_another` is the provenance refusal.
- **Health aggregates as DISCOVERY.md L105-109 specifies.**
  `starting_a_provider_makes_discovery_healthy_at_the_manager`,
  `a_quarantined_cache_reports_degraded_at_start`, and
  `aggregate_health_survives_one_degraded_provider` — one degraded
  provider does not make the node look broken.
- **The exit gate, over real sockets.**
  `a_discovered_candidate_cannot_bypass_trust_or_the_connection_manager`
  starts two real `SwarmRuntime`s on loopback, has
  `StaticBootstrapDiscovery` produce a perfectly good candidate for a
  reachable listener, and asserts the untrusting node neither remembers
  the address (`add_address` returns false: `learn_address` is keyed by
  trust class) nor can dial the peer. The positive control is in the same
  test — the same flow, a node that trusts the listener, which connects.
  Without it the assertions would prove only that the setup was broken.

Three limits, stated because the tests cannot reach past them.

- **The mDNS multicast MECHANISM was not built, `mdns` is not on the
  libp2p feature list, and `DISCOVERY-CONFORMANCE.md` was amended to
  defer its multicast tests to Stage 11 rather than leave a normative
  requirement quietly unmet.** At the stage's close, enabling it pulled
  `libp2p-mdns 0.48`, which pinned `hickory-proto 0.25.x`, carrying
  RUSTSEC-2026-0118 and RUSTSEC-2026-0119 with no upgrade available
  inside that line — `check_dependencies.sh` failed, and §8 makes that a
  gate rather than a warning. That blocker is retired: the `libp2p 0.57`
  bump (below, "the unlock") has since been taken, and what keeps the
  mechanism unbuilt today is the stage decision that sequences it, not
  the advisory. So `crates/discovery/mdns` ships its **normalization half
  only**: PeerId grammar, address bounds, dedup, expiry and the degraded
  report, driven by pushed observations rather than by a socket. The
  degraded arm is real (`a_quarantined_cache_reports_degraded_at_start`'s
  sibling, `aggregate_health_survives_one_degraded_provider`, drives
  `report_backend_down`); the discovering arm has never seen a multicast
  packet. **The exit gate's "mDNS provider composes correctly" is met for
  the provider and NOT for LAN discovery**, and anything that reads this
  stage as having proved LAN discovery is reading it wrong.
  **The unlock exists, measured 2026-09-19 at Stage 11's close of its
  list, and it is a libp2p major bump.** `libp2p 0.57.0` moves
  `libp2p-mdns` to 0.49.0 on `hickory-proto ^0.26` and `libp2p-dns` to
  0.45.0 on `hickory-resolver ^0.26`, and the resolved graph carries
  `hickory-proto 0.26.3`, past both advisories — so `mdns` AND `dns` (the
  other absent feature, which had no owner and was blocked by the same
  crate) come in together. First measured in a scratch worktree and since
  taken on its own pull request, where the costs below were paid as
  recorded:
  the bump costs (a) re-vendoring `libp2p-autonat` at 0.16.0 with
  ADR-0051's patch re-applied by hand — 0.16 has no re-test path and its
  server `Event` no dial-back outcome, and its RNG type and protobuf
  crate changed under the hunks (Decision 8); (b) sixteen compile errors
  in the transport crate alone before its tests — the request-response
  `Codec` trait drops `async_trait`, `relay::Event` gains
  `StatusChanged`, identity types move to `libp2p-identity 0.3` — plus
  whatever the test crates add; (c) a yanked `chacha20` in the new
  graph that `cargo deny check advisories` refuses and `cargo update`
  cannot name unambiguously; and (d) re-measuring every crate fact the
  Stage 11 records pin by version and line — the relay's admit-one-more,
  DCUtR's retry on dial failure, request-response's connection choice,
  the yamux guard. It was a PR of its own under Stage 11's dependency
  discipline, opened on the owner's word rather than folded into
  another, and it carried one cost the measurement had not predicted:
  the frozen spike harnesses path-depended on production crates, so the
  root move pulled a second `libp2p` major into locks pinned at the
  first — the frozen-spike rule in `SPIKES.md`'s preamble is what that
  produced.
- **The manager is a library, composed in tests.** There is no
  `SwarmRuntime` task driving it and no production holder; plan §15 is
  where TransportRuntime constructs one. The `stage-12` entries in
  `tools/checks/domain_fn_exempt.txt` are that gap written down, and they
  were re-dated at this closure because their previous reason — "the
  conformance suite composes the manager" — was wrong about what counts:
  that check strips `#[cfg(test)]` and excludes `tests/` wholesale.
- **`protocol_observations` were left empty at this closure**, per the
  Stage 10 deferral that stood at the time. Stage 10 decided the mapping
  (`kademlia-integration.md` §7, 2026-08-30) and `PeerCacheDiscovery` now
  fills them; the sentence is kept in the past tense because this block
  records what Stage 9 proved, not what is true today.

## 13. Stage 10 — Kademlia

### Prerequisite

**SPIKE-003 ran and closed on 2026-08-30: PASS FOR THIS STAGE.** It does
**not** close ADR-0034's v1 release gate — two required evidence items
are unmet, and both need infrastructure the spike does not have:
server-mode reachability evidence is not consumed (AutoNAT and Relay were
absent from the libp2p feature list when the spike ran — SPIKE-004), and
single-path capture
is not shown to be reduced (measured against controls; no capture was
observed at all, so the comparison cannot speak for the option).
Implementing this stage is unlocked; shipping configured entries
default-enabled is not.

Measured against `libp2p 0.56.0` with the `kad` feature. Record and
reproducing harness in
[`spikes/spike-003/`](../../spikes/spike-003/README.md); verdict and
findings in [`SPIKES.md`](./SPIKES.md). **Seventeen findings bind this
stage**, five of which say the gate cannot be written the obvious way and
three of which name API changes the production crates need. One reorders
the work:

> **Do not begin by enabling the feature.** The production
> `OutboundAdmission` refuses every dial carrying no root admission
> ticket, and every Kademlia query dial carries none — the spike measured
> this at the `handle_pending_outbound_connection` hook rather than
> inferring it. Turning `kad` on before the gate can admit a
> behaviour-originated dial *through* `PolicySnapshot::admit` under
> `DialOrigin::KademliaQuery` produces a subsystem whose every query dies
> at the first hop it lacks a connection for, silently. (SPIKE-003 wrote
> "surfaces as an ordinary dial failure"; SPIKE-004 measured that it
> surfaces as nothing — the Swarm discards the denial of a
> behaviour-originated dial, so Stage 11 owes a record at the gate.)
> Extend the gate first; the spike's `PolicyAdmit` mode is a measured
> proposal, not production code.

Two more that reading the design would not predict. A **routing insertion
starts one query nobody asked for**, and it dials — so the provider's
budget must account for it, and policy installed after seeding is
installed after the dial it meant to govern. And under
`BucketInserts::Manual` a **seed node routes nobody**: inbound
connections insert nothing, so a bootstrap hub answers every query with
an empty list until the provider admits the peers that dialled *it*.
`kademlia-integration.md` §7's admission pipeline reads as an outbound
story; the inbound direction is what a bootstrap node lives on.

**Server-mode reachability evidence is NOT validated.** AutoNAT and Relay
were absent from the libp2p feature list when the spike ran, so it could
not consume the AutoNAT-verified-or-relay-reservation rule this stage's
§14 requires. Stage 11 has since compiled both, which changes nothing
about what this spike established.
SPIKE-004 is where that arrives. Do not treat it as proved.

**The capability-observation mapping is DECIDED (2026-08-30). This
prerequisite is closed.** It is kept here rather than deleted because the
stage's remaining work inherits the decision, and because a reader who
came for the prerequisite needs to be told it was met rather than left to
infer it from silence.

The mapping is stated in `kademlia-integration.md` §7 and repeated in
`providers/peer-cache.md`: a stored observation is
`(protocol_family, wire_major, network_hash, role)` and a
`ProtocolObservation` carries one `protocol_id`, so the four are encoded
AS the derived server protocol string,
`/interweave/kad/<wire_major>.0.0/<network_hash>`, with `role = server`
implied by presence and the minor/patch always zero. `PeerCache::candidates`
fills the field, `PeerCacheDiscovery::add_hint` parses the exact grammar
back, and both directions are round-tripped against
`fixtures/kademlia/kad-network-namespace-v1.json` rather than against
each other.

The consequence that motivated the prerequisite no longer applies: a
targeted lookup built on the empty set read as "no peer supports this"
and silently degraded to no targeting at all.

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

SPIKE-003 evidence converts into permanent cases: namespace derivation
against the frozen golden, manual routing admission, bounded
exploration/saturation, and dial-gate obedience for query-originated
dials.

**Three of the four are NOT under `tests/kademlia`, and should not be.**
This sentence originally said they all would be, which sends an auditor
to a directory holding two files and invites the conclusion that two
cases are missing. CLAUDE.md §4 puts a test at the lowest layer that
completely proves the behaviour, and only two of these need two real
runtimes over real sockets. Where each one is:

| Case | Where | Why there |
| --- | --- | --- |
| namespace derivation vs the frozen golden | `crates/transport/libp2p/src/runtime/kademlia_driver.rs` — `the_namespace_matches_every_frozen_vector` | Pure derivation. Reads every vector from `fixtures/kademlia/kad-network-namespace-v1.json` and refuses an empty fixture, so a derivation that agrees only with itself cannot pass. |
| manual routing admission | `crates/transport/libp2p/tests/kademlia_driver.rs` — `a_trusted_server_routes_and_a_client_never_does` | Needs two runtimes: admission turns on Identify arriving over a real connection, and the client half is the control. Covers the inbound direction SPIKE-003's seed-node finding named — the hub routes a peer that dialled *it*. |
| bounded exploration/saturation | provider unit tests, plus `tests/kademlia/tests/overlay_health.rs` | The pacing, backoff and saturation conjuncts are a state machine and belong beside it. Reaching `Healthy` needs a real driver feeding a real provider, which only the suite has. |
| dial-gate obedience for query-originated dials | `crates/transport/libp2p/tests/kademlia_driver.rs` — `the_gate_refuses_the_walks_dial_to_a_stranger`, with `an_exploration_converges_the_star_through_admitted_dials` as the admitted control | A behaviour-originated dial only exists inside a running Swarm. Both directions, because a gate that refuses everything would pass the refusal half alone. |

`tests/kademlia` holds the two cases that are only observable from
outside a single process: the opt-out, and the overlay reaching health
across the port.

### Exit gate

- standard build supports Kademlia;
- explicit `enabled: false` produces zero Kademlia protocol/query activity;
- small trusted overlays can become healthy/saturated;
- autonomous query dials obey root dial policy.

**Shipping configured entries DEFAULT-ENABLED is not a clause of this
stage.** It was, and the first clause read "standard build supports
Kademlia and configured entries default on" — one bullet bundling a
build capability with a shipping decision. The two are answered by
different things and at different times, and bundling them made a gate
this stage could not pass however good its tests were.

The build capability is a Stage 10 property and is met. The default is
**ADR-0034 §7's v1 release gate**, which makes SPIKE-003 a release gate
rather than an implementation prerequisite: the conformance, security
and integration evidence is required before the standard v1 build ships
with the default enabled. SPIKE-003 closed PASS FOR THE STAGE and
explicitly not for that gate — server-mode reachability evidence is
unconsumed (SPIKE-004) and single-path capture reduction is unmeasured.
It additionally needs a composition root to express a default at all,
which is Stage 12.

So the clause moved to where the decision lives rather than being
dropped. **A reader who followed the old text would have concluded Stage
10 can never close**, since no test in this stage can reach the default;
that is a change of substance and it was taken as an explicit owner
decision, recorded here rather than in a `Met.` block, because a gate
that a stage cannot satisfy is a defect in the gate. Stated in the gate
itself so the gate and
`[workspace.metadata.interweave].status` cannot drift apart again.

**Met.** The port ships as `crates/api/kademlia-control-api`, the
provider as `crates/discovery/kademlia`, and the Swarm-owned driver in
`crates/transport/libp2p`. **Every clause of the exit gate above is met
by a named test.** That gate is one clause shorter than the one this
stage opened against: the shipping default was split out of it and moved
to ADR-0034's release gate, for the reasons stated with the gate itself.
Read this block together with that split — the closure rests on both.

- **The opt-out produces nothing, and each half carries a control that
  is verified to FIRE.** `tests/kademlia/tests/opt_out.rs`: a disabled
  profile answers no Kademlia command, beside an enabled node that must
  answer the same one, and advertises no DHT protocol — read off the
  wire from a third party's Identify rather than from local
  configuration. The first version of the second test inferred the
  advertisement from whether another node routed the subject; that
  passed under mutation, because a peer can fail to route for reasons
  that have nothing to do with the protocol list. Inferring from a
  consequence was the wrong instrument.
- **A query dial obeys the root gate in BOTH directions.**
  `the_gate_refuses_the_walks_dial_to_a_stranger` and
  `an_exploration_converges_the_star_through_admitted_dials`. Only the
  pair is evidence: a gate that refused everything would pass the
  refusal alone. This is the clause SPIKE-003 reordered the stage
  around — the feature could not simply be switched on, because every
  query dial is behaviour-originated and carries no admission ticket.
  SPIKE-003's other unpredicted finding is covered beside it: under
  `BucketInserts::Manual` an inbound connection inserts nothing, so a
  bootstrap hub would route nobody, and
  `a_trusted_server_routes_and_a_client_never_does` has the hub routing
  a peer that dialled *it* — the direction §7's admission pipeline reads
  as an outbound story.
- **A small trusted overlay becomes healthy, and that test is the only
  place the port's two halves run against each other.**
  `tests/kademlia/tests/overlay_health.rs`. Everything else tests one
  side against the port's DEFINITION, so a driver that emits an event
  the provider mis-reads satisfies both suites. **Its bounds are part of
  the claim**: it proves the driver-to-provider direction — drop the
  ingest and health never arrives — and does NOT prove the provider's
  commands are what convergence depends on, because the library's own
  automatic bootstrap finds the third node whether or not any command is
  delivered. A three-node star rather than a pair because with one
  trusted peer the provider is target-satisfied the moment that peer is
  routed and never explores; the two-node version passed with every
  command discarded.
- **The build supports Kademlia.** The port, the provider and the driver
  are exercised against real nodes in
  `crates/transport/libp2p/tests/kademlia_driver.rs`.

**What this closure does NOT clear**, and the reason the split was made
rather than the clause quietly dropped: shipping configured entries
default-enabled remains blocked on **Stage 12** composition — nothing
constructs a provider, so there is no site where a default could be
expressed — and on **SPIKE-004**, which supplies the server-mode
reachability evidence ADR-0034 requires and which SPIKE-003 could not.
A later stage AND a later spike. Stage 10 closing is not evidence for
either, and no build may ship the default on until both land.

What the stage did NOT establish, beyond that clause: **server-mode
reachability evidence is not validated at all.** AutoNAT and Relay were
absent from the libp2p feature list for the whole of Stage 10, so §14's
AutoNAT-verified-or-relay-reservation rule was never exercised here.
Nothing in this block should be read as evidence for it. Stage 11 has
since compiled both, which changes nothing about what Stage 10 proved.

### Why the shipping default was never reachable here

The gate above records the split; this records the fact underneath it,
which outlives the amendment and is the thing a Stage 12 implementer
needs.

**`KademliaDiscovery` is constructed nowhere outside its own crate.**
The provider and the Swarm-owned driver each implement
`kademlia-control-api` and are each tested against it; no production
code connects one to the other. The transport runtime owns the driver
and emits `SwarmEvent::Kademlia`, and nothing consumes those events as a
provider. That is what a neutral port is for and what composition is
for, not an omission in this stage — but it means anything phrased about
PROVIDER state has no production path to observe it, and a *default a
user configures* has no site to be expressed at.

`tests/kademlia/tests/overlay_health.rs` joins the halves by hand, which
is enough for a clause about behaviour and cannot stand in for the
composition root a default needs. That is the whole distinction between
the clauses this stage met and the one that moved.

## 14. Stage 11 — mandatory Internet connectivity

### Prerequisite

Run and close **SPIKE-004**. **Phase A closed 2026-09-01: PASS FOR
IMPLEMENTATION.** The work below is authorized. What is NOT authorized
is calling the stage complete — phase A ran on one machine over
loopback, so the exit gate's NAT/relay/hole-punch matrix was unmet (the
NAT row has since been ruled satisfied; see the 2026-09-09 ruling below,
and the relay and hole-punch rows remain unmet) and
**phase B is required before stage closure**: a public VM and
home/symmetric/carrier NAT (the NAT row of this item is the one ruled
satisfied below; the public VM and the carrier's CGNAT are its
deferrals), two independently operated relay/probe
services, **relay loss and capacity denial**, interface change,
hole-punch success rates, measured resource cost against the default
budgets. That is six items,
matching `SPIKES.md` in content as well as count. It did neither until
2026-09-08: the capacity-denial row was missing entirely, so a reader
who counted here got five, and the first item read "real and carrier
NAT" — dropping the public VM, which is the half of that item this
stage's own deferral discussion turns on.

**AN OWNER DECISION WAS TAKEN ON 2026-09-09, and it is recorded here
because `SPIKES.md` points at this section for it.** The ENVIRONMENT for
the NAT rows now exists at
[`spikes/spike-004/phase-b/`](../../spikes/spike-004/phase-b/README.md) —
containerised, two NAT domains, with BOTH halves of RFC 4787's
classification MEASURED — the mapping class, chosen with nftables, and
the filtering class since `filter.sh`. Measured rather than configured
for the filtering half's default row, where the absence of a rule IS the
row: masquerade's own reverse path is already address-and-port-dependent,
and the harness's two other filtering modes exist so the classifier has a
positive control for each branch rather than one reachable answer. It
closes none of the six by itself. The question put to the owner was
whether the exit gate's NAT row can be satisfied by a containerised matrix
with the population claim, the public VM and a carrier's CGNAT explicitly
deferred — the filtering half was on that list and is not any more — the
shape Stage 9 used for mDNS and Stage 10 for the release gate.
**The ruling: yes, with those three deferrals recorded.** The NAT row of
the exit gate is satisfied by that matrix, measured in both halves RFC
4787 defines. The three deferrals are what was carved OUT of that row and
carried forward as named limits rather than discharged: the public VM
and a carrier's CGNAT are the parts of item 1 above that the same item
also names and a container is not, and hole-punch success rates against
NAT as deployed are item 5, which the row never claimed. **So the ruling
satisfies the NAT row of item 1 and touches no other item**: two
independently operated relay and probe services,
relay loss and capacity denial, network-interface change, success rates
and resource cost against the default budgets are all still phase B —
success rates appearing both as a deferral out of the NAT row and as an
open item, because it is both — so the stage still cannot close on this
evidence alone. It can
close on it once steps 3 through 10 land and the remaining five are met
or deferred with the owner's explicit go-ahead, which is a separate
decision and is NOT taken here. No row other than the NAT one is marked
met.

The verdict and its binding findings are in
[`SPIKES.md`](./SPIKES.md); the record is
[`spikes/spike-004/`](../../spikes/spike-004/README.md), numbered F1
through F13 there. These change the order or the content of the work
below and are repeated where they bite:

- **Attribution comes before the features.** Enabling AutoNAT, Relay or
  DCUtR without it means every reservation and probe is refused as
  `KademliaQuery` against the infrastructure the stack needs. The spike
  ran the shipped gate in front of a real relay client and measured
  exactly that, with the relay's trust class as the only variable. This
  is the same shape as SPIKE-003's "do not begin by enabling the
  feature", and it is why the ordered list below now begins with
  attribution rather than with AutoNAT.
- **A gate refusal of a behaviour dial is invisible.** The Swarm
  discards the `Err` from a denied pending hook, so there is no
  `Dialing` and no `OutgoingConnectionError`; only the originating
  behaviour is told, and an observer sees its reaction instead — in the
  spike, a relay listener closing *successfully*. Whatever this stage
  builds at the gate must record its own refusals, because nothing
  downstream will.
- **`AUTONAT.md` §7 is not implemented by the crate**, and the check
  must run at the PENDING hook: the established hook runs after the
  socket is open, which is after the target has been contacted.
- **D1, D2 and D3 sit in already-shipped code**, not in this stage's
  work, and none is reachable in a shipped build today — nothing
  constructs `DcutrHolePunch` or `RelayCircuit`. The features-on change
  removed the second half of that reason: `relay` and `dcutr` are
  compiled now, so absence of code is all that keeps them latent, and
  it lasts only until this stage builds the paths they govern. `DcutrHolePunch` and `RelayCircuit` were both
  admitted for a `ConnectivityInfrastructureOnly` peer; the fix was not
  "add both to the data-plane predicate", because `RelayReservation`
  must stay outside it and D2 differs from it precisely in naming the
  DESTINATION rather than the relay. And `PreAuthAdmission` bucketed a
  relayed inbound by the source PeerId the circuit carries, which is
  the "unbounded pseudo-source bucket" `contracts/CONNECTIVITY.md` §10
  forbids by name. **D2's architecture clarification landed on
  2026-09-03** — ADR-0036's amendment gave the matrix the row it lacked
  — and **all three code fixes landed in step 2** (D1 and D2 on
  2026-09-04, D3 on 2026-09-05), before DCUtR or relayed paths are built
  rather than after. The description above is kept in the past tense on
  purpose: it is what a reader needs to understand why step 2 exists,
  and it stops being true the moment step 2 is read as a record of work
  already done.
- **ADR-0036's inbound relayed clause had no implementation site.** The
  shipped gate was outbound-only, so a relayed inbound was never
  evaluated against the authenticated end PeerId at all. The spike
  measured that the end PeerId and the relay's are both available at
  the destination's established hook, which is where the decision
  belongs; step 7, which built relayed peer paths, made it there
  (`retention_origin` in `dialing.rs`: a relayed connection of either
  direction is judged under `RelayCircuit`, so only a data-plane far
  end is retained over a circuit), and
  `tests/connectivity/tests/relayed_paths.rs` pins it with the
  destination serving probes and circuits.

**And one thing phase A does NOT unlock: the protocol-isolation
correction** — though see the note at the end of this paragraph, because
half of the evidence it says is owed has since landed outside the spike. The exposure invariant below is about what an
infrastructure-only connection is OFFERED — four data-plane protocols
installed uniformly — and the phase-A harness carries none of them. Its
`SpikeBehaviour` is Identify plus the three connectivity behaviours, so
it can show which control protocols such a peer advertises and nothing
about whether ours are withheld from it. That correction needs a node
carrying the data-plane behaviours beside a real infrastructure-only
connection; treat it as evidence still owed, not as a step the verdict
authorized.

**Half of it has since landed, and outside the spike.** Stage 11's
`tests/connectivity/tests/advertised_protocol_set.rs` runs exactly such
a node, over loopback and in CI, with no relay code — an
`InfrastructureSet` is ordinary configuration — and RECORDS what is
advertised in the window before the refusal. It does not assert it: that
emptiness is scheduler-dependent and `CONNECTIVITY.md`'s matrix permits
Identify for this class, so §14 treats it as an observation. What it does not supply
is the RETAINED case, which is what the correction actually needs and
what remains owed.

Server-mode reachability evidence for ADR-0034's v1 release gate — the
item SPIKE-003 could not supply — is still outstanding and belongs to
phase B. Stage 10's `Met.` block says the same from the other side: its
§14 rule was never exercised, because AutoNAT and Relay were absent from
the libp2p feature list.

### Implement in this order

**Steps 1 and 2 are SPIKE-004's, and they come before any behaviour is
enabled.** The list below used to begin at what is now step 3, and
following it would have enabled a dialling behaviour while every
unticketed dial was still classified `KademliaQuery` — refused, as a
data-plane origin, against the infrastructure the stage exists to use.

**Five obligations sit on this stage without being numbered steps
below.** They are recorded here because the features-on change is the
moment each became visible, and a manifest comment — or a test comment —
is not where a stage's obligations belong. Two of them (`dns`, and the
spike-lock drift) predate Stage 11; they are named here because nothing
else names them.

- **`mdns` — a deadline this stage was given and has not yet met:
  the unlock is taken, the mechanism decision is taken (2026-09-20),
  and the multicast tests are not yet run, below.**
  `contracts/DISCOVERY-CONFORMANCE.md`'s 2026-08-30 amendment defers the
  mDNS multicast tests to Stage 11 by name, "because that is where the
  libp2p feature set is next revisited under SPIKE-004, and where the
  dependency graph is re-resolved anyway", and states that this is a
  deadline rather than a preference. The revisit has now happened, in
  two parts. The feature could not be enabled on the graph pinned at
  the time: RUSTSEC-2026-0118 and -0119 were unresolved inside the
  `libp2p-mdns 0.48` line `libp2p 0.56` selects, so §8's dependency gate
  refused it. **But the advisories were not unresolvable, and that was
  measured at this stage's close** (2026-09-19; the Stage 9 record above carries
  the measurement and its cost): `libp2p 0.57` selects `libp2p-mdns
  0.49` on `hickory-proto ^0.26`, whose resolved `0.26.3` is past both,
  and `dns` clears with it. **The bump has since been taken**, so the
  pinned graph carries `hickory-proto 0.26.3` and the advisory gate is
  clean: what stands between `mdns` and its multicast tests is no
  longer an upstream fix nor a dependency decision, but the multicast
  MECHANISM Stage 9 never built, and what stands before `dns` is the
  transport construction §15's precondition names. **The deadline is
  still UNMET on those terms**, and the stage cannot
  quietly inherit Stage 9's deferral a second time. The three options it had: take the
  bump before this stage closes (a PR of its own — the Stage 9 record
  costs it: re-vendoring `libp2p-autonat` at 0.16 with ADR-0051's
  patch re-applied, the transport crate's compile fallout, a yanked
  `chacha20` in the new graph, and every crate fact the Stage 11
  records pin by version re-measured) — TAKEN; close without it by re-deferring
  the tests explicitly, with an amendment naming the bump as the
  unlock and the stage that will take it, the way Stage 9 did; or
  close without it and leave the deadline recorded as unmet, which is
  the option the amendment exists to avoid. **What is no longer
  available is re-deferring on the premise that there is nothing to
  wait for** — that premise is now false in this repository's own
  record.
  **Decision taken 2026-09-20, by the owner: the multicast MECHANISM
  is built, in this stage, by p2p-network-dev.** The deadline reads
  TAKEN-NOT-MET until `DISCOVERY-CONFORMANCE.md`'s multicast tests run
  against the built mechanism, and MET when they do. One rule travels
  with the build, because the library it wraps forces the question:
  `libp2p-mdns 0.49` pushes `NewExternalAddrOfPeer` for every
  discovered pair with no trust check, so a discovery provider that
  let it through would write the Swarm's address book from a LAN
  broadcast. ADR-0011 (A 2026-09-20) says a discovery provider never
  does that: the emission is swallowed at the wrapper and the only path
  from a multicast packet to a dialable address is the provider's
  normalization, bounds and dedup into `DiscoveryManager`, then
  `ConnectionManager` admission. That rule binds the next provider too.
- **`dns` — an accepted contract with no implementation; owned since
  2026-09-20.**
  `discovery/providers/static-bootstrap.md` says DNS resolution happens
  when the dial path consumes the multiaddress, and `profile-config`
  accepted `/dns4` and `/dns6` accordingly until 2026-09-19, when it
  began refusing them in a build without `dns` (`AddressHostNotBuilt`;
  §15's precondition). The `dns` feature is not
  enabled and the Swarm's transport is TCP alone (plus the relay client's
  when one is configured), so such a dial fails
  `MultiaddrNotSupported` — which `attempt_is_structural` classifies as
  structural, so `record_permanent_failure` runs and
  `known.remove(ticket.address())` drops the address from the book.
  **Say what that does and does not mean today.** `known` is the address
  BOOK, written only by `learn_address` — from a successful establish,
  from Identify, or from the `LearnAddress` command — so what is
  forgotten now is an Identify-learned or explicitly-learned `/dns4`
  address, dialled once and removed. No configured bootstrap address
  reaches it at all: nothing composes a static-bootstrap provider,
  `DiscoveryManager` never dials, and provider composition is Stage 12.
  So the live defect is the book eviction; the sharper consequence —
  a configured DNS bootstrap peer discarded on first use — arrives with
  Stage 12 unless this is fixed first, which §15 now makes a
  precondition of composing any profile that names a DNS host. Either way it contradicts
  `static-bootstrap.md`, which says the ConnectionManager applies its
  normal bounded retry and backoff. This predates Stage 11 and is named
  here because nothing else names it. **Owner assigned 2026-09-20
  (architect-cto, on the owner's instruction to settle it):** `dns` is
  a transport construction and belongs to p2p-network-dev, as one
  of this stage's five obligations, sequenced AFTER the mDNS mechanism above
  and BEFORE Stage 12 composes any profile that names a `dns4`/`dns6`
  host — §15's precondition is what it discharges. Building it means:
  the DNS transport constructed in the Swarm (the feature flag alone
  is nothing, per `static-bootstrap.md` §DNS ownership), the
  `AddressHostNotBuilt` refusal lifted for a build that has it, and the
  book-eviction defect above closed so a resolution failure is a
  retried dial failure and not a forgotten address. It is not started
  until the owner says the mDNS mechanism has landed, unless the owner
  reorders.

- **The connectivity behaviours ship GATED OFF, and `ClassGated<B>`
  lands before the first commit that reaches ANY of the three routes to
  a retained infrastructure-only connection** — a wrapped behaviour
  announcing a reachability origin, an `attempt_dial` call site passing
  one, or a relaxation of the inbound arm (§14 enumerates them).
  Owner's ruling, 2026-09-07, taken
  when phase 1b's config half was about to model
  `transport.connectivity` — whose schema makes `relay.client.enabled` a
  `literal[true]`, so modelling it is what would otherwise construct a
  relay client by default. Recorded here rather than only in `CLAUDE.md`
  because this is where the construction order lives: a decision about
  ordering stated only in the operating contract is the shape Stage 10's
  exit gate had, where a shipping decision sat somewhere that could not
  enforce it. The restriction itself is §14's protocol-isolation
  invariant below; the commit it must precede is **step 3's**, not step
  5's, because step 3 reaches routes 2 and 3 at once — it dials a static
  AutoNAT server under `AutonatProbe` (route 2, an `attempt_dial` call
  site of ours) and must relax the inbound arm to serve a dial-back
  (route 3). **Route 1, not 2, is what this said until 2026-09-09, when
  the pinned crate was read rather than assumed**: the AutoNAT v2 client
  emits only `ExternalAddrConfirmed`, `GenerateEvent` and
  `NotifyHandler`, so it never dials and there is no behaviour-originated
  dial to classify; the dial-back is the SERVER's, and belongs to step 4
  — which reached route 1 with it on 2026-09-18.
  A guard written as a grep over `attempt_dial` call sites would see
  route 3 but not route 2 — and route 2 is exactly what a grep does see,
  which is the reverse of what this paragraph used to claim.

- **An accepted document describes an infrastructure-only state this
  build cannot hold, and step 3 owes the decision.**
  `transport/libp2p/CONNECTIVITY.md`'s protocol matrix gives Identify and
  bounded ping a `yes` in the infrastructure-only column, and ADR-0036
  opens a clause "on an established infrastructure-only connection:".
  Today no such connection survives long enough for either: the inbound
  arm closes it in the same loop iteration as `ConnectionEstablished`.
  That is MEASURED, not asserted — deliberately, since asserting it would
  resolve this very conflict in code, which is what this bullet exists to
  avoid. What `tests/connectivity/tests/advertised_protocol_set.rs`
  asserts is only that such a peer is established and then closed.
  Step 3 has to relax that arm anyway for the AutoNAT dial-back, so it is
  the step that must decide whether the documents or the code move
  (CLAUDE.md §2 — the conflict is named here rather than resolved in
  prose). **DECIDED with step 3's adapter (2026-09-17): the code moved
  to the documents.** A retained infrastructure-only connection now
  exists — an AutoNAT server's inbound, kept under `AutonatProbe` while
  this profile holds an outbound to it — and it carries exactly what the
  matrix grants that class: Identify and the client's dial-back
  protocol, measured by `tests/connectivity/tests/autonat_client.rs`
  as an exact set. Bounded ping is not constructed anywhere yet, so
  the matrix's `yes` for it is still a target, not a claim.

- **Committed spike locks drift silently when the root manifest
  changes, and nothing checks them.** A spike harness is its own
  workspace but path-depends on production crates — and
  `crates/transport/libp2p` declares `libp2p = { workspace = true }`,
  which resolves against the ROOT manifest wherever that crate is built,
  including from inside another workspace. (Not feature unification,
  which does not cross a workspace boundary; getting the mechanism right
  matters for the check below, which must re-resolve each spike rather
  than inspect a unified feature set.) Its committed `Cargo.lock` then
  names fewer packages than the build needs. `cargo metadata --locked` fails;
  a plain `cargo run` rewrites the lock instead, silently, destroying the
  pinning the README claims. SPIKE-003's and SPIKE-004's READMEs now
  document `cargo run --locked`, and SPIKE-002's does since this change;
  SPIKE-006 commits no lock, so `--locked` would fail there and it names
  the plain form correctly. Stage 11's
  features-on change did exactly this to SPIKE-003 — though **that lock
  was already broken before it**, missing the
  `interweave-kademlia-control-api` package since Stage 10 plus three
  dependency edges on `interweave-transport-libp2p`
  (`interweave-discovery-api`, `interweave-kademlia-control-api`,
  `sha2`), so the features-on change
  added a third reason rather than the first. It was found in review
  rather than by any check. **SPIKE-002's harness is in that state too**,
  needing `interweave-discovery-api`, for a reason unrelated to Stage 11
  — it path-depends on no crate that reaches libp2p. Both were stale for
  pre-Stage-11 reasons; only one is also stale for a Stage 11 reason. The durable fix is a tree
  check asserting `--locked` resolves for every committed spike lock,
  wired into CI like any other guard; writing
  it means fixing SPIKE-002's lock in the same change so the guard can
  be green when it lands.
  **DONE 2026-09-19** (`tools/checks/check_spike_locks.sh`, wired into
  CI and `cargo xtask`, with a self-test whose positive case is a lock
  the manifest has outgrown). Two things the guard measured that this
  paragraph had wrong, and they are worth keeping because both are the
  same mistake — a count taken once and then trusted. **All THREE
  committed locks were stale, not two**: SPIKE-004's was too, and no
  review had named it. And what each was missing was SMALLER than
  recorded — SPIKE-002 the `interweave-discovery-api` package and two
  edges (`bs58` and `interweave-discovery-api`, and no `either` edge at
  all: its harness path-depends on nothing that reaches
  `interweave-transport-libp2p`), SPIKE-003 and SPIKE-004 a single
  `either` edge each; the
  `interweave-kademlia-control-api` package and the three edges this
  paragraph names had been resolved by intervening work. The locks were
  therefore updated MINIMALLY (a plain resolve, not
  `cargo generate-lockfile`, which rewrites from scratch and moved
  eighty-odd packages onto newer patch versions when tried): twelve
  inserted lines across the three, nothing removed, no pinned version
  moved — so each spike's evidence still corresponds to the versions it
  was measured at, which is the whole reason a spike commits a lock.

**Between step 2 and step 3 sits a change with no number: `autonat`,
`relay` and `dcutr` entered the libp2p feature list.** It is unnumbered
because it constructs nothing and therefore proves nothing — no field, no
constructor, and at the time no configuration either — so it is not a step
anyone can be at. The configuration half is spent: the paragraph below is
the change that supplied it. It
is recorded here because the steps below were written when those features
were absent, and several of them cited that absence as the reason a rule
could not be violated. Those reasons are spent: from here the guarantee
is the outbound gate, the trust classification and their tests.

**A second unnumbered change sits beside it: the profile document now has
a `transport.connectivity` block.** Unnumbered for the same reason — it
constructs nothing. `profile-config` models the whole section the schema
defines, enforces every range, the cross-field rules (seven at the time; an eighth, the AutoNAT client's refresh interval below its evidence lifetime, joined with step 3's adapter) and the two
pinned-literal classes, and refuses a static relay or AutoNAT server
whose PeerId is in neither `trust.allowed_peers` nor
`transport.connectivity.infrastructure.allowed_peers`. **That last rule
is why the block landed before anything reads it**: SPIKE-004 measured
that a gate refusal of a behaviour-originated dial surfaces as nothing at
all, so an unauthorized configured relay would be a relay that never
connects with no diagnostic anywhere, and the only legible place to
refuse it is configuration.

It also supplies the **first production constructor of an
`InfrastructureSet`** — ADR-0036's second class had been expressible in
code since Stage 5 and in a profile document never, until this block —
by making the `infrastructure` field that type rather than a parallel
one, so the ceiling and the bounded-sequence guard are not duplicated. A profile
that says nothing about transport gets standard v1's client roles
configured, both server roles off, and an empty infrastructure set. What
it does NOT do is build a behaviour: the owner's 2026-09-07 ruling stands
and nothing constructs an AutoNAT client, a relay client or DCUtR from
this block.

1. **dial attribution**: every behaviour-originated dial reaches the
   root gate under its own `DialOrigin`, and the map that carries it
   drops a note for a dial the Swarm refuses before the pending hook;
   the COMMAND PATH sets `RelayCircuit`, since the transport rather
   than the behaviour dials a circuit: `attempt_dial` builds the
   `DialRequest` carrying the origin and admission runs there, and
   `AdmittedDial::from_ticket` — called by `attempt_dial` immediately
   after admission, before the Swarm is touched — enforces the
   address/origin PAIRING both ways. **The attribution and the pairing
   enforcement landed in PR #71**; no caller supplies `RelayCircuit`
   yet, and the reason is the command path rather than any behaviour —
   a circuit is dialled by the transport, so no behaviour supplies this
   origin by design; what is absent is a call site, and the relay
   transport the builder never installs — every
   `attempt_dial` call site today passes `Manual` (`runtime/commands.rs`)
   or comes from the retry tick. The mechanism is there and its first
   user is step 5. **The `relay` FEATURE no longer arrives with step 5**:
   it was compiled ahead of the ordered list below, together with
   `autonat` and `dcutr`, in a features-on change that constructs
   nothing. What step 5 brings is the constructor.
   **One label decides whether relaying works at all**, and step 2's
   move of `RelayCircuit` into `names_application_destination` is what
   armed it: `relay::client::Behaviour` emits two dials of its own
   (`libp2p-relay 0.21.1` `priv_client.rs:334` and `:373`) — a
   reservation, and the dial that establishes the relay connection a
   circuit request needs — and BOTH name the relay rather than the
   destination — read from the two call sites, and pinned from the other
   side by R5.11, which requires that no behaviour-made dial targeted
   the destination (R5.10 prints the targets and is a note, so it cannot
   fail). Both are exchanges *with* the relay and must carry a
   reachability origin. Label the second `RelayCircuit` and, now that
   the origin names an application destination, every circuit through an
   infrastructure-only relay is refused at its set-up dial — relaying
   broken for exactly the peers it exists to reach.
   **The gate records its own refusals here**: the Swarm discards the
   denial of a behaviour dial, so a refusal that is not written down
   at the hook is written down nowhere;
2. **resolve D1, D2 and D3 — DONE: D1 and D2 on 2026-09-04, D3 on
   2026-09-05.** `DcutrHolePunch` was admitted for a
   `ConnectivityInfrastructureOnly` peer, which
   `transport/libp2p/CONNECTIVITY.md` §4's matrix forbids unqualified
   ("DCUtR as destination peer | no"); `RelayCircuit` was admitted for
   that same destination, which §4 now forbids by a row of its own; and
   `PreAuthAdmission` bucketed a relayed inbound by the source PeerId
   the circuit carries, which `contracts/CONNECTIVITY.md` §10 forbids by
   name. All three were code fixes. **D2 was a document conflict
   first**: §4's matrix had no row for a circuit whose DESTINATION is
   the infrastructure-only peer, and §11 excluded only
   `direct-user-command` and `kademlia-query`, so an accepted document
   arguably permitted the behaviour. ADR-0036's Amendment 2026-09-03
   added the row and both sections inherited it (CLAUDE.md §2), which
   left D2 a code change — and it separated the two relay origins rather
   than moving both, because `RelayReservation` must stay outside the
   predicate or every relay the stack needs is refused. D1 and D2 moved
   `DcutrHolePunch` and `RelayCircuit` into
   `names_application_destination` (renamed from `is_data_plane` in the
   same commit); D3 charges a relayed inbound to the relay rather than
   to the source PeerId the circuit asserts. All three landed before
   DCUtR or relayed paths are built, not after, and the spike harness
   reports zero divergences.

   **Two design items sat under this step, and step 2 settled both.**
   The first was the exit-gate test's shape.
   `tests/transport-contract/tests/stage2_exit_gate.rs` carried a test
   called `an_infrastructure_peer_cannot_reach_the_data_plane_by_any_origin`,
   and it derived both of its loops from the predicate under test — so
   it passed for ANY definition of that predicate and could not catch
   the misclassification the name it then carried claimed. **Fixed
   2026-09-03**: it asserts an explicit origin/outcome table with a
   length check against `DialOrigin::ALL`, and is now called
   `an_infrastructure_peer_reaches_the_data_plane_only_where_this_table_says`,
   which is what a table-driven test can honestly claim. Two rows were
   pinned wrong on purpose — the D1 and D2 admissions — so the table
   was one of the places step 2 had to change, and **on 2026-09-04 step
   2 flipped both to `false`**.

   The second was **the predicate's NAME being the defect's root**:
   `is_data_plane` described traffic, while the rule it decides is
   ADR-0036's WITH/FOR question — does this origin name the peer as an
   application DESTINATION. SPIKE-004 misread it in precisely that gap,
   twice. **Renamed to `names_application_destination` on 2026-09-04**,
   in the same commit that moved `RelayCircuit` and `DcutrHolePunch`
   into it. `KademliaQuery` stays in the set although routing is not
   application data, and **that was always an observation about the
   NAME rather than a reason to move the origin**: it stays refused for
   an infrastructure-only peer whatever the predicate is called —
   ADR-0036's matrix row, `CONNECTIVITY.md` §4 and §11 and the digest
   all say so, and letting it out would widen the infrastructure set,
   which is the exact failure ADR-0036 exists to prevent. The rename
   changed what the code SAYS and moved no origin on its own; the two
   origins that moved did so on the matrix's authority, not the name's.

   **A third item was raised here and DISPROVED; it is recorded so it
   is not raised again.** The concern was that the reconnect scheduler
   re-dials an infrastructure-only peer under a refused origin
   forever — `learn_address` admits such a peer's addresses to the
   book, `record_failure` schedules a retry with no class filter, and
   the tick dials under `DialOrigin::ConnectionManager`, which `admit`
   refuses `NotAuthorizedForDataPlane`. The first three are true and
   the conclusion does not follow. `refusal_settles_the_peer` counts
   `NotAuthorizedForDataPlane`, so `retry_claim` returns `Cleared` and
   `clear_retry_claim` REMOVES the retry entry outright rather than
   rescheduling it; `authorization_failures_clear_the_claim` pins that,
   and its own note says why — "re-offering it every tick is a busy
   loop against a decision that will not change on its own". So the
   gate enforces `CONNECTIVITY.md` §11's rule and the scheduler does
   not fight it. What remains is one refused dial and one diagnostic
   per underlying failure, after which the peer leaves the retry table.
   **That is not a divergence and needs no fix**; whether the scheduler
   should skip offering a non-`DataPlaneTrusted` peer even once is a
   cheap tidy-up, not stage work;
3. AutoNAT v2 client — **two of `AUTONAT.md` §4's three client bounds
   had no mechanism in the pinned crate, and ADR-0051's bounds table is
   where that is recorded.** `max_candidate_addresses_per_cycle` maps to
   `Config::with_max_candidates`, whose default is 10 against a
   configured 4, so it must be set rather than inherited. The in-flight
   ceiling is hard-coded at ten per connection with no field and no
   setter, and it is the CRATE'S: `docs/architecture/resource-limits.md`
   now says so, and the `ReachabilityManager` holds no in-flight
   accounting — an earlier version of this paragraph made it the
   manager's "to hold by how many addresses it re-tests", which was the
   probe-planning half PR #84 removed. The per-request timeout is
   hard-coded at 10s against the 15s once configured — stricter, so no
   bound broke, but the configured number described nothing.
   **`max_inflight_probes` and `timeout` were therefore owed a removal
   from `AutonatClientConfig`, `config.schema.yaml` and
   `examples/connectivity-infrastructure.yaml`, and this step's first
   PR (#84) removed them**, together with the four restatements across
   three documents that presented them as defaults — §4's two client
   rows, `CONNECTIVITY.md` §22's and `resource-limits.md`'s client row.
   FOUR SERVER LOOK-ALIKES had to survive, across four documents and
   the code — `AUTONAT.md` §7, `CONNECTIVITY.md` §6, the schema and the
   example profile — and only the code one was mechanically held. Step 4 then removed
   all four (#93): the server's `timeout` named nothing — the pinned
   server bounds request and dial-back at 10 s in code (`AUTONAT.md`
   §7's note) — so `AutonatServerConfig::timeout_ms` is gone and
   `check_ranges`' fixed-length row table reads 24 rows (28 before step
   3, 25 after it), its test mirror the same. The other three were not
   held. The schema and example keys were
   the sharpest — byte-identical to the client's lines above them in
   both files, and the field's serde default means deleting both leaves
   every check green; `shipped_examples.rs` catches only the reverse
   loss. `AUTONAT.md` §7's and `CONNECTIVITY.md` §6's rows are prose.
   Disambiguate by the struct, the section, or the
   `autonat.client`/`autonat.server` block, never by the string. Two
   earlier versions of this said the code was the unguarded one and then
   that the schema was the guarded one; both were backwards. Leaving THE
   CLIENT KEYS would have been the "config the schema documents but
   nothing read" defect this repository has already shipped once.
   **What the step's second PR built and proved (2026-09-17), and what
   it did not.** `ReachabilityManager` has its adapter:
   `SubstrateConfig.autonat_client` (default `None`, the 2026-09-07
   ruling) builds `Toggle<ScopedCandidates<client::Behaviour>>` with
   `with_max_candidates` only; `autonat_driver` folds outcomes into the
   manager, counts refusals under §9, schedules `retest` (refresh,
   second observer, retry under the gate's backoff), dials static
   servers under `AutonatProbe` (route 2), offers a server only when
   this profile dialled it and its Identify carries the protocol, and
   advertises and withdraws external addresses from the verdict alone;
   `dialing.rs`'s inbound arm retains a known server's inbound under
   `AutonatProbe` (route 3). PROVED over real sockets
   (`tests/connectivity/tests/autonat_client.rs`): route 2 admitted and
   announced with nobody asking; route 3's retained inbound offered
   exactly Identify and the dial-back protocol; an infrastructure-only
   bystander still established-then-closed. PROVED over a real Swarm
   with a constructible outcome (`autonat_driver.rs`): two distinct
   servers verify and the address is advertised, a lapse withdraws it,
   a stranger's report is refused by name. NOT PROVED, and not provable
   on loopback since §6 refuses the candidate: a real probe and a real
   dial-back THROUGH THE SUBSTRATE, and so `verified_public` from the
   wire — SPIKE-004 phase B's, with the rest of that matrix. **"Constructible" was the gap
   (2026-09-18):** the only outcome a test could construct was a
   success, and the failure classifier matched an error text the
   crate's public event never carries, so every real failure was "no
   outcome" and no failure vote was ever recorded. Found by the first
   outcome produced over the wire, in step 4's harness; fixed with a
   crate-level two-Swarm test (`tests/autonat_outcome_wire.rs`) that
   feeds the classifier a failure a real server made — which is also
   ADR-0051 Decision 3's owed test, at crate level. Route 3 is keyed on "is a server"
   (the owner, 2026-09-17), not on a probe window; a network change is
   seen only as a change of the bound listener set — the adapter's own
   comparison until step 10 made it the runtime's — and an OS network
   monitor is Phase 7's to bind;
4. AutoNAT v2 server role — including `AUTONAT.md` §7's dial-back
   restriction, which the crate does not implement, at the PENDING hook
   because the established one runs after the target is contacted. The
   crate-level half of ADR-0051 Decision 3's owed two-Swarm test is
   already written (`crates/transport/libp2p/tests/autonat_outcome_wire.rs`,
   PR #90): this step cites it and does not re-own it; the
   substrate-level half stays SPIKE-004 phase B's.
   **What the step built and proved (2026-09-18), and what it did
   not.** `SubstrateConfig.autonat_server` (default `None`, the
   2026-09-07 ruling) builds the vendored server under three wrappers:
   `ProbeServer` — §7's target rule (literal IP, equal to the observed
   source of the request's own connection, the same address-class rule
   as the client's candidates) at its pending hook, and the three
   budgets at the request's dial command, before the crate issues the
   dial (not before it parses the request: a flood's request cost is
   the crate's per-connection cap's to bound, and §7 says so); `Attributing`
   with `always(AutonatProbe)`, so the dial-back is CLAUDE.md §1's
   route 1, reached for the first time; `ClassGated` for the
   infrastructure service, so the dial-request protocol is offered to
   both authorized classes and to nobody else. With the server on, the
   inbound arm retains every authorized inbound under `AutonatProbe`
   (route 3 widened). The field sits after the gates like every
   dialling behaviour, because a later pending-hook denial no longer
   strands the gate's ticket (PR #91, found designing this step).
   PROVED over real sockets (`tests/connectivity/tests/
   autonat_server.rs`): an infrastructure-only client's inbound
   retained and offered exactly Identify and the dial-request protocol;
   its loopback target refused by name before any socket, the client
   hearing a probe failure, no connection reaching it, and the gate
   counting the refused dial-back as a release; a peer in no trust set
   established-then-closed; the same client closed at establishment
   with the server off. PROVED at crate level
   (`tests/autonat_server_wire.rs`, against `autonat_outcome_wire.rs`
   as the control that a dial-back is seen when made): a target refusal
   and a budget refusal reach the client as two classes, and a refused
   request is never also reported as served — the crate's own report
   says how the exchange went, not the dial, and read raw it called a
   refused dial-back served. NOT PROVED, and not provable on loopback:
   a dial-back MADE through the substrate, since §7 refuses every
   loopback target — SPIKE-004 phase B's, with the source-equality and
   unrelated-public-IP cases that need a second interface (R4.12's
   scoping). Two things the step found and recorded: the profile's
   server `timeout` reached no mechanism and was removed (§7's note);
   and the rate windows bound nothing before the driver's first tick
   until the pruning was fixed. Design questions parked for step 5+:
   the release counter in `DialRefusals` is global and wants keying by
   origin once relay reservations also reach `take_placeholder`; and a
   retained client inbound lives to the idle timeout after its probe,
   which §7's budgets do not count;
5. Circuit Relay v2 client reservations — **in two halves, like step
   3.** The first (2026-09-18) is the policy: `ReservationManager` in
   `crates/transport/runtime`, pure and enumerable — targets from the
   direct-inbound verdict capped by the maximum and the population,
   static before learned, a per-relay ladder with the attempt count
   carried through a re-ask, addresses advertised only while active,
   a stranger's or an unasked report refused by name, every list
   bounded (`RELAY.md` §4's note of 2026-09-18). The second (2026-09-18,
   the same day) is the libp2p adapter, `runtime/relay_driver.rs`: the
   relay client TRANSPORT composed into the Swarm's stack by
   `with_relay_client` only when `SubstrateConfig.relay_client` is
   `Some` (the builder branches on the switch, so a default profile
   composes no relay transport), the client behaviour under
   `Attributing` with `always(RelayReservation)` (the reservation's
   control dial is a behaviour dial — SPIKE-004 R2/R6, route 1) and
   under `ClassGated` for the infrastructure service (the stop
   protocol offered to the two authorized classes only), the crate's
   own `ExternalAddrConfirmed` swallowed by `ReservationScope` so the
   Swarm advertises the manager's set; the driver listens on
   `<relay>/p2p/<id>/p2p-circuit` per `Action::Reserve`, rotating a
   relay's addresses across asks, folds every `NewListenAddr` into
   `record_accepted` and every `ListenerClosed` into `record_failed`
   except the close of a listener it removed itself, follows the
   AutoNAT verdict in the same turn it is produced, learns relays from
   Identify under the opt-in, and forgets a learned relay on the trust
   change that de-authorized it. **What the wire test proved**
   (`tests/connectivity/tests/relay_client.rs`, against a bare relay
   server with an external address — SPIKE-004's note 10): the static
   relay dialled under `RelayReservation` and admitted toward an
   infrastructure-only peer; the address the relay reported advertised,
   and the relay itself recording the acceptance on the one connection
   the subject dialled, offered Identify and the stop protocol and
   nothing else; a static relay in no trust set refused by the gate
   under the same origin and never reaching a socket, the manager
   recording the failed ask and one-of-two held reported `Partial`; the
   loss of the relay's connection withdrawing the address within a
   second of the close (R10.10's bound) and the relay asked and
   accepted again after its backoff; under the opt-in, a trusted peer
   advertising the hop protocol learned and reserved on over its
   existing connection — the relay sees one connection, not two — and
   forgotten with its address on de-authorization, a trusted peer
   without the protocol never a relay. **What it did not prove**: a
   circuit through the reservation (step 7); a renewal (the crate's
   default lifetime is an hour); a relay with several external
   addresses (loopback binds one), so the bounded list is proved by
   the manager's tests alone; and the reservation target following a
   REAL verdict, since no AutoNAT client runs in the test — the
   verdict feed is exercised by the manager's tests and read, not
   measured, in the runtime. **Two things the round-1 review of PR #96
   found and the adapter now handles**: an ask that nothing answers —
   a relay de-authorized between its dial and its establishment is
   denied and hidden by `ClassGated`, and the client never closes the
   listener — ends at `REQUEST_HORIZON_MS` (a handshake plus the
   crate's reserve timeout) or at the trust change itself; and every
   event of a listener the driver removed is the driver's until its
   close, so a relay's second address queued behind a release never
   reaches the consumer as an ordinary listener. **One thing left open
   for step 7 and settled there**: the client's ask extends its
   addresses through the other behaviours, so a `/p2p-circuit`
   address of the relay held by Identify's cache or the Kademlia table
   would be dialled through the relay transport under
   `RelayReservation` — and a relayed connection to the relay retained
   under that origin is the row ADR-0036's amendment forbids for
   `RelayCircuit`. Step 7 settled it at the ESTABLISHED hook rather
   than the pending one: the path decides the origin the retention is
   asked under, so such a connection is refused when it comes up,
   whatever address list produced it (the driver's
   `a_relayed_outbound_is_judged_under_relay_circuit_whatever_dialled_it`;
   no two-relay wire test, since the pinned crate's address extension
   is not reproducible on demand). And a `Release` leaves the
   reservation alive on the relay until the next renewal (RELAY.md
   §5's note);
6. Relay server role — **`relay::Config::default()` is not `RELAY.md`
   §8**, in both directions (128 KiB and 120s per circuit against 64 MiB
   and 1h; reservation ceilings looser than §8's), `max_pending_control`
   has no field in the struct, and every per-peer ceiling admits one
   more than it says because the crate refuses on `>` rather than `>=`.
   **Built 2026-09-18** (`runtime/relay_server_driver.rs`): the crate's
   server under `ClassGated` for the infrastructure service (§8's
   service admission is the class gate: the hop protocol is offered to
   the two authorized classes and to nobody else), not `Attributing`
   (it dials nothing); every ceiling set from the profile, the per-peer
   ones handed over one below; `max_pending_control` removed from the
   profile block as the AutoNAT server's `timeout` was, since nothing
   honours it (RELAY.md §8's note names what bounds control work
   instead); the inbound arm retains every authorized inbound under
   `RelayReservation` when the server is on, the closure naming the
   origin; every crate event translated to `RelayServed`. **What the
   wire test proved** (`tests/connectivity/tests/relay_server.rs`): an
   infrastructure-only requester's reservation dial retained and
   offered Identify and the hop protocol and nothing else, its
   reservation accepted; the per-peer ceiling EXACT — the same PeerId
   on a second connection denied `ResourceLimitExceeded`, which the
   crate's own comparison admits, so the one-below hand-over is proved
   on the wire — and the global ceiling exact; a stranger closed at
   establishment and served nothing; a circuit request answered at
   the relay as a circuit event naming both ends. **What it did not
   prove**: a USABLE reservation and a circuit carrying bytes — a
   reservation's addresses are this profile's verified external
   addresses, which loopback cannot produce (§6 refuses a loopback
   candidate), so each client's listener closed with
   `NoAddressesInReservation` after the acceptance and the circuit
   failed at its far end; SPIKE-004 phase B is where a reservation
   carries an address, and step 7 is where a circuit is a path;
7. relayed inbound/outbound peer paths — **built 2026-09-18**, in the
   runtime rather than a new crate: a peer's path is read from the
   endpoint at establishment (`OpenConnection.path`), and the
   consumer's `Connected { peer, path }`, `PeerPathChanged` and
   `Disconnected` are derived per LOGICAL peer from the open set
   (`dialing::path_events`) rather than from the Swarm's
   per-connection events, which is `contracts/CONNECTIVITY.md` §5's
   "no second `PeerConnected`" — the Swarm reports a second
   `ConnectionEstablished` for a peer already connected, and the
   derivation is what absorbs it. A `/p2p-circuit` address is judged
   under `RelayCircuit` from every caller that dials it — `Dial`,
   `DialPeer` and the retry scheduler, through `dialing::origin_for`;
   the scheduler's half was PR #101 round 1's finding, a learned
   circuit route scrubbed by its own retry under the pairing check —
   and a connection that came up over a circuit,
   in either direction, is retained only for a data-plane far end
   (`retention_origin`; ADR-0036's amendment). The relay-derived
   address set is decided here too: the relay SERVER is told only
   the direct external addresses (`ServedAddresses`), so a dual-role
   profile hands its clients no circuit through a circuit — RELAY.md
   §8's step-6 note — measured on the wire in `relay_server.rs`
   against an upstream relay. **What the wire test proved** (`tests/connectivity/tests/relayed_paths.rs`, two
   production runtimes across a bare relay with an external address):
   a circuit to an infrastructure-only far end refused at the gate
   before any socket — the relay never saw the dialer — and to a
   data-plane peer admitted, the relay accepting the circuit and each
   end announcing the other ONCE with `Relayed`; a direct v2 message
   accepted over the circuit and drained at the endpoint it named; a
   direct connection joining the relayed one reported at both ends as
   `PeerPathChanged { relayed -> direct, DirectEstablished }` and not
   a second `Connected`; and an infrastructure-only SOURCE over a
   circuit established, refused and closed at a destination serving
   probes and circuits — never announced — where the same source
   with data-plane trust is retained; and a circuit route the relay
   denies retried by the scheduler as a relay circuit that reaches the
   relay again, the route kept. **What it did not prove**: the
   downgrade when the last direct connection closes with a circuit
   remaining (nothing closes one connection of a pair on demand; the
   derivation's unit test carries `DirectLost`); that the origin is
   `RelayCircuit` and not `Manual`, which the gate cannot tell apart
   on the wire since both are application origins (the unit test
   pins it); §5's stability gate before a direct path counts as
   preferred — step 7 announces the change the moment the set
   changes, and the interval is step 9's, as is `reason: dcutr`
   (step 8 supplies the punch); a circuit's byte and duration limits
   at the relay (the bare relay's defaults, not `RELAY.md` §8's); and
   any NAT, every address being loopback. **Still open after step 7**,
   for the step that composes connection lifetime: a `Release` leaves
   the reservation alive on the relay and its control connection open
   until the next renewal (RELAY.md §4's note) — closing an
   infrastructure-only relay's connection on release is a decision
   about the AutoNAT adapter's connection to the same peer too; and a
   relay AT its reservation ceiling sheds its clients' renewals
   (RELAY.md §8's note), which no wrapper can correct since the crate
   answers before a wrapper sees the request — a vendored patch under
   ADR-0051 is the shape of a fix, if one is wanted;
8. DCUtR — **the crate has no knobs**, so §13's four-concurrent,
   one-per-peer and five-minute cooldown must be built here. **They do
   not belong to the dial gate alone.** The gate sees independent dials
   and is handed no attempt outcome: the spike measured that one punch
   produces a dial at BOTH ends, so no single gate sees the attempt —
   only its own half — and the outcome reaches the DCUtR behaviour
   rather than the gate. So the ATTEMPT
   lifecycle is the DCUtR adapter's to track — open, candidates,
   outcome — and what reaches the gate is a token for that attempt,
   which the gate admits or refuses as a unit. (Candidate multiplicity
   is a further reason to expect the same, and is NOT measured: on
   loopback each endpoint dialled once.) **Built 2026-09-18**
   (`hole_punch.rs`, `runtime/dcutr_driver.rs`): the crate under
   `HolePunchScope`, under `Attributing` with `always(DcutrHolePunch)`
   and the DATA-PLANE class gate — so a non-data-plane peer is offered
   no DCUtR handler and no attempt begins toward it (§2, D1 at the
   handler beside the gate) — constructed only when
   `SubstrateConfig.dcutr` is `Some`, `None` by default. The attempt
   BEGINS at a relayed connection's establishment, where §13's
   eligibility is decided (no direct connection, no cooldown, one per
   peer, four in all; a relayed connection that fails it gets a
   protocol-less handler, so the far end's CONNECT finds nothing) and
   ENDS on the crate's outcome, on any direct connection to the peer
   coming up — the crate reports a success for its OWN dial only, and
   the initiating end of a punch the responder's dial completed
   otherwise retried its stalled dial to the crate's ceiling and
   reported the attempt failed beside a working path, measured — on
   the relayed connection closing (no cooldown), or at a ninety-second
   horizon, since the crate tells the responding end nothing of a
   failed punch. A punch dial's failure reaches the crate only while
   its attempt is in flight, so a finished attempt gets no further
   CONNECT round (measured: without the filter the retries reach the
   gate and are refused for the peer's backoff). Bound listeners are
   offered to the crate as candidates the moment they bind: the crate
   learns candidates only from what a peer's Identify observed, and a
   relay reached before this profile listened observed an ephemeral
   port. The runtime names the punched connection (`take_punched`)
   and announces `PeerPathChanged { relayed → direct, HolePunched }`
   for it. **The address-class boundary** (`DCUTR.md` §6, ADR-0052,
   architect-cto's decision on PR #102's round-1 risk): a punch
   candidate is a peer-supplied address this profile would connect to,
   so `is_punchable_address` — §7's boundary, with a private candidate
   admitted only beside a private listener of its family and no
   source-equality clause — is applied to what the wrapper learns,
   offers and dials, the last filtered at the pending hook before any
   socket (the crate's dial denied and the survivors reissued),
   `refused_by_class` naming the class when nothing survives. **What
   the wire test proved** (`tests/connectivity/tests/
   dcutr.rs`, two runtimes across a bare relay, both listening on the
   host's PRIVATE address — §6 refuses a loopback candidate whoever
   supplies it, so the punch is made over a private-range pair, and a
   host with no private interface runs neither punch-exchange test
   and says so): the circuit's establishment starting an attempt at each
   end, the circuit's listener initiating; every punch dial admitted
   with no refusal under `DcutrHolePunch`; the direct connection
   announced at both ends as the punch and not a second `Connected`;
   the initiator reporting the attempt succeeded and counting it, and
   no retry after the success; with DCUtR off at the responding end,
   the initiator's attempt failing (the CONNECT stream finds no
   protocol), the peer entering the cooldown, and its next circuit
   declined for it while the path stays relayed; a bare initiator's
   loopback candidate refused at the hook as `refused_by_class`, no
   connection at its listener, the gate's ticket taken back; a bare
   initiator's loopback candidate beside its private one removed and
   the punch made through the private one; a loopback-only subject
   sending no candidate at all; the offered listeners following the
   bound ones; an
   infrastructure-only source over a circuit starting no attempt at a
   destination with DCUtR on. **What it did not
   prove**: a punch that fails at the network (on one host every punch
   succeeds, SPIKE-004's limit), so the retry ceiling, the horizon and
   the concurrency ceilings rest on the wrapper's unit tests; the
   stability interval before a punched path counts as preferred (step
   9's; at step 8 `direct_stability_period` was carried in the
   settings and read by nothing); relay retirement after the upgrade
   (§13's last arrow, step 9's); and any NAT. **Two crate facts worth carrying**:
   the initiator's role-overridden connect landing on a listener
   stalls to the dial timeout and is the failure the crate would have
   retried, and `direct_to_relayed_connections` is never pruned at all
   — one entry per punch dial, success or failure, a leak in the
   pinned crate bounded by nothing but the attempt rate the wrapper
   imposes;
   a vendored patch under ADR-0051 is the shape of a fix if one is
   wanted. **Raised and settled the same evening**: a whole-list
   verdict at the hook composed with a private listener always offered
   would have refused a home-NAT node's CONNECT (its RFC 1918 listener
   beside its global mapping) whole at a global-only far end, every
   time — the topology DCUtR exists for; architect-cto chose the
   filter (ADR-0052 rule 5 as corrected), and the wrapper denies the
   crate's dial and reissues the survivors as its own, the backstop at
   the same hook; the composed case is measured on the wire with a
   bare initiator scripting a loopback candidate beside its private
   one;
9. direct-versus-relayed path preference/stability — §5's stability
   interval before an upgraded direct path counts as preferred, and
   §6's head-start before a relay route is raced. (§5's "no second
   `PeerConnected`", which the Swarm does NOT give — it reports a
   second `ConnectionEstablished` for the same peer when the punch
   succeeds, and the relayed connection survives beside it — was
   step 7's, above: the events are derived per logical peer from the
   open set, and step 9 only decides WHEN a change is announced.)
   **Built 2026-09-19.** The STABILITY GATE: the path derivation
   reads each open connection as a sample, and a punched direct one
   ranks below a relayed one until it has held for
   `direct_stability_period` (`DCUTR.md` §4's `direct_candidate`), so
   the relay stays the announced path and `PeerPathChanged {
   HolePunched }` comes from the runtime's tick once the interval has
   passed — or at the relayed connection's close if that comes first,
   the announced path always being a connection that exists; the
   DCUtR wrapper, measuring from the runtime's clock at establishment,
   counts a punched connection that closes sooner as a stability
   failure (`HolePunch { Unstable }`, the peer in cooldown) and one
   that holds as an upgrade. The RETIREMENT (§13's last arrow, §12's
   lost race): once a stable direct connection is the announced path
   — punched past its interval, or dialled, whose handshake is its
   evidence — the relayed connections to the peer are closed when no
   direct or directory exchange this profile started awaits its answer
   (`RelayedConnectionRetired`, once per connection); the reservation
   and the route stay. The rule reads "any stable direct" rather than
   "the punched one" because the review's bare far end retried its
   stalled punch dial after the attempt ended, and the second direct
   connection provided the path over the punched one — under the
   narrower rule the redundant circuit stayed open, and it does not
   idle out: request-response spreads a peer's streams over every
   connection to it. The HEAD-START (`transport/libp2p/CONNECTIVITY.md`
   §12): `DialPeer` reuses a healthy direct connection that CARRIES
   THE DATA PLANE (the relay's control connection is direct and
   infrastructure-only; the gate refuses it as before), dials the
   book's direct candidates first, and a circuit route only after the
   profile's `relay.client.direct_head_start` (750 ms) with no direct
   connection landed (`runtime/path_race.rs`), reporting a deferred
   circuit the gate refuses as a `DialFailed` since nobody holds a
   reply channel for it; a losing attempt is not cancelled, since the
   pinned Swarm cannot abandon a dial. **What the wire tests proved**
   (`dcutr.rs` over the private pair with a two-second interval,
   `path_race.rs` and `relayed_paths.rs` on loopback): after the punch
   the relay stays the announced path for the interval and the move
   comes no earlier than it, the relayed connection is then retired
   with the relay seeing its circuit close and the peer still
   connected; a retirement waits for a direct exchange in flight (a
   bare far end reading the request forever: no circuit closed until
   the subject's own timeout, then the retirement); a punched
   connection the far end closes within the interval leaves the relay
   preferred, the peer in cooldown, nothing retired; a dialled direct
   joining a relayed one retires the circuit; a live direct route wins
   with the circuit never dialled and a second ask dialling nothing; a
   black-holed direct route yields to the circuit no earlier than the
   head-start; a reserved relay asked for by `DialPeer` is refused
   `NotAuthorizedForDataPlane`; a deferred circuit refused after a
   revocation is reported. **What they did not prove**: cancelling the
   losing attempt (not available); the once-only retirement report
   under a slow close (a circuit closes within a tick on one host, so
   the flag's reading is the unit test's); that a peer's streams
   prefer the direct connection while two paths are open (§13 says
   they should; request-response picks by request id, and nothing here
   steers it — the retirement closes the window instead); and any
   NAT;
10. network-change invalidation/recovery. **Built 2026-09-19.** A
    network change is the RUNTIME's, not the AutoNAT client's: a
    change in the set of addresses the listeners have bound, compared
    without the interface-scoped ones after the first bind
    (`runtime/network_change.rs`), detected once at the listener event
    that changed it and reported; only a change that REMOVED an
    address invalidates (§14 item 1; an addition — a VPN, a wildcard
    listener's second address at startup — is offered and forgets
    nothing, the review's risk), and a removal is told to every
    subsystem in the same turn —
    with the client off too, which the client's own comparison (steps
    3 to 9) never covered. The AutoNAT adapter sends the verdict to
    `unknown`, publishes it (the relay target follows in the same
    turn), withdraws the advertised addresses, offers the listeners
    still bound again at once (not on the next tick, where a peer's
    claim about one would land as an observation) and returns every
    candidate to the sweep within one crate tick of JITTER
    (`NETWORK_CHANGE_JITTER_MS`, §14 item 6; step 3's `retest_all`
    re-tested at once); the DCUtR wrapper gives up every attempt in
    flight — it keeps its per-peer permit while the crate's rounds on
    the kept relayed connection run, and ends `Abandoned` with no
    cooldown whatever then reaches it (a landed punch excepted, which
    is `Succeeded`), since removed at once the
    crate's late outcome was charged to the peer's next attempt (the
    review's P2) — lifts every cooldown, and stops judging a punched
    connection in its interval; the runtime closes nothing
    (item 5) and holds no frame to replay (item 7); the consumer is
    told as `NetworkChanged { removed, added }`. **What the wire test
    proved** (`dcutr.rs`, over the host's private interface, the
    client OFF): a private listener going away is reported with the
    departed address named, the first network-scoped bind is not a
    change, a second listener joining is reported and lifts nothing,
    the cooldown a peer earned on the old network is lifted
    so its next circuit begins an attempt, and the reservation stands
    so the relay accepts that circuit; an attempt in flight at the
    change keeps its permit (`inflight` stays one) and ends `Abandoned`
    with no cooldown when the relayed connection closes. **What it did
    not prove**: the
    AutoNAT verdict moving to `unknown` on a change (loopback yields
    no evidence to invalidate; the adapter's reaction — unknown,
    published, withdrawn, re-test due within the jitter, failure
    count reset — is its unit test's, and the runtime's one-line call
    is not observable on this host); an OS-made interface change (it
    arrives as the same listener events a command raises; binding an
    OS monitor is the Android step's); a change that leaves the bound
    set intact (not seen, by design); and any NAT. The first commit
    closed the previous pull request's round-2 P3s: one clock read
    per loop iteration for the settlement and the punch stamp and for
    the wrapper's tick and the stability sample, the policy-owner
    bullet, and `validate`'s doc.

### Mandatory invariants

- all behavior-originated dials pass DialAdmissionGate;
- connectivity-infrastructure peers never gain GossipSub/direct/endpoint/Kademlia authority merely by being connected, and the protocol set such a connection is OFFERED is restricted at the connection rather than answered at the request. **MET by `ClassGated<B>`** (`crates/transport/libp2p/src/class_gate.rs`).

  **Read this as EXPOSURE, not only authority**, because that is what made it easy to believe already done. Stages 6-10 built each data-plane entry point to classify its caller — direct ingress, the GossipSub publisher check, `build_answer`'s trust check, and the Kademlia driver's `try_admit` — so authority was refused throughout, and an implementer checking only that would have found the invariant apparently met. What was not met is that `SubstrateBehaviour` installed `direct`, `broadcast`, `endpoints` and `kad` on every connection uniformly, so an infrastructure-only peer was advertised those protocols and could open their substreams, and a refusal cost a parse and an accounting charge rather than a closed stream. **Four protocols, not three** — `kad`'s authority check leaves such a peer no routing seat, but it could still open the DHT substream and be answered, and an implementer working from a list of three would have restricted three.

  `ClassGated<B>` wraps all four. A `DataPlaneTrusted` connection is unaffected; any other class gets a handler built on `DeniedUpgrade`, whose `protocol_info()` is empty, and the inner behaviour is not consulted at all. Gating and advertisement are one fact rather than two: `Connection::new` reads the handler's protocol set and Identify advertises exactly that, so declining to install a handler is declining to advertise.

  **Measured**, by retaining an infrastructure-only inbound and reading its Identify: seven advertised names before, and `/ipfs/id/1.0.0` plus `/ipfs/id/push/1.0.0` after — Identify alone, which is what `transport/libp2p/CONNECTIVITY.md`'s matrix grants that class. **Read off a local `authorizes_for` mutation, and not reproducible from the tree AT THAT CLOSURE**: nothing could retain such a connection then, which is why the assay in `tests/connectivity` was the trusted-peer control and the wrapper's own unit tests carried the negative. Since Stage 11 steps 3 to 5 a retained infrastructure-only connection exists and its exact protocol set is pinned on the wire — `autonat_client.rs` (Identify and the dial-back protocol), `autonat_server.rs` (Identify and the dial-request protocol), `relay_client.rs` (Identify and the relay stop protocol) — while `kad`'s gating rests on the class gate's unit tests, since none of those profiles configures Kademlia.

  **A gating change closes the connection, in whichever direction it moves.** A handler is chosen once at establishment and libp2p never rebuilds it, so a connection whose peer crosses the data-plane boundary carries the wrong protocol set from that moment. Losing the trust is decided by `connections_to_close`, so the closure lands in `set_trust`'s ADR-0012 count; gaining it is decided by `ClassGated::poll`, since a promotion is not a revocation and is not part of that count. Both are ADR-0036's own instruction — close and re-establish "rather than allowing a transient privilege mix" — and the gaining direction is not merely under-privileged, because a peer holding one `Denied` and one `Allowed` handler is a pair `NotifyHandler::Any` can route a `kad` query into, where it is silently dropped. **The comparison is against the class the connection was ADMITTED under**, recorded on `OpenConnection`, and not against `Revoked::was` — which is what keeps ADR-0036's origin/class separation deciding something: a connection admitted while the peer was infrastructure-only has carried a denying handler all along, so nothing is stale and its origin still says whether it survives. Separately, `sync_broadcast_admission` blacklists a downgraded peer from the mesh, which rejects its MESSAGES while leaving `/meshsub/` registered, so that call is authority and this wrapper is exposure and neither substitutes for the other.

  **Step 3 must not regress this.** It reaches an infrastructure-only connection two ways at once — it dials a static AutoNAT server under `AutonatProbe`, which is the `attempt_dial` route (route 2, not the wrapped-behaviour route 1: the client emits no dial, as the step-3 note above records), and it must relax the inbound arm so the client can serve `/libp2p/autonat/2/dial-back`. Both produce a RETAINED infrastructure-only connection, which is the state this invariant now governs and which nothing before step 3 could produce. CLAUDE.md §1 enumerates the three routes to such an origin; the ordering constraint they were written for is discharged, and what remains is that step 3 keep the restriction true rather than land before it.

- AutoNAT server dial-back candidate is literal IP, matches requester observed source IP, and rejects prohibited address classes;
- statically configured infrastructure is preferred for RELAY, and is what the profile guarantees to DIAL for AutoNAT (ADR-0035's Amendment 2026-09-09: the client can be given no server order, and it offers dial-request only on connections this profile opened, so a server that dialled us is not eligible); Identify-learned relay/probe promotion remains explicit opt-in for both;
- relayed pre-Noise accounting is charged to authenticated relay connection/PeerId plus global limits when original IP is unavailable;
- relayed destination trust is evaluated against the authenticated end PeerId, not the relay;
- a Relay v2 circuit or a DCUtR hole punch whose far end IS an
  infrastructure-only peer is refused, while a reservation with that
  peer and a circuit *through* it toward a trusted destination are
  admitted. A relay may carry a circuit without becoming a party a
  circuit may terminate at; who an exchange is WITH is a different
  question from who it is FOR (ADR-0036 Amendment 2026-09-03). **Met by
  step 2** (2026-09-04): `DialOrigin::names_application_destination`
  names both origins, closing D1 and D2. The REFUSED half is pinned by
  the contract test's table and by the spike harness at R3.5, R3.6 and
  R7.4; the ADMITTED half — a circuit *through* infrastructure toward a
  trusted destination — is pinned by
  `a_trusted_destination_is_admitted_under_every_origin`, which is why
  moving the origins costs the legitimate case nothing. Both halves are
  needed: either alone passes for a gate that refuses everything or one
  that refuses nothing;
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

**Phase 8 reconciled (2026-09-19).** The matrix above, and
`transport/libp2p/CONNECTIVITY.md` §25's twenty required integration
tests, against what the tree pins — each item either named to the test
that proves it or deferred with the reason. Every wire test runs
between real peers over real sockets on one host: loopback, or the
host's private-range interface where a punch needs one. `tests/
security` is Stage 18's adversarial gate; the security rows this stage
owes are pinned where named below — the AutoNAT §7 boundary and the
class gate in `tests/connectivity`; the direct pre-Noise rate and slot
accounting in `stage5_dial_admission.rs`
(`a_source_past_its_pre_auth_rate_is_refused_before_noise` and the slot
tests); the RELAYED pre-Noise bucket — a circuit charged to its relay,
the D3 fix — in `preauth_gate.rs`'s unit tests, with no wire test of
its own.

- **§14 rows.** `private → relay → private` and `→ public`:
  `relayed_paths.rs`, `dcutr.rs`, `path_race.rs`, `relay_failover.rs`
  (on one host "public" and "private" are both loopback; the
  distinction is phase B's). `multiple relay reservations` and
  `relay failure/failover`: `relay_failover.rs`. `AutoNAT abuse/SSRF`:
  `autonat_server.rs` (a loopback dial-back target refused before any
  socket) and `autonat_client.rs`, with the §7 rule's own unit tests.
  `DCUtR success/failure`, `network change`: `dcutr.rs`.
  `infrastructure peer protocol exclusion`: the retained sets in
  `autonat_client.rs`, `autonat_server.rs` and `relay_client.rs` (each
  an exact list), the default-profile set and the downgrade close in
  `advertised_protocol_set.rs`, and the class gate's unit tests for
  `kad`.
  `public ↔ public`: **deferred to phase B** — needs two public
  addresses.
- **§25, by number.** 1 — **phase B** (public reachability; the warm
  target's policy half is `relay.rs`'s
  `the_target_follows_the_verdict_and_is_capped_by_the_maximum_and_the_population`).
  2, 3 — `relay_failover.rs`. 4, 6 — `dcutr.rs`'s cooldown test sends
  a direct message over the circuit after the failed punch. 5 —
  `dcutr.rs`'s upgrade test (the path moves and the circuit is
  retired). 7 — unit: `reachability.rs`'s
  `verified_needs_distinct_servers_and_one_server_twice_is_one_observer`
  and `verified_lapses_at_the_evidence_ttl_without_refresh`; **no
  evidence exists on loopback**, so the wire cannot show it (phase B).
  8 — unit: `autonat_driver.rs`'s
  `a_changed_listener_set_forgets_every_observation_and_retests_every_candidate`
  and `reachability.rs`'s `network_change_resets_to_unknown_and_clears_everything`.
  9 — the retained infrastructure-only connection's exact protocol
  set on the wire: `autonat_client.rs`, `autonat_server.rs`,
  `relay_client.rs` (Identify plus the one control protocol each, no
  data-plane protocol), `kad` by the class gate's unit tests, and
  `relayed_paths.rs` for the source refused over a circuit. 10 —
  `relayed_paths.rs` refuses an infrastructure-only SOURCE over an
  authorized relay; an unauthorized one is refused by the retention
  predicate the relayed inbound is judged by, `authorizes_for`, whose
  unit test `an_unauthorized_peer_keeps_nothing_under_any_origin`
  pins the `Unauthorized` arm — not by a wire test of its own. 11 —
  `relay_client.rs` (withdrawal within a second of the loss). 12 —
  `path_race.rs` (a deferred circuit's refusal reported), with the
  split by path pinned by `dialing.rs`'s
  `the_books_classification_keeps_the_callers_origin_off_a_circuit` and
  `path_race.rs`'s own unit test, the recorded origin of a relayed
  outbound by `a_relayed_outbound_is_judged_under_relay_circuit_whatever_dialled_it`,
  and the race's own `RelayCircuit` constant by the wire test through
  the gate's origin/address pairing rather than by a unit test;
  the root limits apply because
  every race dial passes `attempt_dial`, whose ceilings
  `stage5_dial_admission.rs` pins — by composition, no single test.
  13 — unit: the adapter's `network_changed` (verdict to unknown,
  published) and `relay.rs`'s target-follows-the-verdict test; on the
  wire `dcutr.rs`'s network-change test with the client OFF; the
  raised target is **not observed on the wire** (no evidence to
  invalidate on loopback), and the PeerId is the profile's by
  construction. 14 — `relay_server.rs` (exact ceilings). 15 — by
  composition: a bootstrap entry grants no trust (Stage 9's exit
  gate) and an infrastructure-only peer is offered no `kad` protocol
  (item 9's class-gate tests); no single test names the co-location. 16 —
  `relayed_paths.rs` resolves the default endpoint over a circuit as
  `tests/direct-v2` does over a direct connection. 17 —
  `relayed_paths.rs`'s broadcast test (delivered over the circuit,
  the authenticated publisher the dialer); that the relay is nobody's
  mesh peer holds there by the fixture — a bare relay speaks no
  GossipSub — and for an InterWeave relay by `relay_client.rs`'s
  pinned set on the reservation connection, which offers no
  `/meshsub/`. 18 — `relay_failover.rs` (a dialer with no path left is
  answered `PeerUnreachable` at once); the mid-exchange case is the
  crate's request-response failure, pinned only as a timeout by
  `dcutr.rs`'s retirement test. 19 — `advertised_protocol_set.rs`
  (a downgrade closes the connection) and the class gate's unit test
  for the promotion. 20 — `relayed_paths.rs` (the refused circuit and
  the admitted one through the relay), `dcutr.rs` (no attempt toward
  an infrastructure-only source), `relay_client.rs` (the reservation
  with an infrastructure-only relay admitted).
- **Deferred to SPIKE-004 phase B, all for one reason** — no public
  address and no NAT on this host: `public ↔ public`, item 1, the
  evidence halves of items 7 and 13, and every row of the exit gate's
  NAT matrix. The stage cannot close on loopback evidence, and this
  record does not claim it can.

### Exit gate

The mandatory standard-v1 NAT/relay/hole-punch matrix passes. At this point the low-level network engine is complete.

Flip to `active`: `contracts/schemas/connectivity` (ADR-0049).

## 15. Stage 12 — full TransportRuntime composition

### Objective

Combine the already-tested components behind neutral APIs.

### Precondition

**The DNS transport is built into the Swarm — the `dns` feature on the
list AND the builder wrapping the base transport in it — before this
stage composes a profile that names a DNS host (recorded 2026-09-19;
the construction clause added the same day, below).** Six of the ten
shipped examples under `architecture/config/examples/` name `/dns4`
hosts — `composite-discovery`, `human-android`, `human-desktop`,
`internet-reachability`, `kademlia-enabled`, `remote-bootstrap` — for
infrastructure, relays, Kademlia seeds and bootstrap peers, because
names are the design; four name none. The substrate is built `with_tcp`
alone (plus the relay client's transport when one is configured),
neither of which resolves a name, so such an address fails
`MultiaddrNotSupported`, is classified structural and is forgotten
rather than retried — Stage 11's `dns` obligation (§14) owns the gap,
and `discovery/providers/static-bootstrap.md` §DNS ownership records it
together with the rule that holds meanwhile: profile validation
refuses a `dns4`/`dns6` host in a build without `dns`, the way it
refuses an enabled provider the build omits — recorded 2026-09-19 as a
decision the code had yet to conform to, and conformed to the same day
on the same pull request (`profile-config`'s `AddressHostNotBuilt`,
judged wherever a peers list appears). So this precondition is
mechanical, not remembered — a profile this stage may compose is one
that validates in the build that composes it. Nothing dials a configured
name before this stage, which is why the gap is live only for learned
addresses today; composition is what would turn it into a configured
bootstrap peer discarded on first use. The feature was blocked by the
same dependency line as `mdns`; the owner ordered the `libp2p 0.57` bump
that clears it on 2026-09-19 and it has been taken. The bump cleared the
advisories, not the feature: enabling `dns` is a transport change with
no stage owner, one decision away — and this precondition makes it the
entry decision for a Stage 12 that composes the six, taken and landed
before they are composed. **The feature flag is not the transport, and the
obligation that proves construction lives here (2026-09-19).** Enabling
libp2p's `dns` feature only makes the transport available; the Swarm
builder must wrap the base transport in it, and today it builds
`with_tcp` and the relay client's transport and nothing else. A `/dns4`
dial in a build with the feature on and the builder untouched still
fails `MultiaddrNotSupported` and is forgotten. `tools/checks/
check_dialable_hosts.sh` pairs the root manifest's feature array with
`profile-config`'s dialable host set, so the refusal cannot be lifted by
the flag alone — and a grep cannot verify construction (five review
rounds on PR #108 measured seven ways a text search was wrong about
whether a transport was built, and the attempt was removed). So the
change that lifts the refusal carries **a test that builds the real
transport and asserts the error kind of a `/dns4` dial** —
`MultiaddrNotSupported` while the transport is unbuilt, another kind
once it is built — which needs the Swarm builder factored out of
`SubstrateRuntime`; that factoring is part of the same change, owned by
p2p-network-dev, not a spike (nothing is unknown, a type-level fact
needs a type-level proof). Until that test exists the refusal stays and
this precondition is not met, whatever the feature list says. One that composes only the four that name no DNS
host needs no `dns` and says so in its record (two of those four,
`connectivity-infrastructure` and `local-lan`, are refused today for an
omitted provider, independently of this).

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

### Dependency hygiene for the Gradle build

The Android client brings the repository's first non-Cargo dependency graph — the Kotlin/JVM dependencies of `apps/human-android` and the instrumented tests — and none of the workspace's checks see it: `cargo-deny` resolves the Cargo graph against RustSec, Dependabot's Cargo coverage stops at `Cargo.lock`, and `check_vendored_advisories.sh` reads `third_party/`. So this stage adds the Gradle graph's own software-composition analysis, in the same change that adds the Gradle build (the owner, 2026-09-19):

- **OWASP Dependency-Check**, as the `org.owasp.dependencycheck` Gradle plugin, its version pinned in the version catalog beside every other plugin, its `dependencyCheckAnalyze` task a CI job on `pull_request`, `merge_group` and pushes to `main`, its `name:` added to `CLAUDE.md` §9's list of contexts in the same change — `check_required_contexts.sh` holds that list to the workflow — and to the ruleset by hand, as §9 says it must be; a job that reports nothing gates nothing;
- it resolves against the **NVD**, which needs an API key for a CI-rate run: the key is a repository secret, never a committed file, and a run without one is a slow run, not a skipped one;
- **suppressions are reviewed exemptions**, not a way past the check: one file, one entry per accepted finding with the CVE, the artefact, and a sentence saying why it does not apply here — the models are `deny.toml`'s `[advisories].ignore` entries (id, reason, what would change the answer) and `tools/checks/license_exempt.txt`, and an entry without its sentence is what a reviewer refuses.

Why here and not for the Rust workspace: Dependency-Check matches by CPE against the NVD, which names Rust crates thinly and noisily — most RustSec advisories carry no CVE, and a crate name shared with an unrelated product is a false positive to suppress by hand — while RustSec plus Dependabot's GHSA view already cover the Cargo graph (§8 of `CLAUDE.md` records the one live gap, `yamux`, and the guard for it). Running it over `Cargo.lock` would add suppressions, not findings. CI wiring and the pin are devex-tooling's to land; the dependency policy — what is allowed and why — is decided with the network lane, as `deny.toml` is (`.agent-fabric/roles/`).

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
