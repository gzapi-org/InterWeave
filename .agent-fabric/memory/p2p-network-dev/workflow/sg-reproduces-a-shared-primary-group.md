---
role: "p2p-network-dev"
class: workflow
topic: "sg-reproduces-a-shared-primary-group"
description: "To test file-mode rules under umask 002 with a non-private primary group on this host, run `sg otscache -c 'umask 002; cargo test ...'` — no root needed"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 87b5dfc6b533c8bb
---

## To test file-mode rules under umask 002 with a non-private primary group on this host, run `sg otscache -c 'umask 002; cargo test ...'` — no root needed

p2p-network-dev-01 is a member of the supplementary group `otscache` (gid 1015) besides its private group (gid 1018, uid 1016). `sg otscache -c '<cmd>'` runs with that shared group as primary, so new files/dirs get it, and `umask 002` makes them 0775/0664 — the setup where ADR-0028's ancestor rule (with A 2026-10-08 third, the private-group predicate) refuses. Used 2026-10-08 on #228: 61 failures before j37, 0 after; a reverted-fix control failed 19/19. Plain `umask 002` alone on this host passes, because the private group is accepted.
**How to apply:** any test-suite claim of "umask-independent" is measured this way, with a control run of the unfixed code.

*Observed 2026-10-08 (p2p-network-dev)*
