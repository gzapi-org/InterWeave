---
role: "p2p-network-dev"
class: workflow
topic: "an-allowance-must-not-be-earned-by-what-occupies-it"
description: "When a bounded outbox gates polling, a class of buffered event must not add to the allowance it sits in -- the term cancels and the bound disappears; exclude it from the count and give it its own cap"
tier: 1
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - c58747589cd54c6a
---

## When a bounded outbox gates polling, a class of buffered event must not add to the allowance it sits in -- the term cancels and the bound disappears; exclude it from the count and give it its own cap

InterWeave #117, review R1 (2026-09-25). The runtime polls the Swarm only
while `outbox.len() < event_capacity + progress slack`. To stop a buffered
Kademlia settlement from taking a pending exchange's slot, the first fix
ADDED the count of buffered settlements to the allowance. But `outbox.len()`
already counted them, so `N + T < cap + slack + T` reduced to
`N < cap + slack`: T could grow without limit while the Swarm kept being
polled (the blind review's P1, F1). The old rule's bound had been the
feedback -- stop polling, and the library starts no new queries.

The fix that held: exclude the class from the counted length
(`buffered - T < cap + slack`) AND give it its own stop (`T < one driver
call's worth`), plus a larger hard cap on the ungated command path sized
above everything the gated side can leave (d7717b2e -> 6251ad83).

**Why:** a bound whose right-hand side grows with the thing it bounds is no
bound; unit tests that check "one buffered X buys one slot" pass while the
ceiling is gone.

**How to apply:** for any "allowance per outstanding thing" predicate, write
the inequality out and cancel terms before committing; every class admitted
past the base capacity needs its own ceiling, and an ungated push path needs
a bound that sits above the gated side's worst case. Related:
[[stage6-ingress-burst-test-is-load-timed]].

*References: stage6-ingress-burst-test-is-load-timed*

*Observed 2026-09-25 (p2p-network-dev)*
