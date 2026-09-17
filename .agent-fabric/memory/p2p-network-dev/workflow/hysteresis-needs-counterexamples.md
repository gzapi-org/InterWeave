---
role: "p2p-network-dev"
class: workflow
description: Three rounds on one hysteresis rule, each fix introducing the next defect; what would have caught it in one
tier: 1
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - b4870580b2a04b6b
---

## Three rounds on one hysteresis rule, each fix introducing the next defect; what would have caught it in one

PR #84's `derive()` (reachability verdict, AUTONAT.md §5's "two fresh independent failures") took three rounds, each fix creating the next defect:

1. One outcome per `(address, server)` key — a failure overwrote the success it contradicted, so ONE report from a counting observer unverified the address.
2. Keep both, count the server in both sets — at threshold 1 the address stayed verified a full TTL while its only observer said unreachable.
3. Hold on `saying_reachable + saying_unreachable >= threshold` — a DISSENTER that never said reachable made up the quorum, so removing a verifying server left the verdict standing on the word of the server calling the address unreachable.

The rule that finally held: the quorum is servers that SAID reachable — still saying it, or *reversed* (fresh success under a fresh failure). A dissenter counts only toward the two-failure invalidation; an aged-out success is silence and leaves the quorum.

**Why:** each round I fixed the named case and wrote the condition from it, without asking who else the condition now admits. The automated reviewer found all three, and the third only because it enumerated `remove_server` and the exact-TTL instant — cases no test of mine covered.

**How to apply:** before writing a hysteresis condition, enumerate the population it quantifies over and name each class explicitly (here: still-saying, reversed, dissenting, silent). Then write one test per class. And when a fix adds a term to a comparison, ask what else satisfies that term — a count is not an identity. See [[assertions-that-cannot-fail]], [[audit-agent-ends-review-whack-a-mole]].

*References: assertions-that-cannot-fail, audit-agent-ends-review-whack-a-mole*
