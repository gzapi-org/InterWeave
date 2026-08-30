
# Architecture/implementation spikes

Spikes validate version-sensitive or deployment-sensitive assumptions. They are not production implementation.

Under [ADR-0046](../adr/0046-bottom-up-implementation-order.md) and the [`BOTTOM-UP-IMPLEMENTATION-PLAN`](./BOTTOM-UP-IMPLEMENTATION-PLAN.md), spikes are **just-in-time gates**: run each spike immediately before the implementation boundary it unlocks, capture evidence, and promote validated assumptions into permanent regression/conformance tests. Production crates must never depend on spike packages.

## SPIKE-001 — Claude Channel/package compatibility

**Objective:** validate the exact Channel manifest/MCP packaging accepted by the target Claude Code release.

**Experiment:** minimal non-network Channel stub using current official docs and Telegram package patterns.

**Evidence:** startup capability handshake, notification delivery, tool exposure, shutdown behavior.

**Decision unlocked:** exact package manifest/bridge packaging for implementation.

---

## SPIKE-002 — transport wire/race and GossipSub cache behavior

**Objective:** validate rust-libp2p request-response for direct v2/endpoint-directory behavior and pin the target GossipSub duplicate-cache/authenticity ordering used by the frozen broadcast design.

**Experiment:** two local peers with non-production `/direct/2.0.0` and `/endpoints/1.0.0` codecs. Exercise explicit destination, omitted/default destination, resolved endpoint response, no_route privacy class, queue-admission delay/overload, multiple protocol IDs, and unsupported-major negotiation. **Drive many concurrent retransmissions with the exact same dedup key/message ID through the real rust-libp2p request-response task scheduling, including response timeout/cancellation races, and verify one local enqueue plus shared owner result. Also exercise reservation-capacity overflow.**

**Evidence:** failure events, substream/connection reuse, timeout/cancel semantics, exact protocol-family negotiation behavior, proof that AcceptedV2 can be withheld until bounded local route admission without pathological Swarm blocking, and empirical proof that concurrent same-key retransmissions cannot double-enqueue or escape the bounded reservation map.

**Decision unlocked:** implementation codec/task/channel details without reopening endpoint routing or the direct-vs-GossipSub decision.

### GossipSub duplicate-cache validation ordering

Using the exact target rust-libp2p version, verify that an invalid signed-source/sequence claim cannot create a lasting duplicate-cache entry that suppresses a later valid message with the same `GossipSubMessageIdV1`. Also verify that two authenticated publishers carrying the same application-envelope `message_id` produce distinct mesh IDs and both reach application validation. If library ordering differs from this requirement, document and prototype an equivalent pre-cache authenticity gate before Phase 2 is accepted.

