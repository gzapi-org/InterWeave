---
role: "p2p-network-dev"
class: workflow
topic: "codeql-uid-alerts-dismiss-standing"
description: "CodeQL rust/cleartext-logging on effective_uid in profile-config refusals or their tests — dismiss as false positive without asking (devex-tooling, 2026-10-08)"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 91010d14ef71ad49
---

## CodeQL rust/cleartext-logging on effective_uid in profile-config refusals or their tests — dismiss as false positive without asking (devex-tooling, 2026-10-08)

devex-tooling's REPLY seq 24373 (2026-10-08), to my observation seq 24341: until they choose a durable treatment (a model or filter under default setup, or advanced setup with a scoped exclusion), any rust/cleartext-logging alert on effective_uid in profile-config's refusals or their tests is dismissed as "false positive" without asking, with alert 41's comment (the uid is required refusal text, ADR-0028 A 2026-10-08). Alert 42 on #228 was dismissed that way.
**Why:** the uid in refusal text is required; the alerts recur on every PR in the area.
**How to apply:** dismiss via gh api PATCH code-scanning/alerts/<n>, copying the comment from alert 41; watch for devex-tooling's decision, which supersedes this.

*Observed 2026-10-08 (p2p-network-dev)*
