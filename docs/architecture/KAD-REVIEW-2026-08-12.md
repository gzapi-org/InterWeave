# Kademlia / shared-profile second architecture review closure — 2026-08-12

Status: **review findings incorporated; architecture only; Kademlia remains default-disabled**.

This memo records the disposition of the second full-set review after the Kademlia blueprint was added. It is a delta/review aid; normative behavior lives in the referenced contracts/ADRs.

## Kademlia findings

| Finding | Resolution | Normative locations |
|---|---|---|
| K1 behaviour-originated Kademlia dials bypass ordinary scheduler | ConnectionManager remains **policy** owner; backend adds Swarm-wide `DialAdmissionGate` for explicit and `NetworkBehaviour`-originated dials; SPIKE-003 measures origin/denial/connection volume | ADR-0011, ADR-0009, `kademlia-integration.md`, `transport/libp2p/DESIGN.md`, SPIKE-003 |
| K2 targeted server predicate not locally computable | persist freshness-bounded exact protocol support observation from authenticated Identify in PeerCache; target only trusted peers with fresh positive current-network observation | ADR-0009, ADR-0027, `DISCOVERY.md`, peer-cache/Kademlia provider docs |
| K3 target/health never saturates for small allowlists | `effective_target = min(target, max_routing, remote trusted population)` plus 3-successful-no-progress saturation and exponential exploration backoff capped at 15m | `kademlia-integration.md`, Kademlia provider, tests/SPIKE-003 |
| K4 cross-field config gaps | hard enabled-time rules: target<=max routing; refresh>=min bootstrap; results<=kbucket; all seed sources configured+enabled | config schema, configuration architecture, Phase 1 tests |
| K5 provider/backend dependency ambiguity | new tiny internal libp2p-free `kademlia-control-api`; both concrete crates depend on it, not on each other | rust blueprint, components, Kademlia integration |
| K6 server reachability evidence undefined | v1 distinguishes declared-external vs trusted-peer Identify-observed vs none; no AutoNAT proof claim; `none` degrades server mode | Kademlia integration health, SPIKE-004 |
| K7 Snapshot had no result | asynchronous bounded correlated `SnapshotResult` defined | Kademlia integration internal driver port/tests |

## Generic feature findings

| Finding | Resolution | Normative locations |
|---|---|---|
| F1 direct local fan-out undefined | admitted direct event is independently delivered to every connected event-capable IPC client; broadcast remains join-reference filtered; no primary/round-robin | ADR-0016, LOCAL-IPC, data flows/tests |
| F2 `channels.desired` purpose unclear | profile-level backend pre-warm only; zero local clients means local drop, never buffer/replay, never implicit publish permission | TRANSPORT, configuration, PUBSUB, data flows/failure model |
| F3 self-send undefined | `send(local PeerId)` -> `InvalidArgument`, no self-dial | TRANSPORT, DIRECT, ADR-0012, tests/failure model |
| F4 subscription visibility | `status` includes caller `joined_channels`; profile-desired channels are separate | plugin tool surface/roadmap |

## Additional K1 security clarification

The root dial gate also applies to peers returned *during* iterative Kademlia queries, before a successful connection can be established. Manual K-bucket admission alone is not relied on as the trust boundary for query-generated network activity.

## Phase-freeze impact

Before Phase 1 freezes configuration/models, tests must cover Kademlia cross-field/seed-source validation, protocol-observation bounds, direct local fan-out, profile-desired no-buffer behavior, self-send, and status subscription state.

Before optional Kademlia implementation/support, SPIKE-003 must additionally prove behaviour-originated dial admission/attribution, capability-aware targeted lookup, effective-target/saturation behavior, and `SnapshotResult` semantics on the exact rust-libp2p version.

## Rollout posture

No change: every shipped Kademlia example remains `enabled: false`. An unsupported build rejects explicit enablement; a supporting build still requires operator opt-in.