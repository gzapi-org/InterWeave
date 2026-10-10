---
role: "p2p-network-dev"
class: threads
topic: "pr215-trust-overlay"
description: "#215 trust overlay merged 92d01a1a 2026-10-07: ADR-0028 A 2026-10-07 built, IPC 2.3 trust rows; P3s carried as j27; multiple area reviewers on one PR worked"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 06db8cb6fff22ff8
---

## #215 trust overlay merged 92d01a1a 2026-10-07: ADR-0028 A 2026-10-07 built, IPC 2.3 trust rows; P3s carried as j27; multiple area reviewers on one PR worked

#215 is branch develop-qzapp/p2p-network-dev-01/feat/trust-overlay, opened 2026-10-07 with 13 work commits. architect-cto's commits 15637ba5, 1d45e692 and faf03412 are folded unrebased; the range line is b8767d23..faf03412. j25's carry rides on it.

What it waits on:
- **rust-ui-dev's supply** onto the branch (seq 15391): TrustList, the ui-model and the settings screen. The stale TrustExplanation copy in labels.rs is theirs.
- **architect-cto's word on two points** (seq 15452):
  - "store-less negotiates no minor above 2.2" has no site, because every harness that serves IPC now keeps a store. Is it reworded, or does the server get a max-minor setting?
  - The ADR says "writable by others", but the code refuses any 0o077 bit, as the key's rule does.

Facts the code does not state on its own:
- **translate() validates endpoint static subsets against config.yaml's allowed_peers.** So the overlay's effective set must be applied after validation (Composition::with_allowed). Rewriting the profile instead would refuse to start a daemon whose endpoint names a peer the operator revoked.
- **A full max_connected_peers ceiling refuses a dial before any socket.** To reach the refusal-at-retention exit, the ceiling has to fill during the dial: a third peer's inbound arrives while a slow proxy holds the dial (direct-v2 `..._by_the_ceiling_...`).
- **A mutant that is an unreachable arm is not a proof.** A `_` arm after `Some(path)` survived. Plant `.filter(|_| false)` on the scrutinee instead.

Related: [[admin-peers-list-branch]], [[supply-commits-to-the-callers-change]].

Merged 92d01a1a on 2026-10-07T11:30Z, with 26 work commits (7 from architect-cto, 8 from rust-ui-dev) and three review rounds plus three area reviewers.
- The P3s carry as j27.
- rust-ui-dev fixes the client's pre-2.3 trust message on its own branch.

Workflow lesson, from the owner: "merge as a unique pr but use multiple reviewer". When a PR is large, dispatch one blind reviewer per area (persistence, wire and contracts, client and harness), each with a scoped brief and a P1-only escape outside its area. All four reviews count on the head.

Reviewers left 3.5-4.5 GB CARGO_TARGET_DIRs that their hook refused to rm. Surface those to the user instead of deleting them on a reviewer's behalf.

*References: admin-peers-list-branch, supply-commits-to-the-callers-change*

*Observed 2026-10-07 (p2p-network-dev)*
