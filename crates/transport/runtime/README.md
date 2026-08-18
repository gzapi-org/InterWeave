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

## `fingerprint`

`DirectContentFingerprintV1` — a pure function over content and nothing else. No endpoint, no message ID, and crucially **no timestamp**: a fingerprint covering `sent_at_ms` would make every retry look like different content, which is the opposite of what dedup needs.

Absence uses `media_present = 0` and carries no length field at all, which keeps an absent media type distinct from any present one — and is why the empty string is *rejected* rather than encoded. Two spellings of "no media type" would mean one message with two content identities.

`tests/frozen_fingerprints.rs` reproduces all seven vectors in `fixtures/direct-v2/`. Until now nothing in Rust consumed them, so two implementations could have disagreed with only the Python one checked.

## `dedup`

Bounded ephemeral duplicate suppression (ADR-0019): 10,000 entries, 5-minute TTL, **persistence prohibited** — there is deliberately no save, load, or export, since a durable ledger would turn at-most-once-within-a-window into a promise the transport does not make.

**The key holds what the sender addressed, not where it landed.** `DestinationSelector` is `Explicit(id)` or `Default`; the *resolved* endpoint lives in the record as an outcome. That one choice is what makes a retry stable across configuration change: were the resolved endpoint in the key, an operator repointing `default_direct_endpoint` between a message and its retry would produce a different key, and the retry would be delivered a second time to a different local client. There is a test named for exactly that.

Other properties with reasons:

- **Broadcast and direct are separate variants**, not one struct with optional fields — an `Option<EndpointId>` shared between them would let a broadcast key carry endpoint data that ADR-0030 keeps out of broadcast.
- **`source_endpoint` is in the direct key**, because two endpoints under one PeerId may independently choose the same 128 bits, and collapsing them would silently drop the second message.
- **Asking does not create an entry.** A caller about to reject a message must not have cached it by enquiring.
- **Only positive outcomes are recorded.** Caching a rejection would keep refusing a message whose endpoint was briefly offline, long after the route recovered.
- **Time is a parameter.** Every expiring method takes `now_ms`, so TTL is tested by enumeration rather than by sleeping.

`ReservationMap` closes the concurrent-duplicate race: first caller owns, matching duplicates wait and share the outcome, differing content conflicts immediately. Per-peer budget is checked *before* the global one, so one noisy peer cannot consume the whole allowance and refuse everyone else; both limits are clamped to their ceilings rather than trusted from configuration.
