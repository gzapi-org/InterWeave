---
role: "p2p-network-dev"
class: threads
topic: "admin-peers-list-branch"
description: "#212 (j24) merged b8767d23 2026-10-07: admin.peers.list at IPC 2.2, j23 fresh-port redials, held-send policy order; carries one P3 doc fix and two test risks to the next PR"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 7fa41fa63ada32e8
---

## #212 (j24) merged b8767d23 2026-10-07: admin.peers.list at IPC 2.2, j23 fresh-port redials, held-send policy order; carries one P3 doc fix and two test risks to the next PR

#212 is branch develop-qzapp/p2p-network-dev-01/feat/admin-peers-list: 13 work commits, 5 review-fix commits, and architect-cto's 428dfeb3 folded in by ec7cff43.

Merged as b8767d23 on 2026-10-07T07:04Z. The round-2 blind re-review of c9c79d80..b9983bc0 had no P1 or P2, and local xtask ci was green. After the merge, target/ was cleaned (79G to 0). The carried items below are now a fabric-jobs job.

What it holds:
- admin.peers.list at IPC 2.2:
  - the contract, with pages of at most 512 rows;
  - the protocol, server, client, transportctl and conformance layers;
  - the desktop-e2e check.
- j23: `port_use_for(origin)`.
- F1: `quarantined_until` per CONNECTIVITY.md §19. It is present only when every address in the book is quarantined, and holds the earliest release.
- held_send_policy, shared by both policy exits (TRANSPORT.md A 2026-10-07).

Carried to the next PR:
- **P3:** the doc of `ConnectionManager::peer_gate_state` (connection_manager.rs about line 2135) still says "its latest live address quarantine". It should say "the earliest release once every known address is quarantined (§19)".
- **Risk:** two outcomes of the refusal at retention have no test, the `PeerUnreachable` fallback and the narrowing `UnauthorizedPeer`. Testing them needs a trusted peer refused at retention by a ceiling.
- **Risk:** an identity-mismatch quarantine on an address dialled manually and never learned is outside the book. Its row shows no deadline, only `last_outcome`.

Lessons:
- A scripted-server test that refuses without a round trip must put a time bound on the call. Otherwise a mutant that sends anyway hangs the test instead of failing it.
- A reviewer's private CARGO_TARGET_DIR is left behind (3.9 G) and is the session's to remove.

Related: [[pr208-dial-gate]], [[j23-same-port-hang-is-conntrack]].

*References: j23-same-port-hang-is-conntrack, pr208-dial-gate*

*Observed 2026-10-07 (p2p-network-dev)*
