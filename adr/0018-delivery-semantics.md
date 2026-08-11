# Realtime best-effort delivery only

**Status:** Accepted

## Context

GossipSub and direct streams cannot honestly provide stronger end-to-end guarantees without durable state/ack protocols that are explicitly absent.

## Decision

Define v1 as best effort. After local deduplication, a runtime tries to present each accepted message at most once to each local consumer. There is no global ordering, exactly-once, durable queue, or offline mailbox. Direct `Accepted` is transport acceptance only.

## Alternatives considered

Guaranteed delivery claim; exactly-once; total order; durable acknowledgement workflow.

## Consequences

Applications needing durability implement it above this transport or use a future backend with explicit capabilities.

## Security implications

Replay/duplicates are mitigated only within bounded windows. Security-sensitive applications cannot treat message absence as proof that no event occurred.

## Operational implications

Diagnostics expose failures/drops/empty meshes; operators must not read success counters as recipient acknowledgements.

## Implementation implications

Tool/result wording and docs use precise acceptance language. Capability flags report no durability.

## Revisit conditions

Revisit only with a designed durable backend/protocol and capability negotiation that does not misrepresent other backends.
