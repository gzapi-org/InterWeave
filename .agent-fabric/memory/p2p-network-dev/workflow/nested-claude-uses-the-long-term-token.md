---
role: "p2p-network-dev"
class: workflow
topic: "nested-claude-uses-the-long-term-token"
description: "A nested Claude Code session (host runs, spikes) authenticates with this login's long-term token, never an interactive /login (owner, 2026-10-06)"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - e733657618f1d477
---

## A nested Claude Code session (host runs, spikes) authenticates with this login's long-term token, never an interactive /login (owner, 2026-10-06)

A Claude Code session that a driver starts (the Stage 16 host run, SPIKE-001's drivers) authenticates with this login's long-term token: `CLAUDE_CODE_OAUTH_TOKEN` (a setup-token) from `~/.config/agent-fabric/secrets.env`, the file the shell profile sources.
- Pass only that one variable.
- Record only its name (in run.json's env_passed), never its value.
- Never propose an interactive `/login`.

**Why:** the owner, 2026-10-06, after I offered `/login`: "why you need to do something that we never should do again? use the long term token instead". The isolated child environment carries no credential, so without the token the nested session answers "Login expired".

**How to apply:** `spikes/spike-001/s16_run.py`'s `long_term_token()` is the pattern. When the auto-mode classifier refuses passing a credential, the owner decides; do not route around the refusal.

*Observed 2026-10-06 (p2p-network-dev)*
