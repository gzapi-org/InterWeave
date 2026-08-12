
# Architecture/implementation spikes

Spikes validate version-sensitive or deployment-sensitive assumptions. They are not production implementation.

## SPIKE-001 — Claude Channel/package compatibility

**Objective:** validate the exact Channel manifest/MCP packaging accepted by the target Claude Code release.

**Experiment:** minimal non-network Channel stub using current official docs and Telegram package patterns.

**Evidence:** startup capability handshake, notification delivery, tool exposure, shutdown behavior.

**Decision unlocked:** exact package manifest/bridge packaging for implementation.

---

## SPIKE-002 — direct request-response wire behavior

**Objective:** validate selected rust-libp2p request-response primitive under timeout, cancellation, connection reuse, and protocol mismatch.

**Experiment:** two local peers with a non-production test codec.

**Evidence:** failure events, substream/connection behavior, timeout/cancel semantics.

**Decision unlocked:** implementation codec/task details without changing the direct-vs-GossipSub architectural decision.

---

## SPIKE-003 — Kademlia integration validation

**Objective:** validate the complete optional Kademlia blueprint before implementation is promoted or `enabled: true` is supported, including behaviour-originated dial policy.

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
- server-mode reachability evidence is classified exactly as designed and does not claim AutoNAT verification;
- record filtering/equivalent prevents value/provider inserts from becoming stored application state;
- disjoint query paths and multi-seed topologies measurably reduce single-path capture, without claiming Byzantine resistance;
- 20-node convergence/resource behavior is acceptable with default bounds.

**Decision unlocked:** implement the already-specified `KademliaDiscovery`/driver design, adjust bounded defaults, or keep the provider architecture-only. This spike does not authorize ChannelId/provider records or untrusted discovery-only connections; those require separate ADRs.

---

## SPIKE-004 — NAT/relay deployment matrix

**Objective:** determine which AutoNAT/relay/DCUtR mechanisms target deployments actually require.

**Experiment:** defined home NAT, corporate NAT/firewall, public VM, relay-loss scenarios.

**Evidence:** inbound/outbound reachability and recovery matrix.

**Decision unlocked:** Phase 9 connectivity feature set.

---

## SPIKE-005 — same-user IPC hardening (conditional)

**Objective:** determine whether OS ownership/permissions are sufficient for target deployments or whether same-user client authentication is required.

**Experiment:** hostile same-UID local client attempts against daemon capability model.

**Evidence:** threat-model fit and operational cost.

**Decision unlocked:** retain OS-boundary-only IPC trust or add a local credential/token mechanism.
