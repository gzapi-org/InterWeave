---
role: "p2p-network-dev"
class: workflow
description: "When requesting a review, ask the reviewer to search for other instances of each finding's CLASS before reporting — the user pointed this out after rounds of one-instance-per-round"
tier: 1
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - fcdfa096840fc002
---

## When requesting a review, ask the reviewer to search for other instances of each finding's CLASS before reporting — the user pointed this out after rounds of one-instance-per-round

Every `@codex review` request should tell the reviewer that **when it
finds something, it must look for other instances of the same class and
report them together**.

**Why:** without it, a single defect class arrives one instance per
round. On PR #58/#59 the same two shapes — "an assertion reduced to
whichever field was easiest to reach" and "an input adapter that does
nothing, so every conditional assertion is vacuous" — cost roughly six
separate rounds, each fixing one site. The user named it: *"if it gets a
finding, verify by analogy for the same class of findings"*.

**How to apply.** Put it in the review comment, with concrete axes
rather than a general plea:

- an assertion checking one field of an event → find every assertion
  doing that on other events;
- a check that validates only if something happens (`if let` inside a
  loop over drained events) → find every check with that shape;
- an inert test subject or adapter → find every other one;
- a prose claim not carried by the test it cites → check the other
  claims in that block.

Say plainly that five instances of one class in one round beats one per
round for five rounds.

The same rule applies to my own fixes: after fixing a finding, grep for
its shape before pushing. See [[verify-subagent-coverage-not-just-findings]]
for the sibling habit, and [[audit-agent-ends-review-whack-a-mole]] for
what to do when the class is architectural rather than local.

*References: audit-agent-ends-review-whack-a-mole, verify-subagent-coverage-not-just-findings*