**Result (2026-08-24): PASS**, against `libp2p 0.56.0` — the version the substrate ships, pinned exactly with its lock committed, because a floating requirement cannot rebuild the graph the evidence describes. Evidence and the reproducing harness are in [`spikes/spike-002/`](../../spikes/spike-002/README.md). Every architectural decision this spike could have disturbed held: twenty-four concurrent same-key retransmissions through real request-response scheduling produced **one** local enqueue and twenty-three waiters that stayed attached until the owner's asynchronous admission resolved, every one of them receiving the owner's outcome — including when that outcome was a rejection, which is the half a waiter manufacturing its own acceptance would fail; the bounded reservation map admitted exactly its per-peer budget and refused the rest with the distinct coarse reason `overloaded`, and — separately, because the two limits fail independently and deleting the global check leaves the per-peer numbers untouched — eight distinct source peers against a small global budget and a generous per-peer one were refused by the global bound; one reserved key could accumulate UNBOUNDED waiters -- `acquire` matched an existing key and returned `Waiter` before consulting either budget, so 40 same-key retransmissions produced 39 attached and zero refusals, each holding a response channel until the owner resolved; fixed in the production `ReservationMap` by charging waiters against the same budgets as owners and returning the whole charge on release, with `ENDPOINTS.md` and `DIRECT.md` amended to say the budgets count requests rather than keys; the five internal route failures `no_route` must collapse (endpoint unknown, disabled, no active lease, missing default, policy-denied) were each produced by the PRODUCTION `EndpointRegistry::resolve_inbound` from five genuinely different registry states -- five distinct local failures, proving the predicates are independent -- and reached the wire through the production `ResolveFailure::to_wire` and `DirectRejectReason` as one byte-identical answer, so a refusal cannot be used as an endpoint oracle (two earlier versions of this experiment manufactured that result and were caught in review; breaking the production encoder or collapsing two production predicates now fails it); an invalid SIGNED source claim -- a signature present, well-formed, and computed over different bytes than it carries, written directly to a `/meshsub/1.1.0` substream because no public API can construct one -- was refused under the FROZEN `GossipSubMessageIdV1`, colliding on source and sequence with a genuine message that was still delivered afterwards, so the rejection left no cache entry (the injector's ability to choose a sequence number is what removed the payload-derived substitution B2 needed); that experiment gates its verdict on a control injection the receiver must ACCEPT, since a hand-encoded frame the receiver cannot parse would otherwise "pass" for the wrong reason, and signing the invalid message correctly makes the later genuine message be suppressed, which is what proves the collision half detects a poisoned cache; the unsigned-source rejection is attributed by per-publisher sequence number across all delivery windows rather than by which window an event landed in, so a late-arriving forgery cannot be mistaken for the genuine message -- making the strict receiver permissive reproduces exactly that false pass and the verdict refuses it; a cancellation race — the owner's connection killed mid-admission, waiters attached on a separate connection to the same peer — left the reservation released and the surviving waiters answered rather than orphaned; `AcceptedV2` was withheld across an await while another peer was served normally and still arrived afterwards; two publishers reusing one application-envelope `message_id` produced distinct mesh IDs under the frozen `GossipSubMessageIdV1` — checked against `fixtures/gossipsub/gossipsub-message-id-v1.json` before being used — and both reached validation; and an invalid signed-source claim arrived at a receive path, was rejected there, and left no duplicate-cache entry, so a genuine message with the same mesh ID was still delivered. That last result rests on a permissive receiver wired directly to the forger: without an independent path, "not delivered" is equally explained by the forgery never arriving, and that count is itself filtered on the exact contested payload rather than on which window an event happened to land in, since a delayed control message could otherwise have been mistaken for the message under test. No pre-cache authenticity gate is required.

The harness exits non-zero when any required observation is false, so `cargo run` cannot report success while its own output disproves the recorded result.

Four findings constrain the Stage 6 implementation:

1. **Timeout attribution is a race.** Both sides run the same `request_timeout`, so whichever fires first decides what the other is told: a responder whose inbound timeout closes the substream first leaves the requester reading `Io(Eof)` rather than `OutboundFailure::Timeout`. Both were observed across runs of the same experiment. Stage 6 must not branch on `Timeout` alone to mean "no answer in time".
2. **A withheld response outlives its channel.** After a timeout the responder still holds a `ResponseChannel` whose `is_open()` is `false`; a late `send_response` has nowhere to go. Producing a response is not evidence the peer heard it.
3. **`OutboundFailure::UnsupportedProtocols`** is the clean, pre-timeout signal for a major-version mismatch.
4. **One connection serves both protocol families**, so no connection-per-protocol accounting is needed.

One GossipSub experiment retains a substitution: the unsigned-publish experiment derives the mesh ID from the payload, because the *public* API does not let a caller choose a sequence number. That is redundant coverage rather than a compromise — the raw `/meshsub/1.1.0` injector in the same harness chooses sequence numbers directly, and the companion experiment collides on source and sequence under the unmodified `GossipSubMessageIdV1`. No claim in this record depends on the substitution, and no future rust-libp2p release is required to remove it.

---

## SPIKE-003 — Kademlia integration validation

**Objective:** validate the complete Kademlia blueprint as a **standard-v1 release gate** before shipping configured entries default `enabled: true`, including behaviour-originated dial policy.

**Experiment:** non-production rust-libp2p harness using the selected crate version and private project protocol namespace. Exercise explicit client/server mode, Identify -> manual `add_address`, `BucketInserts::Manual`, bootstrap, `get_n_closest_peers`, disjoint query paths, record filtering, cached protocol-capability observations, effective-target/saturation logic, and the bounded query scheduler. Run 3-, 10-, and 20-node local topologies plus malicious/stale routing responses.

Instrument the Swarm / behaviour boundary so Kademlia-originated `ToSwarm::Dial` activity is measurable. If the public API cannot attribute dial origin precisely, an instrumented wrapper or throwaway spike-only patch is acceptable evidence; do not infer zero dial activity from the absence of ordinary ConnectionManager scheduler calls.

**Expected evidence:**

- deterministic custom protocol derivation from `network_id`;
- no Kademlia activity when disabled;
- current rust-libp2p Identify/manual-insert hooks behave as designed;
- client/server semantics match the upstream specification;
- bootstrap/query event ordering and automatic bootstrap side effects are understood and counted;
- **behaviour-originated dial volume is measured** by query class, and every such dial is subject to trust, punitive per-peer backoff, shutdown state, and global pending/connection limits through the root dial-admission gate;
- remote peers returned during iterative queries cannot establish a connection when current trust policy denies them;
- random exploration produces useful trusted routing/address expansion within the proposed budgets;
- small allowlists use the effective routing target and can reach a healthy saturated state instead of exploring every minute forever;
- consecutive no-new-peer exploration rounds back off as designed and resume after topology/trust/capability change;
- targeted PeerId lookup is scheduled only with fresh advisory evidence that the target previously advertised the exact project Kademlia **server** protocol; it can recover missing addresses where the DHT knows the target, while client-mode nodes are not misrepresented as generally discoverable;
- cached positive/negative Kademlia protocol observations are superseded by fresh Identify evidence and never grant trust;
- `Snapshot` command/response returns the specified bounded driver state;
- server-mode reachability consumes the mandatory normalized Phase-9 evidence: AutoNAT-verified direct or active relay reservation as strong evidence; configured/Identify hints remain weak;
- record filtering/equivalent prevents value/provider inserts from becoming stored application state;
- disjoint query paths and multi-seed topologies measurably reduce single-path capture, without claiming Byzantine resistance;
- 20-node convergence/resource behavior is acceptable with default bounds.

**Decision unlocked:** implement/ship the already-specified `KademliaDiscovery`/driver design with default-on configured entries, adjust bounded defaults before release, or block standard-v1 release and revisit ADR-0034 if evidence is unacceptable. This spike does not authorize ChannelId/provider records or untrusted discovery-only connections; those require separate ADRs.

**Result (2026-08-30): PASS FOR STAGE 10 IMPLEMENTATION; the v1 RELEASE gate is NOT closed.** Two expected-evidence items above are unmet and need infrastructure this spike does not have: **server-mode reachability evidence is not consumed** (AutoNAT and Relay are absent from the libp2p feature list — SPIKE-004), and **single-path capture is not shown to be reduced** (measured against controls — one seed versus three, disjoint paths off versus on, nine routers with `parallelism` 3 — and NO capture was observed at all, so the topology cannot distinguish the configurations; an absence of difference is not evidence for the option). ADR-0034 makes this spike a release gate for shipping configured entries default-enabled, and that gate therefore stays open. What IS unlocked is implementing the specified provider and driver. Measured against `libp2p 0.56.0` with the `kad` feature — the version the substrate ships, pinned exactly with its lock committed, because half of what is recorded is the library's own behaviour and a floating requirement cannot rebuild the graph the evidence describes. Evidence and the reproducing harness are in [`spikes/spike-003/`](../../spikes/spike-003/README.md). The design survives: the published `network_id` golden vector reproduces from the specification text and yields a valid `StreamProtocol`; a build without the behaviour advertises nothing, dials nothing and queries nothing, against a server-mode control in the same run; `BucketInserts::Manual` means an authenticated identified connection routes NOBODY until an explicit `add_address`; a client-mode node does not advertise the server protocol yet still queries to completion; `bootstrap` reports `NoKnownPeers` on an empty table and completes once one peer is known; `StoreInserts::FilterBoth` counts inbound `PUT_VALUE`/`ADD_PROVIDER` and stores zero of each; a ten-node line and a twenty-node star both converge under random exploration with no routing table exceeding its bound; every `SnapshotResult` field is computable and is a scalar or a fixed-width tag; two `network_id`s on one crate version advertise different protocols and do not mix; and a mode change WITHDRAWS the advertised protocol on the very next Identify exchange, so capability evidence is superseded rather than merely aged. **Behaviour-originated dials were measured rather than inferred** — the brief forbids inferring them, and `SwarmEvent::Dialing` cannot attribute origin — by instrumenting `handle_pending_outbound_connection`, the same hook the production `OutboundAdmission` uses, where a dial arriving with no admission ticket is behaviour-originated by construction. An iterative query originates such dials, aimed at the peer it is walking toward; TODAY's gate refuses every one; the production `PeerTrustPolicy` refuses an unauthorized peer a router returns, so a malicious trusted router cannot manufacture a connection by placing a peer in a response; a peer put into backoff through the manager's own failure path — told nothing about Kademlia — has its query dials refused as `peer backoff`, a draining node is still asked for dials and refuses every one as `shutting down`, a pending-dial ceiling filled by the gate's OWN tickets refuses the rest of a fan-out query and returns every slot when they settle, a connection ceiling of one is consumed by a behaviour dial that establishes and refuses the rest as `connection limit reached`, and a dead address handed over by a router does not advance peer backoff while a known-good route to the same peer remains dialable.

Sixteen findings constrain Stage 10. The first is a sequencing constraint rather than a detail, and three more say the gate cannot be written the obvious way:

1. **Stage 10 cannot begin by enabling the feature.** The production gate refuses every dial without a root admission ticket, and every Kademlia query dial carries none — so turning `kad` on without extending the gate produces a subsystem whose every query dies at the first hop it lacks a connection for, silently, since the refusal surfaces as an ordinary dial failure. The gate must admit a behaviour dial THROUGH `PolicySnapshot::admit` under `DialOrigin::KademliaQuery`; the harness prototypes that as a proposal, not as production code.
2. **A routing insertion starts one query nobody asked for**, on every run, and it dials. The provider's budget must account for it, and policy installed after seeding is installed after the dial it meant to govern.
3. **Under `BucketInserts::Manual` a seed node routes nobody and a star does not converge**: inbound connections insert nothing, so a hub answers every query with an empty list. The admission pipeline must treat an INBOUND connection's Identify observation as a candidate, not only the peers a query returns — §7 of `kademlia-integration.md` reads as an outbound story, and a bootstrap node lives on the other direction.
4. **A query result cannot distinguish a lying router from a peer that is down**: `GetClosestPeers` omits a peer whose only address fails, so the diagnostic must come from the dial outcome, where the address is named.
5. **Capability evidence is withdrawn, not merely aged out**, so the cache must replace on fresh evidence and let negative evidence overwrite positive before the TTL.
6. **`network_id` separation holds at the protocol level.**
7. **`ConnectionPolicy::admit` is NOT the root admission.** It answers trust, backoff, quarantine and drain; the pending-dial and connection CEILINGS are enforced one layer up, in the manager that mints the `DialTicket` reserving them. A gate consulting the policy directly refuses an untrusted or backed-off peer perfectly — every trust test passes — while the global limits influence no Kademlia dial at all.
8. **A `DialTicket` reserves TWO things, and settling it wrongly exempts one silently.** It holds a pending-dial slot and the connection that dial may become, and `Drop` returns both. Dropping it on receipt bounds nothing; dropping it when the dial ESTABLISHES — which reads like the obvious cleanup — bounds `max_pending_dials` correctly and exempts `max_connections` entirely. `record_success` converts it into a `ConnectionSlot` that keeps the reservation until `record_connection_closed`. Relatedly, a gate whose clock is a field pinned at zero makes `PeerBackoff` PERMANENT — a backoff recorded at 0 expires at a moment the clock never reaches — while every assertion about the immediate refusal keeps passing.
9. **Address-scoped policy CANNOT be enforced at the dial hook.** For a behaviour-originated dial, `handle_pending_outbound_connection` receives NO candidate addresses — it is the hook where behaviours contribute them, and the union is dialled after it returns. A gate checking `addresses.first()` reads an empty list and admits every quarantined route while appearing to check. The address exists at `handle_established_outbound_connection`, after TCP connect and before the handler: later than production's check, and the only place a behaviour dial has one.
10. **The two halves key the same route differently.** A behaviour dial's address carries `/p2p/<peer>`; the address book and quarantine map are keyed by the bare transport address, which is what `AdmittedDial` binds. Without stripping that component every quarantine lookup silently misses.
16. **The dial hook must not act on address-scoped denials it cannot evaluate.** Its probe carries the empty placeholder, so `AddressQuarantined` judges an address that does not exist and `PolicyStateFull` reports the address TABLE is full — which under pressure refuses every Kademlia dial, including ones whose real address is already known-good. Both belong at the established hook, where the address is real.
15. **An address failure cannot be recorded without passing the policy that failure just changed.** `record_failure` takes a `DialTicket` and a ticket comes only from `admit`, so settling a fully-failed multi-address dial has no correct ordering: settle as you go and the first failure's peer backoff refuses the rest; mint every ticket first and settlement needs a spare slot per address, which a tight ceiling lacks. Stage 10 needs an address-scoped failure API that requires no admission.
14. **A bounded query scheduler must take its permit BEFORE calling the behaviour, and must mint every settlement ticket before settling any.** `kad::Behaviour::get_*` creates the query when it is invoked, so a scheduler consulted afterwards records a decision already made — ten calls run ten queries whatever the budget said. And the first `record_failure` of a fully-failed multi-address dial advances PEER backoff, after which every later `admit` for the remaining addresses is refused for it and those addresses stay unscored.
13. **The dial hook cannot attribute a dial to a QUERY.** libp2p hands it a connection id and a peer; for a behaviour dial there is no query id and no originating behaviour. Per-class dial volume — a release criterion — must therefore come from the provider declaring what it is running, and is exact only while one class is in flight. Two related consequences the harness had to handle: a settlement that cannot re-mint its ticket must CLOSE the established connection, or it survives outside `max_connections` with no accounting; and a failed multi-address dial must score every address `DialError::Transport` names, since the unscored ones are immediately retryable.
12. **A `DialTicket` binds its address at admission, which a behaviour dial cannot supply.** The held ticket therefore carries an empty placeholder, and `record_success`/`record_failure` feed `ticket.address()` to the address policy and the address book — so every Kademlia route settles against ONE empty entry: the working address never becomes known-good, the failing one is never scored. Stage 10 needs a re-bindable ticket or a settlement API that takes the address; the harness re-mints against the address actually used, recovered from the established hook or from `DialError::Transport` when the dial never established.
11. **A late address probe must discard the CAPACITY answers.** `admit` decides policy and takes a reservation, and the dial being probed already holds one — so at a tight ceiling the probe is refused for capacity that dial is itself occupying. Capacity was decided when the ticket was minted.

What this spike did NOT establish is recorded in the same file and must not be read out of its silence: there is no adversary (disjoint paths was measured as path WIDTH, not as capture resistance), no wire-protocol-violating peer, one machine on loopback with no NAT or loss, and — the material gap — **server-mode reachability evidence is not consumed**, because AutoNAT and Relay are absent from the feature list. SPIKE-004 is where that arrives, and Stage 10 must not treat the rule as validated.

---

## SPIKE-004 — mandatory AutoNAT/relay/DCUtR validation

**Objective:** validate and tune the already-selected mandatory Internet-reachability architecture on the exact rust-libp2p version. This spike does **not** decide whether Phase 9 ships.

**Experiment:** build a non-production harness covering public VM, home NAT, symmetric/restrictive NAT where available, corporate firewall/proxy-like restrictions, Android/mobile carrier-NAT conditions where available, two independent relay/probe services, relay loss/capacity denial, network-interface changes, and trusted vs infrastructure-only peers. Instrument behaviour-originated dials and connection classes. Exercise AutoNAT v2 client/server, Circuit Relay v2 reservations/circuits, DCUtR success/failure, direct-vs-relay racing, address advertisement, and all configured limits.

**Expected evidence:**

- AutoNAT-v2 event/address semantics match the pinned crate and multi-observer aggregation yields stable `unknown`/`verified_public`/`not_verified`;
- probe servers cannot become application data-plane peers merely through infrastructure authorization;
- private/not-verified peers obtain and refresh the target redundant relay reservations;
- relay-derived addresses are usable while reservations are active and withdrawn promptly after loss;
- end application PeerId authentication/trust survives relayed transport and is independent of relay PeerId;
- direct connection preference and the configured relay race head-start avoid unnecessary relay use without causing unacceptable latency;
- DCUtR attempts are measurable, bounded, cooled down, preserve relay fallback on failure, and create stable direct paths on success where NATs permit;
- every AutoNAT/relay/DCUtR behaviour-originated dial is attributable and crosses `DialAdmissionGate`, backoff and total/per-peer limits;
- relay/AutoNAT server quotas and abuse limits behave as specified when those roles are enabled;
- AutoNAT server refuses requester-supplied DNS, loopback/private/link-local/special-use, and unrelated-public addresses; only a literal globally routable candidate whose IP equals the requester observed source IP can reach dial admission;
- Identify-learned AutoNAT/relay infrastructure candidates are disabled by default and, when explicitly enabled, never displace usable static candidates merely because they advertise the protocol;
- relayed inbound handshakes with no original source IP consume the relay-connection/PeerId pre-auth bucket plus global caps;
- relay service admission accepts only configured `DataPlaneTrusted`/`ConnectivityInfrastructureOnly` classes in standard deployment;
- a stable DCUtR upgrade emits `PeerPathChanged` without a duplicate logical `PeerConnected`;
- network change invalidation/recovery converges without changing PeerId or EndpointId routing;
- measured relay bandwidth/connection/probe costs fit default resource budgets.

**Decision unlocked:** pin/tune bounded defaults and implementation adapters for the mandatory design. If the selected rust-libp2p release cannot enforce ADR-0035/0036 safely, **block standard-v1 release and supersede the ADRs explicitly**; do not silently make Phase 9 optional.

---

## SPIKE-005 — same-user IPC hardening (conditional)

**Objective:** determine whether the mandatory split data/admin socket boundary plus OS ownership/permissions are sufficient for target deployments or whether same-user client authentication/user-presence is required.

**Experiment:** hostile same-UID local client attempts against both sockets. Prove that data-socket `client.kind` spoofing cannot obtain `admin.*`, then evaluate the residual case where hostile same-UID code can directly open the admin socket. Compare stricter admin-socket ACL/service-account layouts and viable OS-native credential/user-presence mechanisms.

**Evidence:** threat-model fit, platform coverage, operational cost, and whether a stronger credential can be kept out of config/logs/network payloads.

**Decision unlocked:** retain split-socket OS-boundary trust for same-UID deployments or add a stronger local authentication/authorization mechanism without merging the sockets.

## SPIKE-006 — identity-recovery portability

**Objective:** verify that the pinned rust-libp2p identity API can export/import the exact Ed25519 32-byte secret boundary assumed by `interweave-ed25519-bip39-entropy-v1` and reproduce the same PeerId across backup/restore.

**Experiment:** non-production local harness only. Starting from the test-only zero-secret fixture and multiple CSPRNG-generated Ed25519 identities, obtain the portable secret bytes using the supported identity API/serialization boundary, encode/decode the 24-word BIP-39 entropy form, reconstruct the key, and compare public keys and PeerIds. Exercise process restart and the exact dependency versions selected for implementation. Also prove that the BIP-39 PBKDF2 seed output is never accepted as the transport secret. Explicitly verify which rust-libp2p API exposes the exact 32-byte Ed25519 secret seed used by the recovery format; do **not** derive mnemonic entropy from an opaque/private-key protobuf blob or any 64-byte expanded `secret || public` representation. Exercise the read-only `identity verify` path separately from restore.

**Evidence:** byte-for-byte secret round trip, the repository golden fixture PeerId, random-key round trips, documented API calls/serialization assumptions (including the exact 32-byte seed accessor/import path and any larger protobuf representation encountered), verify-only no-write behavior, and confirmation that no mnemonic/private-key material enters logs, IPC, crash reports, or network traces.

**Decision unlocked:** production `transportctl identity backup/restore` implementation against the frozen recovery contract. If the current library boundary cannot reliably expose/reconstruct the exact Ed25519 seed, keep recovery implementation disabled and revise the identity serialization adapter without silently changing the mnemonic format.

**Result (2026-08-19): PASS**, against `libp2p-identity 0.2.14` — the version `libp2p 0.56` depends on, re-run when Stage 4 showed the originally-measured 0.3.0 would have put two incompatible `Keypair` types in one graph. Every answer was identical. Evidence and the reproducing harness are in [`spikes/spike-006/`](../../spikes/spike-006/README.md). The golden all-zero entropy reproduces the frozen public key and PeerId through libp2p, and 64 CSPRNG identities round-trip byte-for-byte.

Three findings constrain the adapter:

1. `ed25519::SecretKey::to_bytes()` is **`pub(crate)`**. The only public path to the raw seed is `AsRef<[u8]>`; an implementer reaching for the obvious accessor will not find it, and the tempting next move — `Keypair::to_bytes()` — returns a different, 64-byte thing.
2. That 64-byte form is `seed || public`, **not** `expanded || public`, so its first half is genuinely the seed. It is still not what the adapter should use: a 64-byte intermediate is one refactor away from being mnemonic-encoded whole, which this spike's objective forbids.
3. `try_from_bytes` **zeroes the caller's buffer** on success. An adapter passing its only copy loses it, and nothing at the call site says so.

## SPIKE-007 — encrypted software-key envelope

**Objective:** select and validate an audited passphrase-encrypted at-rest envelope for exportable Ed25519 software identities as an optional v2.x feature, without changing PeerId or the mnemonic recovery format.

**Experiment:** evaluate maintained Rust implementations/formats that provide a memory-hard password KDF plus authenticated encryption (for example an age/scrypt-style envelope or another reviewed equivalent). Exercise wrong passphrase, parameter/version migration, atomic rewrite, crash recovery, unattended-daemon constraints, memory/secret handling, and interaction with mnemonic backup/restore. Do not design a bespoke cipher/KDF format in this repository.

**Evidence:** pinned external format/library and parameters, interoperability fixture, failure/recovery behavior, explicit unlock UX/credential source, and proof that passphrases/plaintext private keys never enter normal config/logs/IPC/network traffic.

**Decision unlocked:** add a versioned `identity.key_protection=passphrase-envelope` v2.x option. Until this spike/ADR follow-up lands, standard v1 remains `filesystem-only`.

## SPIKE-008 — Android execution / store-policy viability

**Objective:** validate the selected Android foreground-service lifecycle for persistent first-party P2P messaging.

**Experiment:** build a non-production harness on minimum/current target APIs; start the service from allowed user-visible paths; exercise backgrounding, process reclamation, force-stop/relaunch, notification behavior, Wi-Fi/cellular changes, Doze/OEM-like constraints where measurable, and current Google Play foreground-service declaration/review requirements for `remoteMessaging`. Validate the dedicated non-exported recovery Activity with `android:excludeFromRecents=true` and `FLAG_SECURE`, including screenshot/screen-record/task-snapshot behavior, and validate packaging backup rules on cloud-backup/device-transfer capable devices/emulators. Prove identity/config/human-store exclusions remain effective on the supported Android backup-rule variants and that a partial system/device-transfer restore cannot manufacture or silently replace a transport identity. Also exercise ADR-0044 restart retention: pending outbound + unread/kept inbound survive local process restart, while terminal/read-unkept content does not.

**Evidence:** lifecycle trace, target-SDK/service-type policy matrix, restart/offline behavior, battery/network observations, screenshot/recents behavior, manifest/data-extraction/full-backup rule inspection plus restore matrix for security-sensitive/human-store state; verify future explicit message-backup eligibility would include only unread/kept inbound and exclude pending outbound.

**Decision unlocked:** production Android stay-reachable packaging. Failure does not authorize hidden FCM/cloud dependency; it requires an Android lifecycle ADR update or foreground-only availability claim.

## SPIKE-009 — Android exact-key custody

**Objective:** validate Android Keystore wrapping without changing the Ed25519/PeerId/recovery contract.

**Experiment:** generate AES-256-GCM AndroidKeyStore wrapper; wrap/unwrap the fixed 32-byte recovery fixture; verify identical rust-libp2p PeerId; test TEE/StrongBox reporting, user-presence and background-compatible modes, lock/reboot/process restart, key invalidation and ciphertext tamper. Exercise the in-app 24-word picker, confirm there is no clipboard path or normal free-text mnemonic field, inspect IME/autofill/analytics/crash/state surfaces for phrase leakage, and validate `stay-reachable + user-presence` emits `background_restart_requires_user_authentication=true` after restart until local authentication succeeds.

**Evidence:** exact seed/PeerId round trip and failure matrix; phrase-UI exfiltration checklist; no mnemonic/seed in clipboard, normal IME path, logs, analytics, saved state or crash artifacts; availability diagnostic/restart trace.

**Decision unlocked:** Android production key-at-rest implementation.
