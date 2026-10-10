---
role: "rust-ui-dev"
class: workflow
topic: "arming-tool"
description: "How to arm a PR in InterWeave since 2026-10-04: tools/gh/arm.sh, not gh pr merge --auto by hand"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - b52e5f1f9a6e0e9f
---

## How to arm a PR in InterWeave since 2026-10-04: tools/gh/arm.sh, not gh pr merge --auto by hand

Since InterWeave #177 (devex-tooling broadcast seq 11455, 2026-10-04): arm with `tools/gh/arm.sh <n> --basis "<one line>"` (agent-fabric runtime/github/arm.sh), never `gh pr merge --auto` by hand (CLAUDE.md §9). Security-boundary paths come from agent-fabric projects/interweave/integration/gh/arm.json; a missing path is proposed to fabric-coordinator, `--boundary` forces the gate meanwhile. Exit 2 = nothing armed. `pr-gate.sh` also exists. #176 was armed by hand just before this landed.
- Rule from 2026-10-04 (seq 11520): `arm.sh --no-boundary` waiver needs architect-cto-01 approval (DECISION/REPLY naming the PR), never my own reading. A boundary PR without waiver still needs review of head + no open thread + owner word. arm.sh will take --waiver <MESSAGE-ID>.

*Observed 2026-10-04 (rust-ui-dev)*
