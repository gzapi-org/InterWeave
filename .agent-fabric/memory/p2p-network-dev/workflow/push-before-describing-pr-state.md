---
role: "p2p-network-dev"
class: workflow
topic: "push-before-describing-pr-state"
description: "Never report a PR's commits, review coverage or arming readiness from the local log; push first, then read the remote"
tier: 1
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 05971b6b0372b951
---

## Never report a PR's commits, review coverage or arming readiness from the local log; push first, then read the remote

Push the branch BEFORE saying anything about what a PR contains, what a
reviewer covered, or whether it is armable. Read the answer back from the
remote (`gh pr view <n> --json headRefOid,commits`), never from
`git log origin/main..HEAD`.

On 2026-09-19 (PR #108) I answered "how many commits?" from the local log
and described eight review-fix commits, their review coverage and the
arming arithmetic in detail — while `origin` was eight commits behind. The
PR had 13 commits; I was describing 21. The owner caught it: "if these are
about 108 you must put them there." Every statement I had made about round
coverage was therefore wrong, because a reviewer reads the remote and
nothing else — the round-2 reviewers could not have seen any of the eight,
and the automated reviewer had nothing new to answer.

**Why:** pushing is ungated and `main` is protected, so there is never a
reason to hold commits locally. The cost of not pushing is not a lost
commit — it is that every subsequent claim about review state is made
against a tree nobody else can see, and it reads as authoritative.

**How to apply:** push at the end of each commit batch, and again before
any sentence containing a commit count, a head sha, "reviewed", "covered"
or "armable". When a review round ends and fixes are made, push before
dispatching the next round, not after. Related: [[arming-rule-by-pr-commit-count]].

Second lesson from the same session, unrelated cause: `git checkout --
<file>` after a mutation check destroyed uncommitted work four separate
times. Commit the real change FIRST, then mutate, then restore. The
project brief already says this; four repeats in one session say it is not
enough to have read it.

*References: arming-rule-by-pr-commit-count*

*Observed 2026-09-19 (p2p-network-dev)*
