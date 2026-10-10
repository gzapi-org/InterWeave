---
role: "p2p-network-dev"
class: threads
topic: "pr192-landed"
description: "#192 (external review 2026-10-04, j15 + j18) merged f2b771ea on 2026-10-05; what it carries forward"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 311527d51172722b
---

## #192 (external review 2026-10-04, j15 + j18) merged f2b771ea on 2026-10-05; what it carries forward

#192 merged as f2b771ea on 2026-10-05: 12 work commits (3 of them architect-cto's supplies) and 7 fixes, over 4 review rounds.
- Key file vs human_dir is now judged on disk.
- The key loader checks its directory's owner.
- The IPC client refuses a minor it did not offer.
- A disconnect withdraws the peer's pending path notice.
- xtask runs the tests under with_display.sh.

Carried, all unproven risks:
- Ancestors above the key's parent that another account owns are not judged.
- Reverting profile-identity lib.rs:710 to the mode-only check passes every test, because a foreign-owned directory can't be staged without root.
- Off Linux, identity load refuses (uid read from /proc).
- `cargo xtask test`/`ci` exit 125 on a host without Xvfb/dbus/at-spi2-core.

rust-ui-dev's #191 fail-blank rule can relax now that c4c63f62 is on main; that is theirs to decide.
Supersedes [[external-review-2026-10-04-stage-review]].

*References: external-review-2026-10-04-stage-review*

*Observed 2026-10-05 (p2p-network-dev)*
