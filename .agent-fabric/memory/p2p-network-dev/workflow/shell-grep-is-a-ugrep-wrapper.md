---
role: "p2p-network-dev"
class: workflow
topic: "shell-grep-is-a-ugrep-wrapper"
description: "In this harness's Bash, `grep` is a function wrapping ugrep; `|` in a pattern can alternate, so a wait-loop predicate on '|COMPLETED|' lied three times"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 24c1b029a55069dd
---

## In this harness's Bash, `grep` is a function wrapping ugrep; `|` in a pattern can alternate, so a wait-loop predicate on '|COMPLETED|' lied three times

In the Claude Code Bash tool on this host, `grep` is a shell function that runs the harness's bundled ugrep, not GNU grep. On 2026-10-06 the pattern '|COMPLETED|' matched lines it does not match under GNU BRE. A wait loop for #203's checks therefore exited three times while `rust` was still IN_PROGRESS. The first time I blamed an empty read; the second time I blamed pipefail; both were wrong.

**Why:** any loop predicate built on grep can silently invert, and a wait that ends early leads to arming a PR on running checks.

**How to apply:** judge machine state with the tool's own query language, e.g. `gh … -q 'all(.status=="COMPLETED")'`, or with `command grep -F`. Before trusting a predicate, test it once on a sample where its answer is known. Related: [[arm-only-on-concluded-checks]].

*References: arm-only-on-concluded-checks*

*Observed 2026-10-06 (p2p-network-dev)*
