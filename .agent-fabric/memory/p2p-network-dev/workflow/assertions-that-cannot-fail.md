---
role: "p2p-network-dev"
class: workflow
description: A green mutation check proves the test catches THAT mutation, not that the assertion asserts what its name says — read the function the assertion calls
tier: 1
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - 839a987cf42ac7a4
---

## A green mutation check proves the test catches THAT mutation, not that the assertion asserts what its name says — read the function the assertion calls

**Before trusting an assertion, read what the function it calls actually
reads.** Two assertions on PR #74 passed for twelve hours and asserted
nothing; both survived a mutation check, because the mutation I chose was
one they happened to catch.

- `assert!(policy.address_dialable(peer, other_route))`, written to prove a
  permanent failure was ADDRESS-scoped. `is_address_dialable` is
  `is_none_or` over the QUARANTINE map, and the test never quarantines
  anything — so it answered `true` for any string, including the address the
  line above had just proved removed. The book is a different map.
- `assert!(err.reason.contains("RelayCircuit"))`, written to prove a refusal
  named the origin. `"…must be admitted as RelayCircuit"` is the message's
  constant tail, present whatever the origin was — the identical predicate to
  the test forty lines up.

**Why the mutation check did not save me.** I mutated `known.remove(addr)` to
`known.clear()`; the test caught it, so I recorded the assertion as
load-bearing. The reviewer mutated to `known.pop_last()` — which drops the
SURVIVOR and keeps the refused address — and everything stayed green while
the test's own message ("the refused route is GONE") was false. A mutation
check is evidence about the mutation you ran, and nothing more.

**How to apply.**

- Choose the mutation that would make the test's NAME false, not the one that
  breaks the code most obviously. "Address-scoped" is falsified by removing
  the WRONG address, not by removing everything.
- Prefer an assertion that names the expected value over one that counts or
  answers yes/no. `assert_eq!(dial_candidates(peer), vec![survivor])` says
  which of two things happened; `known_addresses() == 1` and a boolean both
  pass for either.
- A yes/no API built on `is_none_or` / `unwrap_or(true)` answers `true` for
  absent state. In a test that never populates that state, it is a constant.

**A cited command is a claim, and an uncited one is safer than an unrun one.**
Same family, found twice more on the same PR. A comment cited
`grep -rn 'DialOrigin::DiscoveryReconnect' crates/ apps/` as the check
settling a claim; the sites it needed are `Self::`-qualified, so the command
returned the test arm and the comment quoting itself. The claim was true and
its evidence was fabricated by typing a qualified form after running an
unqualified one — worse than no citation, because it reads as verified.

The replacement was wrong too, and differently: it **enumerated the hits**.
A count goes stale the moment anyone writes the word in a comment, and one
already had, in the same file. Cite what the CODE sites are, not how many
lines a search returns. Run the command, paste its real output, and prefer a
claim that survives a comment being added.

**Shortening is an edit; check each DROPPED clause for whether it was doing
work.** On PR #75 a commit shortened two comment blocks because the
meta-commentary was where every error had lived — and cut three load-bearing
clauses doing it. "IN PLACE" was the word telling two mutants apart, and
without it the paragraph contradicted its own neighbour. "This fixture has one
peer" carried an inference that made a `len()` argument follow. "The only one
an ordinary edit would reach" was true, actionable, and unrelated to the
defect being removed. Writing more and writing less are the two ways to get
this wrong; the fix is neither, it is checking each clause against the source.

**Knowing something is not checking it.** The same commit asserted "this tree
runs no mutation tooling" while [[mutation-testing-evaluated-and-dropped]]
records that cargo-mutants WAS run here, and `.gitignore` carries a
`mutants.out*/` rule because a `git add -A` once swept twenty-two of its files
into a commit. Recall produced the claim; a grep would have produced the true
one ("none runs in CI or `xtask`").

**Re-read the paragraph, not the sentence.** Three times on that PR I wrote a
claim its own neighbouring sentence disproved — "the other two are one-token
edits" beside a sentence rejecting one of them for changing two tokens; a
disclaimer hedging a phrase the same commit had deleted; a failure message
describing the predicate it no longer matched. The defect is editing one
sentence without re-reading the ones around it.

The same one-directional blindness produced the other P2 of that round: the
circuit guard was pinned against WIDENING and never against NARROWING, and
`local.any(P2pCircuit) && !remote.any(P2pCircuit)` passed everywhere while
restoring the defect the PR existed to fix. **Pin a guard in both
directions.**

Related: [[mutation-checks-need-the-formatted-text]] is the patch failing to
apply; this is the patch applying to a test that was never asserting the
claim. [[ask-reviewers-to-generalise-findings]] is what surfaced both.

**And do not assert a property of code you have not opened.** On PR #85 I
wrote, in an accepted decision record, that "no check compares the schema to
the code, so deleting both fields passes silently". A reviewer opened the
file: the validation table is a FIXED-LENGTH `[...; 28]` array with a row for
that exact key, mirrored by a test table of the same length that says so in
its own comment. Deleting the field is a compile error and a test failure. So
the warning pointed at the one instance that fails loudly and away from three
that fail silently — the opposite of its purpose. The same paragraph also
claimed a "survivor" in a scenario where both blocks are deleted, and named
one code site where `grep -c` finds ten.

A claim about what a check does or does not cover is a claim about code. Open
the check.

*References: ask-reviewers-to-generalise-findings, mutation-checks-need-the-formatted-text, mutation-testing-evaluated-and-dropped*
