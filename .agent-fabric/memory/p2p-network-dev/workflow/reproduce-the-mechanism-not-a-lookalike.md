---
role: "p2p-network-dev"
class: workflow
description: "A fixture that resembles a reviewer's scenario can pass vacuously; reproduce the exact mechanism they named and prove it with a mutation"
tier: 1
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - e4e71a7b97b00b32
---

## A fixture that resembles a reviewer's scenario can pass vacuously; reproduce the exact mechanism they named and prove it with a mutation

**When a reviewer names a MECHANISM, the fixture has to reproduce that
mechanism, not something that looks like it.**

On InterWeave PR #85 a reviewer found that the vendored-advisory guard
skipped any local package already in `workspace_members`, because cargo
auto-promotes path dependencies to members — so a crate vendored outside
`third_party/` was invisible to all three of the guard's sources and it
exited 0 with a vulnerable crate compiled in.

I wrote a fixture with a single-package manifest and a path dependency.
It passed. The mutation check is what caught it: restoring the old logic
failed two *other* assertions and left mine green.

The reason: **a single-package manifest does not promote its path
dependencies.** Measured — `workspace_members` held the root package
only. A workspace ROOT does promote them, even ones `members` does not
list, and that is the real repository's layout. Rebuilt with an explicit
`[workspace]` table, the mutation produced the predicted failure exactly.

**How to apply.** Measure the mechanism before building the fixture around
it: one `cargo metadata` call would have told me which manifest shapes
promote. Then mutation-check, and read WHICH assertions failed rather than
just that the suite went red — a mutation killing the wrong test is the
signal that the fixture is not the scenario.

Related: [[mutation-checks-need-the-formatted-text]],
[[measure-the-linter-before-asserting]], [[assertions-that-cannot-fail]].

*References: assertions-that-cannot-fail, measure-the-linter-before-asserting, mutation-checks-need-the-formatted-text*
