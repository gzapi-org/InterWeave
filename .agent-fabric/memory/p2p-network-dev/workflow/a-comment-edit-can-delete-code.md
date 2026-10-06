---
role: "p2p-network-dev"
class: workflow
topic: "a-comment-edit-can-delete-code"
description: "A python exact-replace of a comment block whose anchor included the next code line dropped that line (#180, notices.end()); run the tests after any \"comment-only\" edit"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-05"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 070b48cdbbf11ecc
---

## A python exact-replace of a comment block whose anchor included the next code line dropped that line (#180, notices.end()); run the tests after any "comment-only" edit

On #180 (2026-10-04) a "comment-only" review fix replaced an `old` block that ENDED with the code line `self.notices.end();`, and the `new` text carried only the comment. The call vanished. A comment-only re-review passed it, because it read the comment and not the deleted line. Only the compiler's dead-code warning on the next build showed it.

**Why:** an exact-replace anchor is chosen for uniqueness, so it often runs into the code line below the text being changed. The new text is then written from the intent ("reword the comment"), not from the anchor.

**How to apply:**
- When an anchor spans a code line, check that the replacement carries it. Better: anchor on comment lines only.
- After any edit that calls itself comment-only, run the crate's build and tests and read the diff's `-` lines before committing.
- Brief a re-review with "the range changes only a comment" only when `git diff --stat` and the `-` lines confirm it.

Related: [[a-cleanup-regex-spans-what-sits-before-its-anchor]], [[prefer-python-over-shell]].

*References: a-cleanup-regex-spans-what-sits-before-its-anchor, prefer-python-over-shell*

*Observed 2026-10-04 (p2p-network-dev)*
