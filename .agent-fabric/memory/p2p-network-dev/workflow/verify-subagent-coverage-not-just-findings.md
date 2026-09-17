---
role: "p2p-network-dev"
class: workflow
description: "A subagent's 'all N findings covered' is not evidence N was right — check the union of results against the input list before acting"
tier: 1
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - bc175def5cf01b18
---

## A subagent's 'all N findings covered' is not evidence N was right — check the union of results against the input list before acting

When fanning out verifier agents over a numbered list (review findings,
files, test cases), **check each agent's findings AND check the union of
all agents against the original list.** They are different questions and
only the first one feels like verification.

**What happened (2026-08-27, PR #49).** Four read-only agents split a
26-item review (S0-1 … S6-14). Each report was scrutinised against its
file:line evidence. The agent holding the Stage 6 block closed with
"sufficient evidence for all 13 findings" and stopped at S6-13 — its
count only looked right because S6-5 had been folded into another
agent's item. **S6-14 was assigned to nobody and had no verdict**, and
my PR body presented 25 rows as full coverage. The user caught it, not
me. One command would have found it:

```
grep -oE '\bS[0-9]-[0-9]{1,2}\b' <review> | sort -u
```

**Why:** the aggregate claim is the one no one owns. Each agent verifies
its own slice and reports honestly about that slice; nothing in the
fan-out checks that the slices tile the input.

**How to apply:** before dispatching, enumerate the input IDs and write
the partition down. After collecting, diff the union of reported IDs
against that enumeration and state the count in the summary. Treat a
subagent's own "all N" as a claim to test, exactly like
[[stage6-review-retrospective]]'s rule that a test existing is not
evidence it asserts the thing.

The S6-14 outcome happened to be "no action — already carried as a
Stage 8 inherited obligation", so nothing shipped wrong. That is luck,
not a reason to skip the check.

*References: stage6-review-retrospective*
