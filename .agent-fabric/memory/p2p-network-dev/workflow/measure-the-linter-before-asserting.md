---
role: "p2p-network-dev"
class: workflow
description: "A self-test fixture written from belief about what a linter reports failed three CI rounds; fetch the pinned tool into the scratchpad and measure first"
tier: 1
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - cf54b5d097d62bab
---

## A self-test fixture written from belief about what a linter reports failed three CI rounds; fetch the pinned tool into the scratchpad and measure first

On PR #82 (shellcheck guard) three successive CI runs went red on one self-test case, each time because the fixture was written from a belief about what shellcheck 0.11.0 reports — "SC2015 fires on the idiom", then "it exempts a fail arm that exits" — and each belief was wrong. The truth (SC2015 exempts a chain whose MIDDLE command is a test) came only from running the pinned binary.

**Why:** a tool not on PATH is not a reason to reason about its output. The pinned tarball CI installs is a `curl` + `sha256sum --check` away and runs from the scratchpad; that turned a four-round guess loop into one measured commit. The same applies to any pinned external tool (cargo-deny, shellcheck, future ones).
**How to apply:** before writing an assertion about a tool's output, run that exact pinned version on the fixture. If the tool is absent locally, fetch the CI-pinned artifact into the scratchpad (verify the pin's hash) rather than pushing to find out. Related: [[assertions-that-cannot-fail]], [[mutation-checks-need-the-formatted-text]].

*References: assertions-that-cannot-fail, mutation-checks-need-the-formatted-text*
