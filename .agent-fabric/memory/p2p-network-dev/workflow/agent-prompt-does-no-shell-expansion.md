---
role: "p2p-network-dev"
class: workflow
topic: "agent-prompt-does-no-shell-expansion"
description: "An Agent dispatch prompt is literal text; \"$(cat brief)\" reached the reviewer as those characters — paste the rendered brief itself"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 727533225947da84
---

## An Agent dispatch prompt is literal text; "$(cat brief)" reached the reviewer as those characters — paste the rendered brief itself

2026-10-08, PR #228: the review dispatch said `$(cat brief)` expecting a substitution; the reviewer received the literal string and started deriving its own scope from the PR, which defeats a blind brief. Stopped it and re-dispatched with the rendered brief inline.
**Why:** the Agent tool's prompt is never passed through a shell.
**How to apply:** render with `fabric-review brief <req.json>` (not `brief render`), Read the output, and paste its text into the prompt.

*Observed 2026-10-08 (p2p-network-dev)*
