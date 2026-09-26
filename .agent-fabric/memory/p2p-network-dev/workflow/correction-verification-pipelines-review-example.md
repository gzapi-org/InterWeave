---
role: "p2p-network-dev"
class: workflow
topic: "correction-verification-pipelines-review-example"
description: "Correction to workflow/verification-pipelines-must-fail-loudly.md line 79: the example command's --automated-only flag no longer exists; the lesson stands"
tier: 1
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 7bd84b8cf6e61a8a
---

## Correction to workflow/verification-pipelines-must-fail-loudly.md line 79: the example command's --automated-only flag no longer exists; the lesson stands

Corrects one example in `.agent-fabric/memory/p2p-network-dev/workflow/verification-pipelines-must-fail-loudly.md`,
line 79: `bash tools/gh/pr-review-status.sh 86 --wait 30m --automated-only; echo "EXIT=$?"`.
Since InterWeave PR #114 (1cde0049, 2026-09-25) the automated reviewer is retired
and pr-review-status.sh (now forwarding to agent-fabric's
`runtime/github/pr-review-status.sh`) takes no `--automated-only`; the example
reads `bash tools/gh/pr-review-status.sh 86 --wait 30m; echo "EXIT=$?"`.

The slice's lesson is unchanged and still true: never end a verification step in
`| head`/`| tail`; capture the exit code.

**Why:** a copied example with a flag the reader refuses fails as an invocation
error, which reads like a gate result.
**How to apply:** change the example only; keep the section.

*Observed 2026-09-25 (p2p-network-dev)*
