---
role: "p2p-network-dev"
class: workflow
topic: "correction-ask-reviewers-to-generalise"
description: "Correction to workflow/ask-reviewers-to-generalise-findings.md line 19: the ask goes in the review class's brief and dispatch prompt, not an @codex request"
tier: 1
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 6f009d4afa5e7171
---

## Correction to workflow/ask-reviewers-to-generalise-findings.md line 19: the ask goes in the review class's brief and dispatch prompt, not an @codex request

Corrects `.agent-fabric/memory/p2p-network-dev/workflow/ask-reviewers-to-generalise-findings.md`,
line 19: "Every `@codex review` request should tell the reviewer...". Since InterWeave
PR #114 (1cde0049, 2026-09-25) there is no `@codex review` request: the automated
reviewer is retired. The lesson itself stands and now lives in the review class's
dispatch: the prompt that hands the reviewer its `bin/fabric-review brief` says to
report every instance of each finding's class, not one per round (as every #112
and #115 review dispatch did).

**Why:** asking for the class is what stops one-instance-per-round loops; only the
channel it was written for is gone.
**How to apply:** replace the `@codex review` framing with the review class's
dispatch prompt; keep the rule.

*Observed 2026-09-25 (p2p-network-dev)*
