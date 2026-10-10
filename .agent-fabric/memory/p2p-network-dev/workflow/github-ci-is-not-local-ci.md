---
role: "p2p-network-dev"
class: workflow
topic: "github-ci-is-not-local-ci"
description: "Local `cargo xtask ci` passing does not mean GitHub's rust job passes: the Fedora host has system -devel libraries the Ubuntu runner lacks; read the PR's checks after EVERY push"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - e9c7a1c5dde88fce
---

## Local `cargo xtask ci` passing does not mean GitHub's rust job passes: the Fedora host has system -devel libraries the Ubuntu runner lacks; read the PR's checks after EVERY push

On #170 (Stage 14 batch 8, ui-slint, 2026-10-03), every GitHub `rust` run failed from the first push (8b14131b) to 0b46e408, five heads in a row. Local `cargo xtask ci` passed each time, and I did not look at GitHub until the owner said "next" before arming.

The cause was that slint's font stack (fontique) pulls in yeslogic-fontconfig-sys. Its build script needs fontconfig.pc: Fedora has fontconfig-devel, and the runner does not.

Fix (d700cedb): name i-slint-common at the toolkit's exact pin, with `fontconfig-dlopen`, which slint does not forward. Fontconfig is then opened at run time.

To reproduce the runner locally, set PKG_CONFIG_LIBDIR=/nonexistent PKG_CONFIG_PATH=/nonexistent. That is both the control and the proof.

**Why:** the local host is not the CI image. A new native-linking dependency passes locally and fails there, and the result sits unread while review rounds go on.

**How to apply:**
- After every push, run `gh pr view <n> --json statusCheckRollup` and read each conclusion.
- Before admitting a dependency, build with pkg-config hidden to see what it links natively.

Related: [[arm-only-on-concluded-checks]].

*References: arm-only-on-concluded-checks*

*Observed 2026-10-02 (p2p-network-dev)*
