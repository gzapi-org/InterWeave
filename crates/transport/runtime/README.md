# runtime

Backend-neutral orchestration, endpoint/subscription registries, direct admission, local-session coordination, and normalized events.

**Current status:** Stage 2, active workspace member — **pure modules only**. No socket, no Swarm, no async runtime, no clock. That exclusion is what lets these rules be tested by enumeration rather than orchestration.

## `endpoint_registry`

Who owns which endpoint, and where a directed message lands.

**The rule the module exists for:** ordinary remote messages can never create, steal, transfer, or enable a lease. Every mutating operation takes a `LocalSessionId`, and there is no path from an inbound message to one. `resolve_inbound` — the only thing an inbound message drives — takes `&self` and therefore cannot mutate anything. The guarantee is in the signatures, not in a comment.

**One endpoint, never fan-out.** An omitted destination means the configured default. `resolve_inbound` returns a single `EndpointId`, so the type cannot express fan-out even if someone wanted it to.

**Local precision, coarse wire.** `ResolveFailure` distinguishes unknown, disabled, offline, default-missing, and policy-denied — useful in a diagnostic. `to_wire()` is a `const fn` returning one value, so there is no mapping table to get wrong and a future variant cannot acquire its own wire code by being added. Distinguishing these remotely would make the protocol an endpoint-existence oracle (ADR-0030).

Ordering matters and is tested: endpoint policy is evaluated **before** the lease is consulted, so the presence of a lease is not observable through which local error surfaced.

Other decisions worth naming:

- **A duplicate claim is refused, not granted by displacement.** Taking a lease from a live session would silently redirect its traffic to whoever asked most recently.
- **Disabling an endpoint revokes its lease** and returns the ended epoch. Leaving it would have a session believing it owns a route that no longer accepts traffic.
- **An unleased endpoint drops, it does not buffer.** `EndpointOffline` creates no queue — there is no mailbox here to accumulate one (ADR-0020).
- **Outbound authorization delegates to `trust-api`** rather than re-implementing the intersection, so "narrow but never widen" has one implementation.
