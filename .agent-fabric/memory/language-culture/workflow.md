---
role: "language-culture"
class: workflow
description: How the English cold read behaves when given only the text and the rules — what it catches and what to check before taking a finding
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "language-culture-en"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - fb7bf265621d313b
---

## How the English cold read behaves when given only the text and the rules — what it catches and what to check before taking a finding

The English holder's cold read (a fresh read-only dispatch given only the
composed text and the vocabulary rules, never the request) caught my own
overclaim ("Published to the channel") and real ambiguities (pronouns, a
status indistinguishable from another). But 9 of its 25 findings
(2026-10-09, labels.rs) were withdrawn on a check against the code: it
cannot know which values fill a template, which labels are inbound-only,
or that a string is for a screen reader only. So judge each finding
against the code before taking it, and record what you withdraw and why.
Related: [[interweave-copy-conventions]]

*References: interweave-copy-conventions*

*Observed 2026-10-09 (language-culture)*
