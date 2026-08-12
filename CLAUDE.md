# Repository instructions

This repository contains the accepted architecture under `architecture/` plus a tracked implementation/test **skeleton** at repository root.

## Current implementation state

- `architecture/` is the normative source of truth.
- `apps/`, `crates/`, `tests/`, `fixtures/`, `test-data/`, `spikes/`, `packaging/`, and `xtask/` are landing zones defined by ADR-0045.
- The root Cargo workspace intentionally has zero members.
- Do not add a production crate manifest/source, Android application build, installer, service unit, or executable test package unless the corresponding implementation phase is explicitly being executed.
- When a phase begins, add only the crate/package(s) needed for that phase and add them to `[workspace].members` in the same change.
- Do not implement production code inside `architecture/`.

## Placement rules

- `apps/*` are thin composition roots; reusable logic belongs in `crates/*`.
- Neutral contracts under `crates/api/*` must not depend on libp2p, Slint, Android, SQLite, Claude SDK, or platform-specific types.
- Put pure tests beside future source, public crate-consumer tests under future `<crate>/tests/`, and multi-crate/network/conformance/E2E tests under repository-root `tests/`.
- Frozen protocol/crypto/config vectors belong in `fixtures/`; mutable scenarios belong in `test-data/`.
- `tests/support` is test-only and must never be a production dependency.
- Spike code stays under `spikes/` and does not become production implementation without normal design/review/test integration.

## Hard architecture boundaries

- Broadcast means GossipSub. Directed traffic means the dedicated direct libp2p protocol. Do not route directed messages through GossipSub.
- One transport profile owns one persistent PeerId. Model B EndpointIds are local routes under that PeerId, not new identities.
- Direct-capable IPC v2 clients own one exclusive configured EndpointId lease. Direct v2 routes to exactly one endpoint; no undocumented all-client fan-out.
- A remote source EndpointId is peer-asserted routing metadata, not proof of a human, Claude instance, role, or authorization.
- Endpoint-specific policy may narrow but never widen profile PeerTrustPolicy.
- Endpoint directory is optional/trust-gated/opt-in/bounded and must never become DiscoveryProvider, GossipSub, or Kademlia state.
- Keep Claude Code and human UI isolated from libp2p concepts. Multiaddr, Swarm, ConnectionId, GossipSub mesh internals, Noise sessions, and Kademlia routing tables stop below transport/local-client boundaries.
- Discovery is replaceable and advisory. It does not dial, grant trust, manage pub/sub, route endpoints, or interpret payloads.
- Trust, discovery, connection management, endpoint routing, broadcast, and direct messaging are separate responsibilities.
- GossipSub validation follows ADR-0029: objective invalidity = Reject, valid-but-locally-unauthorized original publisher = Ignore, valid+authorized = Accept.
- Caller must be joined before broadcast; reply tokens do not recreate subscriptions.
- IPC v2 JSON bodies are capped at 128 KiB and must carry every legal 48 KiB payload plus max endpoint metadata. Data-plane clients never receive endpoint/shutdown admin capability.
- No human identity, social graph, agent roles, Git workflow, task state, issue state, repository ownership, or read receipts belong in transport contracts.
- Transport/daemon delivery remains realtime/non-durable. Human application durability is limited to ADR-0044: pending outbound, unread inbound, and receiver-kept-after-read inbound.
- Secrets and persistent PeerId keys are local state, never repository configuration.
- Standard v1 includes Kademlia support; configured Kademlia entries default `enabled: true` with explicit opt-out. No EndpointId/channel/application/trust records enter the DHT.
- Standard v1 includes ADR-0035 AutoNAT-v2/Circuit-Relay-v2/DCUtR. Keep connectivity-infrastructure authorization distinct from application trust per ADR-0036.

When implementation work reveals an accepted decision is wrong, update the ADR/contract rather than silently contradicting it in code.
