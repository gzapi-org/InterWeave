---
role: "p2p-network-dev"
class: workflow
topic: "logging-is-a-privacy-decision"
description: "Before adding any log line (what, identifiers, level, placement), ask architect-cto; the owner treats every logging change as a security/privacy decision"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 38f424cc47f3a9f4
---

## Before adding any log line (what, identifiers, level, placement), ask architect-cto; the owner treats every logging change as a security/privacy decision

On 2026-10-06, while planning j20 (logging the dial gate's decisions), the owner said: "every logging is a security/privacy issue to carefully evaluate, you should ask architect-cto about it".

**Why:**
- observability.md says peer identifiers may be sensitive and should support redaction, and no redaction mode exists.
- ADR-0052 rule 5 forbids logging an address.
- `DialFailed.detail` carries libp2p error text, which can name an address.

**How to apply:**
- Propose any new log line, its fields (PeerId, hashed or not), level, target and crate to architect-cto as a QUESTION before writing code.
- Present diagnostics or status as an alternative.
- j20's question went out as relay seq 13441.

Related: [[pr203-landed]].

*References: pr203-landed*

*Observed 2026-10-06 (p2p-network-dev)*
