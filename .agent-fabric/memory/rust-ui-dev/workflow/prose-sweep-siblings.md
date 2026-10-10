---
role: "rust-ui-dev"
class: workflow
topic: "prose-sweep-siblings"
description: "before committing a human-client behaviour or copy change, sweep the sibling crate READMEs, .slint comments, the parity golden, and every test that asserts the old event behaviour (apps/human-* included)"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 9aa4ba661f652934
---

## before committing a human-client behaviour or copy change, sweep the sibling crate READMEs, .slint comments, the parity golden, and every test that asserts the old event behaviour (apps/human-* included)

On #242 (2026-10-09) the review class found stale prose twice in files I had not opened:
- ui-slint's README still called the copy "unreviewed" after I rewrote the claim in ui-model's README and in labels.rs. My grep searched the uppercase phrase only.
- `ui/app.slint`'s ConversationRow comment and ui-slint's README image sentence still described the old drawing after I changed `render_conversations` and `drawn_body`.

**Why:** a client change's prose lives in four places: ui-model and ui-slint READMEs, `ui/*.slint` comments, and `test-data/human-chat/render-parity.json`, the golden Android compares against. The diff shows none of the ones I did not edit.

**How to apply:**
- Search case-insensitively for the old behaviour's own words across `crates/human`, `apps/human-*` and `test-data/human-chat`, `.slint` and `.md` included, before the commit.
- A new drawing branch in `drawn_body` gets a parity case (`RENDER_PARITY_WRITE=1`, then read the diff).

- When an EVENT's meaning changes (e.g. a path notice also at route begin), also grep the tests that assert on it, in `apps/human-desktop/tests`, `crates/human/*/tests` and `tests/desktop-e2e`. On j43 (2026-10-09), `apps/human-desktop/tests/headless.rs` still asserted `path == None` before a change. It went red on #245's CI after hand-off and had to be fixed by a second supply (j45, 502d7d3a). Grep for the accessor (`.path(&`) and the fake's driver (`path_changed(`), not only the prose.

Related: [[labels-english-supplier]]

*References: labels-english-supplier*

*Observed 2026-10-09 (rust-ui-dev)*
