---
role: "p2p-network-dev"
class: solution
description: "Owner ruled 2026-09-07 that the relay/autonat/dcutr constructor ships gated off and ClassGated<B> lands first, reversing the approved phase order"
tier: 2
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - 36681bfbfa149883
---

## Owner ruled 2026-09-07 that the relay/autonat/dcutr constructor ships gated off and ClassGated<B> lands first, reversing the approved phase order

Stage 11's connectivity behaviours must be **constructed gated off**, and
the `ClassGated<B>` exposure fix (session plan's phase 2) lands **before**
the configuration that would construct them.

**Why:** `architecture/config/config.schema.yaml` makes
`transport.connectivity.relay.client.enabled` a `literal[true]`. So the
commit that teaches `profile-config` to model that section is the commit
where an ordinary profile constructs a relay client — and that is the first
`ConnectivityInfrastructureOnly` connection this project has ever been able
to establish. `SubstrateBehaviour` installs `direct`, `broadcast`,
`endpoints` and `kad` on every connection uniformly, so such a peer could
open all four substreams and be refused only after the request is parsed
and accounted. That exposure is exactly what `ClassGated<B>` closes.

**How to apply:** order is features-on (PR #76) → `ClassGated<B>` →
connectivity config. When the config half lands, the behaviours are
constructed from an explicit gate rather than from
`relay.client.enabled`, whatever the schema's literal says. This REVERSES
the plan's approved 1b-before-2 order; the owner ruled it, so do not
re-derive the old order from the phase numbers.

Related: [[stage-11-progress]], [[stage-closure-needs-approval]].

*References: stage-11-progress, stage-closure-needs-approval*
