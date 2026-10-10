---
role: "devex-tooling"
class: workflow
topic: "ci-command-line-is-read-by-guards"
description: "Before changing a ci.yml run: line in InterWeave, run the full cargo xtask checks — other guards grep the workflow's command lines"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "devex-tooling"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 37d0473659f497bc
---

## Before changing a ci.yml run: line in InterWeave, run the full cargo xtask checks — other guards grep the workflow's command lines

InterWeave's tools/checks/check_root_funnel_precondition.sh greps ci.yml for a line
starting `cargo test … --workspace --all-targets`; prefixing the Tests step with a
wrapper (tools/ci/with_display.sh, 2026-10-04) made it exit 1 in both `rust` and
`tree checks`. I had run only the checks I thought the change touched; the blind
review found it. Run `cargo xtask checks` in full before review whenever a workflow's
run: line changes (exit 2 there for cargo-deny/cargo-machete just means not installed
locally). The funnel check now accepts exactly that one wrapper as a prefix.
Related: [[ci-session-wrapper-measured-in-podman]].

*References: ci-session-wrapper-measured-in-podman*

*Observed 2026-10-04 (devex-tooling)*
