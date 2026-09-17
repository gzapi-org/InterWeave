---
role: "p2p-network-dev"
class: workflow
description: "When scripting an insertion into Rust source, anchor on the END of the preceding item — anchoring on the following item's declaration splits it from its doc comment"
tier: 1
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - cc1a62dc2c564f57
---

## When scripting an insertion into Rust source, anchor on the END of the preceding item — anchoring on the following item's declaration splits it from its doc comment

Scripted edits that insert a new item (enum variant, struct, fn, const)
**must anchor on the end of the item BEFORE it**, never on the
declaration line of the item after it.

**Why:** a Rust item's real start is its `///` doc block, not its
declaration. Anchoring on `PermissionsTooOpen {` or
`pub struct PeerCacheDiscovery {` inserts *between* the doc and the thing
it documents — silently orphaning the doc onto the new item and leaving
the old one undocumented. This happened four times in one session
(`StoreError::PermissionsTooOpen`, `PeerCacheDiscovery`,
`split_peer_multiaddr`, `remember_watermark`), each time costing a
clippy `missing-docs` failure and a rework round.

**How to apply:**
- Anchor on the previous item's closing `},` / `}` / `;` and append after
  it. If the new item must come first in the list, anchor on the opening
  `{` of the enclosing block.
- After any scripted insert near documented items, run
  `cargo clippy -p <crate> --all-targets -- -D warnings` before
  committing — `missing-docs` catches this, so the only cost is rework,
  and only if the check runs first. Do NOT commit on the assumption it
  is fine.
- The same trap applies to `cargo fmt` reflowing a comment you anchored
  on: a pattern that matched when written may not match after `fmt`, so a
  scripted edit that asserts its pattern count is how you find out.
  See [[verify-subagent-coverage-not-just-findings]] for the general
  shape — a scripted change that silently does nothing reads as success.

*References: verify-subagent-coverage-not-just-findings*
