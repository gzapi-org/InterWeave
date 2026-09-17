---
role: "p2p-network-dev"
class: solution
description: "DONE 2026-08-28 (PR #53): CLAUDE.md §9 lifecycle moved to the pr-lifecycle skill, 28% cut. Seven review findings, all one shape — keep this before splitting CLAUDE.md again"
tier: 2
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - ac4447a8fc8be846
---

## DONE 2026-08-28 (PR #53): CLAUDE.md §9 lifecycle moved to the pr-lifecycle skill, 28% cut. Seven review findings, all one shape — keep this before splitting CLAUDE.md again

**Done 2026-08-28, PR #53 merged as `695c978`.** CLAUDE.md 8,345 → 6,053
words (28%, below the 40% target); `.claude/skills/pr-lifecycle/SKILL.md`
holds phases 2–6, tooling, second-PR rules, concurrent sessions, and
subagent dispatch. §4 and §5 untouched, as agreed.

**Why it took 5 review rounds and 7 findings for a docs-only PR** — more
than the Stage 7 implementation PR — and what to do differently next time:

Six findings were the same defect: **a rule that must be in context BEFORE
a step was filed in a skill whose load trigger fires only AFTER it.** The
`reset --hard` guard; the open-PR file-set check; the agent-side worktree
contract; "dispatch is a load trigger"; and the PR-scope narrowing. The
seventh was a rule deleted outright (`opus`/`fable` forbidden) because a
hook *appeared* to state it — the hook only ASKS on premium, and never
fires inside a `Workflow`.

**How to apply:** the split axis is *when must this be in context*, never
topic. Every time a rule got misfiled, the pull was topical ("subagent
rules are lifecycle rules, so they go with the lifecycle") and the timing
question was answered by the filing decision instead of the other way
round. Before moving any rule into a skill, ask: what is the FIRST action
this rule constrains, and does the skill's trigger list name something
that happens before it? If not, the rule stays, or the trigger grows.

And: **a hook that denies is not a rule that states.** A compliant call
sees nothing from it. Anything the reader must know on the compliant path
has to live in prose that loads.

**Why:** the trimmed file is only cheaper if nothing load-bearing left it.
Every one of the seven was caught in review, none by re-reading the diff
— the file that was not edited is not in the diff.

See [[stage6-review-retrospective]] for the earlier evidence behind the
§4/§5 carve-outs.

*References: stage6-review-retrospective*
