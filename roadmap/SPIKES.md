
# Architecture/implementation spikes

Spikes validate version-sensitive or deployment-sensitive assumptions. They are not production implementation.

## SPIKE-001 — Claude Channel/package compatibility

**Objective:** validate the exact Channel manifest/MCP packaging accepted by the target Claude Code release.

**Experiment:** minimal non-network Channel stub using current official docs and Telegram package patterns.

**Evidence:** startup capability handshake, notification delivery, tool exposure, shutdown behavior.

**Decision unlocked:** exact package manifest/bridge packaging for implementation.

---

## SPIKE-002 — direct v2 and endpoint-directory wire behavior

**Objective:** validate rust-libp2p request-response for direct v2 endpoint framing/acceptance and the separate endpoint-directory protocol under timeout, cancellation, connection reuse, and protocol mismatch.

**Experiment:** two local peers with non-production `/direct/2.0.0` and `/endpoints/1.0.0` codecs. Exercise explicit destination, omitted/default destination, resolved endpoint response, no_route privacy class, queue-admission delay/overload, multiple protocol IDs, and unsupported-major negotiation. **Drive many concurrent retransmissions with the exact same dedup key/message ID through the real rust-libp2p request-response task scheduling, including response timeout/cancellation races, and verify one local enqueue plus shared owner result. Also exercise reservation-capacity overflow.**

**Evidence:** failure events, substream/connection reuse, timeout/cancel semantics, exact protocol-family negotiation behavior, proof that AcceptedV2 can be withheld until bounded local route admission without pathological Swarm blocking, and empirical proof that concurrent same-key retransmissions cannot double-enqueue or escape the bounded reservation map.

**Decision unlocked:** implementation codec/task/channel details without reopening endpoint routing or the direct-vs-GossipSub decision.

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

**Experiment:** build a non-production harness covering public VM, home NAT, symmetric/restrictive NAT where available, corporate firewall/proxy-like restrictions, two independent relay/probe services, relay loss/capacity denial, network-interface changes, and trusted vs infrastructure-only peers. Instrument behaviour-originated dials and connection classes. Exercise AutoNAT v2 client/server, Circuit Relay v2 reservations/circuits, DCUtR success/failure, direct-vs-relay racing, address advertisement, and all configured limits.

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
- network change invalidation/recovery converges without changing PeerId or EndpointId routing;
- measured relay bandwidth/connection/probe costs fit default resource budgets.

**Decision unlocked:** pin/tune bounded defaults and implementation adapters for the mandatory design. If the selected rust-libp2p release cannot enforce ADR-0035/0036 safely, **block standard-v1 release and supersede the ADRs explicitly**; do not silently make Phase 9 optional.

---

## SPIKE-005 — same-user IPC hardening (conditional)

**Objective:** determine whether OS ownership/permissions are sufficient for target deployments or whether same-user client authentication is required.

**Experiment:** hostile same-UID local client attempts against daemon capability model.

**Evidence:** threat-model fit and operational cost.

**Decision unlocked:** retain OS-boundary-only IPC trust or add a local credential/token mechanism.

## SPIKE-006 — identity-recovery portability

**Objective:** verify that the pinned rust-libp2p identity API can export/import the exact Ed25519 32-byte secret boundary assumed by `cp2p-ed25519-bip39-entropy-v1` and reproduce the same PeerId across backup/restore.

**Experiment:** non-production local harness only. Starting from the test-only zero-secret fixture and multiple CSPRNG-generated Ed25519 identities, obtain the portable secret bytes using the supported identity API/serialization boundary, encode/decode the 24-word BIP-39 entropy form, reconstruct the key, and compare public keys and PeerIds. Exercise process restart and the exact dependency versions selected for implementation. Also prove that the BIP-39 PBKDF2 seed output is never accepted as the transport secret. Explicitly verify which rust-libp2p API exposes the exact 32-byte Ed25519 secret seed used by the recovery format; do **not** derive mnemonic entropy from an opaque/private-key protobuf blob or any 64-byte expanded `secret || public` representation. Exercise the read-only `identity verify` path separately from restore.

**Evidence:** byte-for-byte secret round trip, the repository golden fixture PeerId, random-key round trips, documented API calls/serialization assumptions (including the exact 32-byte seed accessor/import path and any larger protobuf representation encountered), verify-only no-write behavior, and confirmation that no mnemonic/private-key material enters logs, IPC, crash reports, or network traces.

**Decision unlocked:** production `transportctl identity backup/restore` implementation against the frozen recovery contract. If the current library boundary cannot reliably expose/reconstruct the exact Ed25519 seed, keep recovery implementation disabled and revise the identity serialization adapter without silently changing the mnemonic format.
