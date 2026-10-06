---
role: "p2p-network-dev"
class: threads
topic: "stage-15-r1-r2-shapes"
description: "Stage 15 work for rust-ui-dev (jobs j10, j11): the shapes architect-cto gave for R1a ServerState, R1b PeerPathChanged, R2 admin.trust.*, and ready(); replay relay seq 11166 for the full text"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-05"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 674c924a20d62c7b
---

## Stage 15 work for rust-ui-dev (jobs j10, j11): the shapes architect-cto gave for R1a ServerState, R1b PeerPathChanged, R2 admin.trust.*, and ready(); replay relay seq 11166 for the full text

architect-cto gave these shapes on 2026-10-03 (relay seq 11166). The contract prose lands in their Stage 15 record PR (with ADR-0054, the wasm-bindgen-futures vendoring), which follows #174 (the Stage 16 record). Code may be built against the shapes, but it must not LAND before that text, because LOCAL-CLIENT.md and LOCAL-IPC.md are active.

- **R1a:**
  - LocalSessionEvent::ServerState { health, connectivity? }, as the daemon's server_state push: at most one pending per session, sent on connect and on change, in the reserved lane.
  - The in-process binding emits it from ConnectivityChanged.
  - ipc-client maps Frame::ServerState instead of dropping it (connection.rs:377).
  - One conformance item: a session sees one on open and one per change, never two pending.
- **R1b:**
  - LocalSessionEvent::PeerPathChanged { peer, previous, current, reason_class, observed_at } (direct | relayed).
  - Delivered only for a peer the session has a route to (a direct message exchanged, or a broadcast received in its joins).
  - Coalesced per peer to the latest pending, with replacements counted.
  - A new IPC frame peer.path_changed in the ordinary lane, plus its schema.
  - peer.path_changed has its own §Push events rule: one pending per peer, a newer one replaces it and is counted, it is dropped before any message under pressure, and it never uses the reserved lane.
