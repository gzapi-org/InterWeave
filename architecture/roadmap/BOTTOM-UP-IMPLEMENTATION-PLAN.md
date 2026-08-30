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
| SPIKE-004 | Stage 11 mandatory connectivity | AutoNAT v2, Relay v2, DCUtR, infrastructure class, dial admission, deployment/NAT matrix |
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
  are written once over `&mut dyn DiscoveryProvider` and applied to all
  three — `every_provider_passes_the_shared_suite`. That alone would be
  worth little: a generic suite passes for a stub. So the crate also
  carries a `MisbehavingProvider` that emits before start, ignores the
  batch bound and keeps emitting after shutdown, and asserts the suite
  CATCHES each — `the_suite_catches_a_provider_that_emits_before_start`,
  `…that_ignores_the_batch_bound`, `…that_emits_after_shutdown`. The
  suite's own mutation check is part of the suite.
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

- **The mDNS multicast MECHANISM was not built, and `mdns` is not on the
  libp2p feature list.** Enabling it pulls `libp2p-mdns 0.48`, which pins
  `hickory-proto 0.25.x`, carrying RUSTSEC-2026-0118 and
  RUSTSEC-2026-0119 with no upgrade available inside that line —
  `check_dependencies.sh` fails, and §8 makes that a gate rather than a
  warning. So `crates/discovery/mdns` ships its **normalization half
  only**: PeerId grammar, address bounds, dedup, expiry and the degraded
  report, driven by pushed observations rather than by a socket. The
  degraded arm is real (`a_quarantined_cache_reports_degraded_at_start`'s
  sibling, `aggregate_health_survives_one_degraded_provider`, drives
  `report_backend_down`); the discovering arm has never seen a multicast
  packet. **The exit gate's "mDNS provider composes correctly" is met for
  the provider and NOT for LAN discovery**, and anything that reads this
  stage as having proved LAN discovery is reading it wrong.
- **The manager is a library, composed in tests.** There is no
  `SwarmRuntime` task driving it and no production holder; plan §15 is
  where TransportRuntime constructs one. The `stage-12` entries in
  `tools/checks/domain_fn_exempt.txt` are that gap written down, and they
  were re-dated at this closure because their previous reason — "the
  conformance suite composes the manager" — was wrong about what counts:
  that check strips `#[cfg(test)]` and excludes `tests/` wholesale.
- **`protocol_observations` stay empty**, per the Stage 10 deferral at
  L967-991. `PeerCacheDiscovery` does not fill them and says so at its
  source.

## 13. Stage 10 — Kademlia

### Prerequisite

**SPIKE-003 ran and closed on 2026-08-30: PASS FOR THIS STAGE.** It does
**not** close ADR-0034's v1 release gate — two required evidence items
are unmet, and both need infrastructure the spike does not have:
server-mode reachability evidence is not consumed (AutoNAT and Relay are
absent from the libp2p feature list — SPIKE-004), and single-path capture
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
> at the first hop it lacks a connection for, silently, because a refused
> behaviour dial surfaces as an ordinary dial failure. Extend the gate
> first; the spike's `PolicyAdmit` mode is a measured proposal, not
> production code.

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
are absent from the libp2p feature list, so the spike could not consume
the AutoNAT-verified-or-relay-reservation rule this stage's §14 requires.
SPIKE-004 is where that arrives. Do not treat it as proved.

**Decide the capability-observation mapping in the architecture before
writing code.** `PeerCache::candidates` exports `protocol_observations`
**empty**, and that is a deferral — not a statement that a peer has no
observations. `providers/peer-cache.md` has the Kademlia provider read
fresh capability evidence "through normal candidate/hint data", which is
that field, and the "targeted lookup only with locally computable fresh
server-capability evidence" item below is the consumer that needs it.

The mapping is not specified anywhere, which is why the code declined to
guess one: a stored observation is `(protocol_family, wire_major,
network_hash, role)`, while a `ProtocolObservation` carries a single
`protocol_id`. ADR-0047's canonical
`/interweave/kad/1.0.0/<network-hash>` gives three of those four an
evident home and `role` none, and "wire_major 1 means 1.0.0" is an
inference no document states.

Specify it, then fill in `crates/discovery/cache/src/cache.rs`. A
targeted lookup built on the empty set does not fail loudly — it reads as
"no peer supports this" and silently degrades to no targeting at all.

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
- connectivity-infrastructure peers never gain GossipSub/direct/endpoint/Kademlia authority merely by being connected. **Read this as EXPOSURE, not only authority.** Stages 6-9 built each data-plane entry point to classify its caller — direct ingress, the GossipSub publisher check, `build_answer`'s trust check — so authority is already refused, and an implementer who checks only that will find the invariant apparently met. What is NOT met is the other half: `SubstrateBehaviour` installs `direct`, `broadcast` and `endpoints` on every connection uniformly, so once an infrastructure-only connection exists, that peer can advertise and open those substreams and be refused only after the request has been parsed and accounted. Nothing exercises this today because relay, AutoNAT and DCUtR are absent from the libp2p feature list, so no infrastructure-only connection can be established at all — Stage 11 is the change that creates the first one, which is why the correction belongs here. The protocol set an infrastructure-only connection offers must be restricted at the connection, not merely answered at the request;
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
