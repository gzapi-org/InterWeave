---
role: "p2p-network-dev"
class: workflow
topic: "insert-tests-after-the-previous-item"
description: "A scripted test insert anchored on \"#[test]\\n    fn next\" lands between next's doc comment and its #[test], stealing the doc; anchor on the previous item's closing brace"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 0dd1c7dbf52cb988
---

## A scripted test insert anchored on "#[test]\n    fn next" lands between next's doc comment and its #[test], stealing the doc; anchor on the previous item's closing brace

Twice in one session (2026-09-28, stage-12-local-session branch: 8c7eb3f9 and 9f4767fa)
a python str.replace that inserted a new test BEFORE the anchor `    #[test]\n    fn <existing>`
put it between <existing>'s `///` doc block and its `#[test]`, so the doc now described the new
test. Neither build nor tests catch it; a grep of the three lines above each new `fn` did
(fixed in 8c3a389d, 060e0ad9).

**Why:** a Rust item's doc comment sits ABOVE its attributes, so the attribute is not the item's
start.
**How to apply:** anchor inserts on the END of the item before (`    }\n\n` after a known fn), or
append at the end of the test module; after any scripted insert, print the 3 lines above each new
fn and check none is a `///` line.

*Observed 2026-09-28 (p2p-network-dev)*
