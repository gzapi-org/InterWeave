---
role: "p2p-network-dev"
class: workflow
topic: "background-verify-reads-the-live-tree"
description: a backgrounded full verify compiles the working tree as it goes; editing during it certifies a mixed tree — stop it or wait
tier: 1
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - b1dac928199699e7
---

## a backgrounded full verify compiles the working tree as it goes; editing during it certifies a mixed tree — stop it or wait

A full verification run in the background (fmt, clippy, workspace tests,
xtask checks under `fabric-lease stack-up`) reads the working tree crate by
crate as it compiles, so an edit made while it runs yields a green result for
a tree that never existed. On #112 (2026-09-25) a fix was written into
`iface.rs` and `mdns_bounds.rs` mid-run; the run was killed and restarted on
the committed head.

**Why:** the verify is what licenses the push, and a result over a mixed tree
licenses nothing.

**How to apply:** while a background verify runs, do only non-tree work
(messages, briefs, PR replies, memory). If a fix is needed, stop the run
first, commit the fix, record `git rev-parse HEAD` beside the run, and restart.
Related: [[push-before-describing-pr-state]].

*References: push-before-describing-pr-state*

*Observed 2026-09-25 (p2p-network-dev)*
