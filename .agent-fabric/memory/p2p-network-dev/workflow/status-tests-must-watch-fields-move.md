---
role: "p2p-network-dev"
class: workflow
topic: "status-tests-must-watch-fields-move"
description: a status/introspection surface gets review findings unless each field is seen at two values through the real wiring; two counters can be one atomic
tier: 1
knowledge_scope: full
distilled_at: "2026-10-05"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 6a469180904c2f4e
---

## a status/introspection surface gets review findings unless each field is seen at two values through the real wiring; two counters can be one atomic

PR #135's blind review found (P2 x2): `ConnectionManager::connections` and `PolicySnapshot::connections` are ONE `Arc<AtomicUsize>` counting SLOTS (established + reserved at admission), so reporting both was one number twice and neither was "established"; and eight status fields were asserted at a single value, surviving constant mutations.

**Why:** a status field read once at its idle value asserts nothing; the brief's rule is "every count has a control showing it moves".

**How to apply:** before exposing a counter, read what it loads (grep the Arc clones); factor the assembly into a function a unit test drives through every field at two values; put the unobservable ones in the test header by name. A mutation equivalent to the fix (e.g. `admits` vs `admits_discovered`, both refusing circuits) proves nothing — mutate to the rule being excluded.

*Observed 2026-09-27 (p2p-network-dev)*
