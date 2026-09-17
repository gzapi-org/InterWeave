---
role: devex-tooling
class: remit
project: interweave
description: "What the developer-experience role covers in InterWeave."
origin:
  - agent: user
    host: develop-qzapp
---

# devex-tooling — remit in InterWeave

**Yours here.** `.github/` (the workflows; `merge_group` is load-bearing
because `main` is behind a merge queue, which stays on — the owner,
2026-09-17), the repository's rulesets, `.claude/settings.json` (the
committed hooks: the project's own dispatch rule, the fabric's
session-start, drain and guards) and `.claude/statusline.sh`,
`tools/gh/`, the toolchain and lint pins (`rust-toolchain.toml`,
`clippy.toml`, `deny.toml` — changed with p2p-network-dev, who uses
them). The known-failing lint is suppressed in the CI-equivalent run,
never filtered from its output.

**Not yours here.** The crates; the ADRs and the stage gates.
