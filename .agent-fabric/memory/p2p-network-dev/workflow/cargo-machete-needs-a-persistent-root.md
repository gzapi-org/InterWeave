---
role: "p2p-network-dev"
class: workflow
topic: "cargo-machete-needs-a-persistent-root"
description: "The session scratchpad is wiped on restart, so a cargo-machete installed there vanishes and xtask ci fails check_unused_dependencies with exit 2"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 641d631bc47b1aae
---

## The session scratchpad is wiped on restart, so a cargo-machete installed there vanishes and xtask ci fails check_unused_dependencies with exit 2

`cargo xtask ci` runs `tools/checks/check_unused_dependencies.sh`, which
needs `cargo-machete` 0.9.2 (CI's pin) on PATH and exits 2 when it is
missing. The host has none installed. Installed into the session
scratchpad it disappeared twice on 2026-09-29, once each time the
session restarted, and each time a full CI run came back red on that
guard alone (#147).

**Why:** the scratchpad is per-session and cleaned. A tool the verify
loop needs every run belongs outside it.

**How to apply:** install it once to a persistent root outside the
tree, `cargo install cargo-machete --locked --version 0.9.2 --root
~/.local/share/cargo-tools -j 2`, and prefix that `bin` to PATH for
`cargo xtask ci`. Before a CI run, check with `command -v cargo-machete`
and never start the full run without it. See
[[host-cargo-is-system-1-98-no-rustup]].

*References: host-cargo-is-system-1-98-no-rustup*

*Observed 2026-09-29 (p2p-network-dev)*
