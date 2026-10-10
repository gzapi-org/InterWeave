---
role: "language-culture"
class: solution
description: "The English conventions and glossary the InterWeave human client's copy holds (labels.rs), set at the first read 2026-10-09"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "language-culture-en"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 86d0192f0ef3cc97
---

## The English conventions and glossary the InterWeave human client's copy holds (labels.rs), set at the first read 2026-10-09

InterWeave human-client copy (crates/human/ui-model/src/labels.rs), as set
by the first English read (delivered to rust-ui-dev-01, REQUEST
01a12001-199d-75d9-8e8d-cceb01ea9bbf, 2026-10-09):
- en-US spelling ("Canceled"), the remit's en-US supply rule.
- Labels, buttons, headings: sentence case, no final period. A complete
  sentence ends with a period; every ErrorClass text is a full sentence,
  because it fills "Not sent: {reason}" and "Could not open the session: {reason}".
- Glossary: "transport daemon" (local component, never "local transport"),
  "app" (this client), "list of trusted peers", "Keep" (human-client-ui.md §6's
  verb), "may have reached the peer" (never "received": §5 keeps it for inbound),
  "might" for possibility (never "may not": it reads as permission).
- labels.rs tests bind the words: no delivery label contains
  read/seen/processed/delivered (substring); all UiTexts distinct, so two
  actions cannot share one text (Retry is "Send again", TryAgain "Try again");
  Offline must say "offline", Unknown must not.
- A new value drafted by its caller carries a `PLACEHOLDER (en)` line comment (line end, or the line above when it wraps), defined in labels.rs module doc on #242 (rust-ui-dev-01, 2026-10-09); grep for it to find work; the commit taking final text removes it.
- Error texts are ONE sentence: #242 review F3 caught a two-sentence Busy text of mine; count sentences after filling the templates.
Related: [[cold-read-without-meanings]]

*References: cold-read-without-meanings*

*Observed 2026-10-09 (language-culture)*
