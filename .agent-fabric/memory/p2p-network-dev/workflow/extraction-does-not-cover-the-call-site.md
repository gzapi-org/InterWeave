---
role: "p2p-network-dev"
class: workflow
description: "Extracting an inline decision into a function makes the DECISION testable, never its call site; and \"fixing\" an unenforced-claim finding must not trade an honest disclaimer for a stronger false one"
tier: 1
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - 243e5f8552043a5f
---

## Extracting an inline decision into a function makes the DECISION testable, never its call site; and "fixing" an unenforced-claim finding must not trade an honest disclaimer for a stronger false one

**When a review says a comment's claim has no test, the two legitimate
answers are a test that really holds it or a WEAKER comment. Replacing the
disclaimer with a stronger claim is a third outcome, and it is worse than
the finding.**

InterWeave PR #86, rounds 5 and 6. An inline arm in a command closure
dropped a refused channel's wire mapping only when no session held it. Its
comment said, honestly, `NOTHING TESTS THIS LINE`, and said why: reaching
the arm needs a live Swarm and a GossipSub filter that refuses a topic, and
the installed filter never refuses.

I "fixed" it by extracting the decision to
`BroadcastState::forget_if_unheld`, writing a test for it, and replacing the
disclaimer with "this test fails if the condition goes".

**The extraction covered the decision and not the arm.** The test calls the
method directly. Reverting the CALL SITE to a bare `forget` left:

- the new test green (it never reads the call site);
- its sibling green (it had always said so);
- `clippy -D warnings` green — **the method is still reached from the test
  module, so there is no `dead_code`**, which is the thing that makes this
  invisible.

So the runtime defect the condition exists to prevent was reachable again
with every check passing, and the comment now asserted the opposite. CLAUDE.md
§4 says a weaker true comment beats a strong unenforced one; I had traded the
true one away.

**How to apply.**

- An extracted unit's test covers the unit. Its CALL SITES need their own
  mechanism. Ask: "which line does this test actually read?"
- When a behavioural test for the call site is unavailable, a **structural**
  guard is the available substitute — count the call shapes in the file's
  production text (`include_str!` of its own source, cutting every
  column-zero `#[cfg(test)] mod`). Pick patterns that cannot collide as
  substrings, and assert one count per pattern, never the sum.
- `dead_code` does NOT backstop this. A helper reached only from tests looks
  live to clippy.
- Three claims usually need narrowing, not one: the extracted item's doc, the
  new test's comment, and the OLD comment that said the thing was untestable
  — that last goes stale in the very commit that makes it testable.
- After changing a claim, re-plant the mutation the NEW wording names and
  confirm it fails. The wording is what you are testing, not the code.

**THE SHARPEST INSTANCE, PR #86 round 7, found only by a class-scoped audit
after six ordinary review rounds had missed it.** The PR existed to
canonicalize the `(peer, address)` key at `attempt_dial`. That one line —
`address: canonical_dial_address(peer, address)` — was pinned by NOTHING.
Reverting it to `address.to_owned()` left **202 tests and clippy green**.

Why every mechanism missed it:

- eleven new unit tests called `canonical_dial_address` and `learn_route`
  **directly**, so they tested the helper, never its use;
- the structural guard counted the CONSUMERS (`learn_address(`,
  `record_failure(`, …) and not the PRODUCER, so the revert changed none of
  its numbers;
- no integration test supplied a suffixed address at all —
  `grep -rn '/p2p/' tests/ --include=*.rs` returned nothing;
- three other comments in other files asserted "`attempt_dial` now
  canonicalizes", inheriting the gap and making it read as settled.

Fix was one table row: `("canonical_dial_address(", 3)`.

**The generalisable rule: a guard that counts who READS a key does not pin
who WRITES it.** When a change introduces a normalisation, count the
normalising call itself, not only the sites that consume its output. And
when several comments across files assert the same fix, that is weak
evidence it is enforced — it is the same belief written down repeatedly.

**When three rounds in a row each find one instance of the same class, stop
iterating and audit the class.** Six incremental rounds missed this; one
audit scoped to "a comment claiming more than its mechanism holds" found it
plus five others. See [[audit-agent-ends-review-whack-a-mole]].

Related: [[assertions-that-cannot-fail]],
[[mutation-checks-need-the-formatted-text]],
[[commit-before-mutation-checking]],
[[audit-agent-ends-review-whack-a-mole]].

*References: assertions-that-cannot-fail, audit-agent-ends-review-whack-a-mole, commit-before-mutation-checking, mutation-checks-need-the-formatted-text*
