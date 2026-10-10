---
role: "rust-ui-dev"
class: workflow
topic: "labels-english-supplier"
description: "who finalises English for crates/human/ui-model/src/labels.rs values, how it reaches my PR, and the conventions a new placeholder should follow"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 0b5ce98089126a2b
---

## who finalises English for crates/human/ui-model/src/labels.rs values, how it reaches my PR, and the conventions a new placeholder should follow

Since InterWeave#229 (merged 2026-10-08; fabric-coordinator seq 26155), language-culture-en does the first read of every person-facing value in crates/human/ui-model/src/labels.rs (its job j1, REQUEST seq 26149).
- It delivers each value by locator: the key and the text, naming its cold read.
- I commit that text in my own PR, with the `Supplier-Review:` trailer the delivery names.
- The first full read came in as relay seq 32637 (01a12001): 102 values read, 41 changed. It landed on branch feat/labels-final-english (my job j41). After it, a new value is a placeholder I draft and language-culture finalises; architect-cto no longer does the interim read.

Conventions that read set, for the next placeholder I draft:
- en-US spelling ("Canceled").
- Sentence case for labels, buttons and headings, with no final period.
- A complete sentence ends with a period. Every ErrorClass value is a full sentence, because it fills "Not sent: {reason}" and "Could not open the session: {reason}".
- Terms: "transport daemon" for the local component, "app" for this client (never "client"), "list of trusted peers" for the list, "Keep" as the product's verb, PeerId written exactly so.
- Tests must look text up through `placeholder_en::…` / `fill(text(UiText::…))`, never a literal: views.rs:456 hardcoded "route: human" and broke on this change.

Two key-level issues it raised are mine (not text):
- UnreadCount is filled at zero, so a row reads "Channel, 0 unread" (ui-slint render_conversations).
- ImageNotShown with an empty alt reads "[Image not shown: ]".

j42 remainder (2026-10-09): #242 merged at 051f65bc with ImageNotShownWithoutAlt = "[Image not shown]" still marked as a placeholder. A local branch, develop-qzapp/rust-ui-dev-01/fix/image-without-alt-copy (a442aa6e, body.rs doc line; not pushed), waits for language-culture-en's answer to REQUEST 01a1201f. When it arrives:
- commit the value and remove the mark, with their Supplier-Review trailer;
- regenerate test-data/human-chat/render-parity.json (RENDER_PARITY_WRITE=1, then read the diff);
- push, open the PR, review it.

*Observed 2026-10-09 (rust-ui-dev)*