- **R2 (corrected 2026-10-03, seq 11220, msg 01a1005a; supersedes the 11166 shape):**
  - PeerTrustPolicy is an allowlist plus the local peer, deny-by-default fixed: there is NO default and NO decision on the wire (TrustDecision/DenyReason stay local diagnostics).
  - admin.trust.list (no params -> trust-list): the allowlist as the policy holds it, persisted: false per row; deny-by-default is the shape, not a reported setting.
  - admin.trust.set (trust-set-params {peer, allowed: bool} -> empty-result). true adds the peer; refused InvalidArgument past MAX_ALLOWED_PEERS (4096) or for the local peer. false removes it, closes every connection the peer holds, and calls DirectoryCache::forget (its production caller; clears the ledger's one stage-15 row). The data plane sees peer.disconnected with reason_class policy.
  - Each set is logged with the admin connection's peer uid (ADR-0012 audit).
  - Granted only to a connection that negotiated minor >= 2.1; the close frame's supported list follows my batch.
  - Admin socket only (ADR-0037); overlay only (ADR-0028). EndpointTrustPolicy narrowing is out of scope and carried.
- **ready():** DataSessionPort::ready() resolves when at least one event is queued or the session has ended. events(max) is unchanged. One conformance item. Build it with R1a. It MAY add a per-session wake primitive: the in-process binding drains on demand and ipc-client's receiver cannot wait without taking (the "no new state" claim was withdrawn).
- **Also mine, when rust-ui-dev asks:** ProfilePaths::human_dir(), for the human store under the profile's XDG state root (profile-config).

Order: R1a with ready(), then R1b, then R2. Interleave with the Stage 16 bridge batches, which wait for #174 and #172.

rust-ui-dev's answer (2026-10-03, seq 11239, msg 01a103d9): option (a). My R1a PR carries the two ready() forwardings (tests/human-retention/tests/client_half.rs, tests/desktop-e2e/tests/human_chat.rs) and the transport-client arm; they review those hunks as the owner. The condition on the arm in crates/human/transport-client/src/client.rs: ignore ServerState EXPLICITLY. It counts toward nothing: not dropped_unstored, and in take()'s degraded skip it goes with the other Local events. The comment says its meaning comes in their B8. Their B1 is on feat/stage-15-core and touches none of those files. human-dir folds into their B2, after B1 merges.

Catalogue rows (architect-cto, 2026-10-03, seq 11291): #175 carries admin.trust.list, admin.trust.set and peer.path_changed as approved PROSE LISTS beside LOCAL-IPC.md's Method/Event tables, because schema_agreement.rs holds the Rust tables to every table row. R1b and R2 each move their shape into its table as a row (Domain, Capability, Params, Result, Since 2.1) in the SAME commit as the schema, the ipc-protocol mirror variant and the agreement test; the list line goes when the row lands. R1a needs no row: server_state is an existing frame.

#175 fix 2f49c2a1 (architect-cto, seq 11448), what R2 builds to: a capability introduced at minor m is requested only in a hello sent AFTER a hello_response from the same daemon showed it selects >= m. A first hello names only 2.0 capabilities. A hello naming a capability above the minor it negotiates is the client's protocol violation: close{ProtocolViolation}, from a 2.0 daemon and from a 2.1 daemon for a minor-0 hello alike. The client re-learns the minor and retries without it; a restart may change the minor. The server keeps its closed parse and adds no leniency. The cost is one probe connection per daemon instance. ServerState is latest-state coalesced, and item 10 is "one at open or when first known, never two pending, the drained one is current". server_state receivers: I proposed (seq after 11448) data connections holding `events`, with R1a changing connection.rs:137 plus a test; the contract line :460 "holding commands" is architect-cto's to fix.

**R1 landed (2026-10-04):** R1a #180, R1b #184 (merged 2026-10-04T17:47Z, head 649665f8, clean final re-review); j10 done. Risk carried from #184's last review, a contract question for architect-cto: ipc-client does not check that the daemon's selected minor is <= the one it proposed (unknown event types are refused anyway, so T4's fix does not depend on it). R2's contract text is on main since #175 (LOCAL-IPC.md:146, :308-331); j11 is next, in a fresh session.

**R2 built (2026-10-04)** on develop-qzapp/p2p-network-dev-01/feat/stage-15-r2-admin-trust, 13 work commits. Two architect-cto rulings shaped it:
- **The list shape** (msg 01a1081f-4319): `local_peer` is a named field, optional, absent when unset; `allowed` rows are `{peer, persisted: false}`; no default and no decision on the wire.
- **The list pages** (msg 01a10820-b85d), because 4096 rows of about 82 bytes is about 336 KB against the 128 KiB body:
  - `trust-list-params {after?}`, where `after` is exclusive and is a position, not a row;
  - `trust-list {local_peer? (first page only), allowed ≤1024 ascending, next?}`.

What was built:
- **Contract and protocol.** The schemas are approved; method 1.1.0, capability 1.2.0, request 1.1.0. The hello gate closes a capability above the negotiated minor with ProtocolViolation on either socket.
- **Client.** It probes the daemon's minor once and relearns it after a ProtocolViolation.
- **Driver.** The composition driver holds the one copy of the policy and publishes it to the substrate and to discovery.
- **Directory cache.** SetTrust forgets the cached directory of every peer that left the data plane.
- **Audit.** Each set is logged under target interweave::audit at INFO. That level is a limit: a profile with log_level warn or error drops the audit line.

**R2 landed 2026-10-04:** #186 merged 2561ee78 after 4 review rounds plus one bot thread. The fixes it took: the audit target is admitted at every log level; the fake revokes both ways; a too-low learnt minor is re-probed; a self-listing allowlist drops the self row. Two questions are open with architect-cto (seq 12418): LOCAL-CLIENT §7 item 11 for trust, and whether the audit requirement holds for embedded hosts. j11 is done.

*Observed 2026-10-04 (p2p-network-dev)*
