---
role: "p2p-network-dev"
class: workflow
topic: "review-dispatch-shape-in-interweave"
description: "Blind review dispatch here: subagent_type code-review, model fable, lowercase description starting \"review\"/\"re-review\", no isolation -- opus is refused since the fabric's 2026-09-25 guard change (it was the reverse before)"
tier: 1
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 67b5fb746cad364c
---

## Blind review dispatch here: subagent_type code-review, model fable, lowercase description starting "review"/"re-review", no isolation -- opus is refused since the fabric's 2026-09-25 guard change (it was the reverse before)

The shape that dispatches a blind review in InterWeave changed on
2026-09-25. Before, the project's `.claude/settings.json` hook exempted a
review from worktree isolation only on `model: opus`, and `fable` was
refused as "does not set isolation". After agent-fabric #40 and InterWeave
#114 (both merged 2026-09-25), the fabric's dispatch guard refuses `opus`
for the review class ("The review class rides the fable alias, always")
and `fable` passes -- measured on PR #117's review the same evening.

**Why:** the alias is the fabric's routing vocabulary; the review class's
real model comes from its agent file, gated by
`routing/policies/review-grade.json`, and the guard checks the alias.

**How to apply:** `Agent(subagent_type: "code-review", model: "fable",
description: "review <what>")`, no isolation, prompt pointing at a brief
rendered by `bin/fabric-review brief`; post the report with
`tools/gh/post-review.sh <n>` (the only reviewer since #114). If the guard
message changes again, the refusal text names the rule to follow.

*Observed 2026-09-25 (p2p-network-dev)*
