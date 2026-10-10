---
role: "p2p-network-dev"
class: solution
topic: "discovery-relearn-holds-a-newly-allowed-peer"
description: "Composition discovery: a candidate offered to the book while untrusted is held back 5 min by the learned map, so a peer allowed at run time stays unreachable unless set_trust clears it"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 3614e282bd2f84b3
---

## Composition discovery: a candidate offered to the book while untrusted is held back 5 min by the learned map, so a peer allowed at run time stays unreachable unless set_trust clears it

`crates/transport/composition/src/discovery.rs`: `changed_candidates` hands each candidate to `SwarmRuntime::learn` once and records the offer in `learned`, offering it again only when its addresses change or after `RELEARN_INTERVAL_MS` (300 s). The learn door gives an UNCLASSIFIED peer no address. So a statically configured peer that is not trusted at start is "offered", refused and recorded; allowing it later at run time left it unreachable for five minutes.

Found by the real-runtime test in Stage 15 R2 (`a_trust_change_connects_and_revokes_and_is_reported_as_policy`, composition/tests/admin.rs). It failed only when a discovery round ran before the allow, and passed in 0.03 s otherwise. A test that passes alone is not evidence: run it six times.

Fixed in `Discovery::set_trust` (branch develop-qzapp/p2p-network-dev-01/feat/stage-15-r2-admin-trust): a peer newly allowed loses its `learned` record, so the next round offers it again. The unit test is `a_newly_allowed_peer_is_offered_again_at_once`.

The class: any per-peer "already offered" memory keyed only on the addresses goes stale when the peer's TRUST changes. Look for the same in any later cache placed in front of a trust-gated door. Related: [[kademlia-routing-table-is-a-second-door]].

*References: kademlia-routing-table-is-a-second-door*

*Observed 2026-10-04 (p2p-network-dev)*
