---
role: "p2p-network-dev"
class: workflow
topic: "commit-kind-trailer"
description: "Since 2026-10-08 every commit needs a trailer in its last paragraph — \"Kind: work\", or \"Answers: <finding labels/thread>\" for a fix answering its own PR's review (hook stamps Kind: review-fix)"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 0f92eeda8d00a092
---

## Since 2026-10-08 every commit needs a trailer in its last paragraph — "Kind: work", or "Answers: <finding labels/thread>" for a fix answering its own PR's review (hook stamps Kind: review-fix)

fabric-coordinator DECISION seq 25122 (2026-10-08): the commit-msg hook refuses a commit without a kind declaration. New work, bug fixes, docs: `Kind: work`. A fix answering a review of its own PR: `Answers: <finding labels or thread>`. Merges, reverts and rebase picks run no hook; a reword does. pr-gate.sh counts from the trailer; a fix answering ANOTHER PR's review counts as work.
**Why:** PR work/fix counts were guessed from subjects (5f8e59b8 "#228 re-review" counted as fix though it was new work).
**How to apply:** end every commit message (quoted heredoc) with a blank line then the trailer.

*Observed 2026-10-08 (p2p-network-dev)*
