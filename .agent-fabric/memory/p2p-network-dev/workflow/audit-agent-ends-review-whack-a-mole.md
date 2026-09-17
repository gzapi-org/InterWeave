---
role: "p2p-network-dev"
class: workflow
description: "When review rounds keep finding the same invariant at neighbouring sites, the user wants a fresh uncontexted agent to audit the whole class so one pass ends it — proven on PR #56 round 32"
tier: 1
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - 9ebfcc9dc29c0987
---

## When review rounds keep finding the same invariant at neighbouring sites, the user wants a fresh uncontexted agent to audit the whole class so one pass ends it — proven on PR #56 round 32

On PR #56 (Stage 9 discovery), 32 review rounds each fixed the invariant
at the named site and the next round found it at a neighbour. At round 32
the user interrupted: "To many iteration here with this PR. Please
dispatch a fable agent wothout sharing context so he can do a full
analyze of this series of issue so you can fix all the situation in one
run."

**Why:** per-site fixing converges too slowly when the defect class is
architectural. A fresh agent with a self-contained brief (invariants, the
open findings, the history pattern, "assume the reviewer's next round
exists and find it first") is unbiased by the incremental framing and can
judge the DESIGN — the audit's verdict was to invert the storage so the
threshold no longer lived in the thing being removed, which one commit
then killed structurally (one mutation failed seven door-tests at once).

**How to apply:** after ~3 rounds of same-invariant-neighbouring-site
findings, stop patching and propose the audit dispatch instead of
waiting to be told. The dispatch is opt-in ([[claude-md-skill-split-after-stage-7]]
rules: `isolation: "worktree"`, explicit `model`; `fable`/`opus` only
because the user named the tier). Brief must be self-contained; agent
runs no git; baseRef is `head` so commit first. Validate its design by
prototype before adopting, and adapt its defect tests to the new
mechanism rather than pasting them (they may assert the OLD mechanism's
intermediate state). See [[stage8-review-retrospective]] for the earlier
per-round pattern this replaces.

**ONCE PER CLASS, THEN GO BACK TO NARROW.** On #85/#86 the audit SHAPE was
kept as the per-round default — every later brief still asked for full-range
re-verification — and the owner caught it from the token counts: ~170k per
review on comment-only diffs. The wide pass is an escalation that ends a
class; the round after it is scoped to the diff again. See
[[scope-review-briefs-to-the-diff]].

*References: claude-md-skill-split-after-stage-7, scope-review-briefs-to-the-diff, stage8-review-retrospective*
