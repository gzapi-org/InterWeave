---
role: "p2p-network-dev"
class: threads
topic: "pr137-landed-carry-to-batch-3"
description: "Stage 12 batch 2 (#137) merged fe4da3d1 on 2026-09-28; the P3s and risks it carries into batch 3"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-05"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 0bc899a0958f4f9f
---

## Stage 12 batch 2 (#137) merged fe4da3d1 on 2026-09-28; the P3s and risks it carries into batch 3

#137 (Stage 12 batch 2, composition + ADR-0011 "the book remembers the proof") merged as
fe4da3d1 on 2026-09-28 after eight review rounds; round 8 of 4a04f891 clean on P1/P2.

Carried into batch 3 (see [[review-loop-same-invariant-escalate]]):
- hand_over's and settle's hold-until-publish of an outcome unit are unpinned (need a DURING_ADMIT interleaving test).
- ConnectionPolicy::admit's two outside-book sub-terms unpinned (subsumed by the reservation).
- set_trust bound sentence / "table cannot take" wording overclaims (outcome-unit refusal, lapsed stuck peers); release_from_book doc says "both callers".
- Risks: non-monotonic now_ms breaks R+Q<=max; retirement pass publishes per hand-over (quadratic); O(n) outside-book count per outcome.
- UPDATE 2026-09-28: all of the above landed in #138 (batch 3a) except the two perf risks. #138 carries into batch 3b: shutdown's 1,024 cap keeps the OLDEST, is not derived from event_capacity, drops silently, and makes composition/src/runtime.rs "returns every event" false; the F1 mesh sync (sync on every retained establishment) and the held-path flush wiring are untested end to end (a path event is held only while direct-exchange slack keeps polling past capacity); settled_at unpinned on record_success/permanent_failure/authorization_withdrawn/locally_refused; kademlia_driver.rs ~879/3221 still describe the try_send flush.
- UPDATE 2026-09-28 (#139, batch 3b, armed at dee71dda): carries -- F5 close/drop leaving joins untested (needs a substrate diagnostic of a session's joins); N1 shutdown dropped count lands in a counter nobody reads after shutdown (log or return it; counter doc wrong); N2 revoke_endpoint takes holder and records notice under two locks (close-between leaks an owed entry; cancelled revoke loses holder); N3 ClaimGuard cancellation untested, join() inserts into joined only after the await; N4 events() doc says every event has a receipt time (Local does not); N5 DrainLeased took DrainEndpoint's doc summary; pre-existing: name-keyed drain_endpoint still public, events() not cancellation-safe; F2 (driver not awaiting sessions) has no test.
- Older: RouteConfirmed origin filter untested; "never an address a peer asserted" overclaims; shut substrate before draining; learned memo not cleared on book refusal/eviction; paths assume every Disconnected.

*References: review-loop-same-invariant-escalate*

*Observed 2026-09-28 (p2p-network-dev)*
