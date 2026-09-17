---
role: "p2p-network-dev"
class: solution
description: "If the opus review dispatch prompts for authorization, settings.json is fine — the session holds a stale in-memory hook; restart, don't edit"
tier: 2
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - be226d5b3c962a00
---

## If the opus review dispatch prompts for authorization, settings.json is fine — the session holds a stale in-memory hook; restart, don't edit

When an `Agent` dispatch named `review …` on `model: "opus"` triggers the
premium-model confirmation prompt, **do not edit `.claude/settings.json`.**
The exemption is already there and correct as of commit `61e46cc`
(2026-08-31): the hook allows silently when `model == "opus"` AND the
lowercased `description` matches `^(re-?)?review[ :]`.

**How to tell in one look:** the on-disk prompt text contains
"— EXCEPT a code review, which is standing on opus and passes without
asking" and ends "Approve only if you asked for this tier." A prompt
ending "Approve only if you did." with no EXCEPT clause is a *different
string*, so it came from an older copy — the session read settings.json at
start, before that commit landed.

**Verify rather than assume**, by piping the literal payload through the
hook's own jq command:

```
CMD=$(python3 -c "import json;d=json.load(open('.claude/settings.json'));print(d['hooks']['PreToolUse'][0]['hooks'][0]['command'])")
echo '{"tool_input":{"description":"review PR 72","model":"opus"}}' | eval "$CMD"
```

Empty output = allowed silently. `Address review feedback` must come back
DENY — the exemption requires `review` at the START plus the space/colon,
because matching `review` anywhere once let a WRITING dispatch through.

**Why:** hooks load at session start, so a long-running or `--resume`d
session keeps the version it booted with. Editing the file cannot fix the
running session, and a redundant edit looks like a fix while changing
nothing. The remedy is a session restart.

**How to apply:** diagnosed twice in one session already. Test the hook,
show the message-text mismatch, say a restart is the fix, approve the
prompt and continue — the dispatch itself is correct.

Related: [[reviewer-declined-dispatch-opus]]

*References: reviewer-declined-dispatch-opus*
