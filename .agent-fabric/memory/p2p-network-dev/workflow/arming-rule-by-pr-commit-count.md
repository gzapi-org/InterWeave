---
role: "p2p-network-dev"
class: workflow
topic: "arming-rule-by-pr-commit-count"
description: "when to arm a PR's merge without asking the owner — 8 to 16 commits of work (review-round fixes do not count) arm on your own once the review gate is met; fewer, ask; keep the work at or under 16 commits"
tier: 1
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 9172a91fa3b6b451
---

## when to arm a PR's merge without asking the owner — 8 to 16 commits of work (review-round fixes do not count) arm on your own once the review gate is met; fewer, ask; keep the work at or under 16 commits

The owner's rule for arming `gh pr merge --auto` (given 2026-09-18 on
PR #89 after I had waited hours for the word; made the rule for every
developer role by fabric-coordinator's DECISION the same day, GZCoord
seq 2250, which also added the follow-up half below):

- Count only the commits of WORK — the change as opened. Commits
  made to answer review findings do not count toward the limit (the
  owner, 2026-09-18, second message).
- A PR with **8 to 16 work commits**: arm it once CLAUDE.md §9's
  review gate is met (a posted review of the current head, zero
  unresolved P1/P2) — no need to ask.
- A PR with **fewer than 8** work commits: ask before arming.
- Keep the work **at or under 16 commits**: past that the reviewer
  becomes less accurate. Split the change into another PR before
  opening it, not during the review loop.

**Why:** a small PR is cheap to look at and the owner wants a say; a
mid-sized one that has been through the review loop is finished work
and the wait only idles the session; a large one is where a blind
review misses things, so the bound is on the PR, not on the asking.

**How to apply:** at the gate, count the commits up to the first
review round (`git rev-list --count origin/main..<head at round 1>`),
not `origin/main..HEAD`. 8–16 → post the arming-basis comment and arm;
<8 → ask. When a change is shaping up past 16 commits, split it before
opening the PR. Never arm without the gate regardless of count.

**P3s after the clean round go to the next PR** (the owner, 2026-09-18, on #93): once a round reports nothing at P1/P2, arm at THAT head; the P3s it lists are fixed as the FIRST commits of the following PR on a fresh branch, not appended to the armed one — another round on comment-level findings costs a review and a CI run for nothing the merge needs. Say so in the arming-basis comment.

**Follow-up onto a PR architect-cto opened** (the second half of seq
2250): when my work implements a decision record, contract or
migration architect-cto has an OPEN PR for, push my commits onto THAT
branch rather than opening a second PR, so decision and
implementation are one range under one review. Before the first push,
a `REPLY` to architect-cto naming the branch and what I am adding
(they may decline — then my own PR). State on the PR whose range is
which (`<sha>..<sha>: <login>`). The count rule then covers both
lanes' work commits together, review fixes of either excluded, and
the PR's opener arms it — I tell them when my part is at the gate. See [[stage-11-pr84-landed]] for the review
loop this governs.

**One review pass, one PR (the owner, 2026-09-18).** Fixes that arrive together -- an external review's list, a round's P3s -- go into ONE PR whatever crates they span; "independent work, separate PRs" is about unrelated tasks, and CLAUDE.md says PR shape is not commit shape. I opened #99 with a single profile-config commit beside #98's store commits and the owner asked why; it was folded into #98. A one-commit PR is the shape the 8-16 rule exists to prevent: every PR costs a review round.

**The session batch (the owner, fleet-wide, 2026-09-18; gzapp's #885 states it in its CLAUDE.md).** One branch per session per working day (or per task when a task is genuinely bigger), 8-16 work commits, two pushes (one opens the PR, one follows the fold of origin/main), one PR. Under 8 ask the owner, except a docs-only PR and a single-check tooling fix, which arm at the gate; a security-boundary change always waits for a review of its current head AND the owner's word. A piece another lane asked for is supplied onto the caller's branch, never its own PR.

**Restated by the owner 2026-09-18 evening, verbatim in substance:** "when you go over 8 commits, not counting the review coming after, you can arm without asking my permission. so try to keep together at least 8 commits" -- so the floor is a target, not only a threshold: keep adding real work to a branch until it holds 8 before opening (the eighth on #100 was a re-measurement of an assay row, real work, not padding).

*References: stage-11-pr84-landed*

*Observed 2026-09-18 (p2p-network-dev)*
