---
role: "p2p-network-dev"
class: workflow
topic: "arm-through-arm-sh"
description: "InterWeave PRs are armed with tools/gh/arm.sh <n> --basis \"<one line>\", never gh pr merge --auto by hand (since #177, 2026-10-04)"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-05"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - c578681f5c5b43c2
---

## InterWeave PRs are armed with tools/gh/arm.sh <n> --basis "<one line>", never gh pr merge --auto by hand (since #177, 2026-10-04)

Arm an InterWeave PR with `tools/gh/arm.sh <n> --basis "<one line>"`, which forwards to agent-fabric's runtime/github/arm.sh (devex-tooling broadcast, relay seq 11455; InterWeave #177, agent-fabric #89/#90).

**Why:** the tool applies the gate (blind review on the head, threads, checks, the security-boundary paths from agent-fabric projects/interweave/integration/gh/arm.json). With no rules for the project, or on an unexpected answer, it exits 2 and arms nothing.

**How to apply:** read `tools/gh/arm.sh --help` before the first use. A boundary path the rules miss is proposed to fabric-coordinator; meanwhile `--boundary` forces the gate. A renamed file is judged by both names. See also [[arming-rule-by-pr-commit-count]] and [[arm-only-on-concluded-checks]].

The owner's rule from 2026-10-04 (devex broadcast, seq 11520): `--no-boundary` waives the security-boundary gate ONLY with architect-cto's approval, which is a DECISION or REPLY addressed to me that names the PR. Never waive on my own reading or on the owner's word. arm.sh is gaining `--waiver <MESSAGE-ID>`. Arming another session's PR on the owner's word takes `--any-owner` and a basis that names the word (done for #175, 2026-10-04). The tree's tools/gh/arm.sh needs #177 in the branch; otherwise call ../agent-fabric/runtime/github/arm.sh directly.

*References: arm-only-on-concluded-checks, arming-rule-by-pr-commit-count*

*Observed 2026-10-04 (p2p-network-dev)*
