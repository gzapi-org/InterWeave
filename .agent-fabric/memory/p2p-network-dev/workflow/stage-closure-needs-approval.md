---
role: "p2p-network-dev"
class: workflow
description: Never close a canonical stage without asking the owner first; Stage 10 named explicitly
tier: 1
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - cdac9df4c972f78e
---

## Never close a canonical stage without asking the owner first; Stage 10 named explicitly

Do NOT close a canonical stage — flip `[workspace.metadata.interweave].status`,
write the stage's `Met.` block, or open the next stage — without returning to the
owner and asking to proceed. Stated for Stage 10 on 2026-08-31 and framed as a
general gate, not a one-off.

**Why:** a stage closure is the repository's own record of what was proved, and
CLAUDE.md §1 says the `Met.` block IS the record — later stages read it instead of
re-deriving. A closure written from the implementer's own view of the work is the
one document nobody independently checks, and this repository has already had a
`Met.` block corrected twice for claiming evidence its tests could not carry
(Stage 9's conformance-suite claim, and the "fails three checks" count that was
actually one). The owner is the check that does not share the implementer's beliefs.

**How to apply:** implementation work, tests, fixes and PRs proceed normally. The
closure step specifically stops and asks. Bring to that conversation: which exit-gate
items are met, which are not and why, and what the `Met.` block would claim — so the
approval is about the evidence rather than about the milestone.

Related: [[verify-subagent-coverage-not-just-findings]] — same shape, a claim of
coverage is not coverage.

*References: verify-subagent-coverage-not-just-findings*
