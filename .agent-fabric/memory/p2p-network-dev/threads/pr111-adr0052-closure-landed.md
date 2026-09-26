---
role: "p2p-network-dev"
class: threads
topic: "pr111-adr0052-closure-landed"
description: "PR #111 (mDNS mechanism, DNS transport, ADR-0052 full enforcement) merged fa3eab8 on 2026-09-25; what it left open and whose each item is"
tier: 2
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 038df84d98c257da
---

## PR #111 (mDNS mechanism, DNS transport, ADR-0052 full enforcement) merged fa3eab8 on 2026-09-25; what it left open and whose each item is

#111 merged 2026-09-25 10:25 UTC as fa3eab8 (head 8dd3f4d), armed on the
owner's "merge 111 as soon as you can" after the automated reviewer
passed 8dd3f4d clean and every review thread was answered and resolved.
The blind re-review of 30658e6..8dd3f4d was interrupted by a session end
and resumed after the merge; any finding it reports is a follow-up PR.

Left open, each with its owner:
- **mDNS crate store bound** (ADR/DISCOVERY-CONFORMANCE Decision
  2026-09-25, D-a): mine. Vendor and cap libp2p-mdns under ADR-0051's
  route with its own decision record, flood measurement FIRST as a
  committed test, and the vendored crate's bind/join/send/receive
  failures mapped to degraded (item 9). All before the mdns deadline
  reads MET; not before.
- **A peer-door learn command for Stage 12's composer** (rule 9's "one
  door still owed", plan s15): mine, at Stage 12. The conformance test's
  add_address use is a test topology, not the pattern.
- **Review-dispatch hook conflict** (fabric says fable, the project hook
  exempts only opus): devex-tooling / fabric-coordinator; not yet raised.
- **Host path in published history**: the three reproduction logs were
  redacted at d569598, but earlier #111 commits still carry the path;
  removing it needs a force-push the owner has not asked for.

*Observed 2026-09-25 (p2p-network-dev)*
