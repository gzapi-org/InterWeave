---
role: "p2p-network-dev"
class: workflow
topic: "pkill-f-matches-its-own-shell"
description: "pkill -f <pattern> inside a Bash tool call kills the tool's own shell (exit 144), since the shell's command line contains the pattern"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 79f29a0957fc33ab
---

## pkill -f <pattern> inside a Bash tool call kills the tool's own shell (exit 144), since the shell's command line contains the pattern

2026-10-04, Stage 15 R2. `pkill -f "scripted_server-"; git checkout ...` in one Bash call ended with exit 144 and ran nothing after it. The tool's own `bash -c` command line contains the pattern, so pkill matched and killed it. It also left a planted mutation in the tree, which the next status check caught.

**Why:** the harness wraps every command in `bash -c '<the whole line>'`, so any `-f` pattern written on that line matches that shell.

**How to apply:**
- Find the process first with `pgrep -fa`, then kill it by PID in a separate call.
- Stop a background task with TaskStop rather than pkill.
- Bound every mutation run with `timeout 300 cargo test ...`, so a hang ends by itself.
- Bound a scripted server's `accept()` with a timeout, so a client that never connects fails the test instead of hanging it.

*Observed 2026-10-04 (p2p-network-dev)*
