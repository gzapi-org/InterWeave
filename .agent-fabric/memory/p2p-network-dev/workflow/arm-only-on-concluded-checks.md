---
role: "p2p-network-dev"
class: workflow
topic: "arm-only-on-concluded-checks"
description: "Before arming, every check on the head must have CONCLUDED green; pr-review-status's 'N pass, 1 other' hides a running rust job"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 15044c5044d9bae0
  - 74189380e9f6729c
---

## Before arming, every check on the head must have CONCLUDED green; pr-review-status's 'N pass, 1 other' hides a running rust job

On 2026-10-02 I armed #168 at 90601e07. pr-review-status.sh had read "checks: 6 pass, 1 other". The "other" was the PR's `rust` job, still running, and it then failed. The cause was a flaky transportctl identity test: "record" is a BIP39 word, and the test searched the output word by word. Arming did nothing; wait-merged exited 5 (BLOCKED, a required check failed).

**Why:** arming is standing consent. A red check blocks the merge, so this cost a fix round rather than a bad merge. Still, "other" is not "pass", and the arm message overstated the gate.

**How to apply:** before `gh pr merge --auto`, run `gh pr view <n> --json statusCheckRollup` and require every conclusion to be SUCCESS. Read the conclusions, not a summary count. A check still running means wait. Related: [[push-before-describing-pr-state]].

2026-10-06 (#194): arming does not skip the PR's own checks. When a GitHub outage cancels the required PR checks, an armed PR stays BLOCKED (mergeQueueEntry null) until they pass. Re-run only the cancelled required jobs (`gh run rerun <id> --failed`), and expect the queue to run CI again. I told the owner "arm now = one run", and that was wrong. Cost was never the issue: InterWeave is public, so Actions bills 0 ms, and actions-health.sh's overage warning was devex's defect (relay seq 12998), being fixed.

*References: push-before-describing-pr-state*

*Observed 2026-10-06 (p2p-network-dev)*
