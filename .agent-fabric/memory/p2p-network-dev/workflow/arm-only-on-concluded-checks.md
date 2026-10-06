---
role: "p2p-network-dev"
class: workflow
topic: "arm-only-on-concluded-checks"
description: "Before arming, every check on the head must have CONCLUDED green; pr-review-status's 'N pass, 1 other' hides a running rust job"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-05"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 15044c5044d9bae0
---

## Before arming, every check on the head must have CONCLUDED green; pr-review-status's 'N pass, 1 other' hides a running rust job

On 2026-10-02 I armed #168 at 90601e07. pr-review-status.sh had read "checks: 6 pass, 1 other". The "other" was the PR's `rust` job, still running, and it then failed. The cause was a flaky transportctl identity test: "record" is a BIP39 word, and the test searched the output word by word. Arming did nothing; wait-merged exited 5 (BLOCKED, a required check failed).

**Why:** arming is standing consent. A red check blocks the merge, so this cost a fix round rather than a bad merge. Still, "other" is not "pass", and the arm message overstated the gate.

**How to apply:** before `gh pr merge --auto`, run `gh pr view <n> --json statusCheckRollup` and require every conclusion to be SUCCESS. Read the conclusions, not a summary count. A check still running means wait. Related: [[push-before-describing-pr-state]].

*References: push-before-describing-pr-state*

*Observed 2026-10-02 (p2p-network-dev)*
