
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

**Objective:** verify that the pinned rust-libp2p identity API can export/import the exact Ed25519 32-byte secret boundary assumed by `cp2p-ed25519-bip39-entropy-v1` and reproduce the same PeerId across backup/restore.

**Experiment:** non-production local harness only. Starting from the test-only zero-secret fixture and multiple CSPRNG-generated Ed25519 identities, obtain the portable secret bytes using the supported identity API/serialization boundary, encode/decode the 24-word BIP-39 entropy form, reconstruct the key, and compare public keys and PeerIds. Exercise process restart and the exact dependency versions selected for implementation. Also prove that the BIP-39 PBKDF2 seed output is never accepted as the transport secret. Explicitly verify which rust-libp2p API exposes the exact 32-byte Ed25519 secret seed used by the recovery format; do **not** derive mnemonic entropy from an opaque/private-key protobuf blob or any 64-byte expanded `secret || public` representation. Exercise the read-only `identity verify` path separately from restore.

**Evidence:** byte-for-byte secret round trip, the repository golden fixture PeerId, random-key round trips, documented API calls/serialization assumptions (including the exact 32-byte seed accessor/import path and any larger protobuf representation encountered), verify-only no-write behavior, and confirmation that no mnemonic/private-key material enters logs, IPC, crash reports, or network traces.

**Decision unlocked:** production `transportctl identity backup/restore` implementation against the frozen recovery contract. If the current library boundary cannot reliably expose/reconstruct the exact Ed25519 seed, keep recovery implementation disabled and revise the identity serialization adapter without silently changing the mnemonic format.

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
