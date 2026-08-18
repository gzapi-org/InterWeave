# core

Human client domain state: the ADR-0044 message retention state machine.

**Current status:** Stage 2, active workspace member. Decides `Durability`; stores nothing.

## Read the dependency list first

`serde` and nothing else. **No** dependency on `chat-protocol`, `transport-api`, or any store.

That is the design, not an accident of scope. The retention decision cannot see message content, a sender, an EndpointId, a contact label, or a notification payload — so "a remote sender cannot request or force retention" is not a rule this crate *follows*, it is a rule the dependency graph makes **unstateable**. There is no field for a remote party to set because there is no remote data in scope at all.

`InboundMessage::keep()` takes no argument beyond `&mut self`, for the same reason.

## Exactly three durable states

`outbound pending`, `inbound unread`, `inbound kept`. Everything else is `Durability::Remove` — an instruction to delete, not a suggestion.

`tests/retention_invariants.rs` walks **every reachable state** and asserts the durable set is exactly those three, rather than checking the states that came to mind. It also asserts no path reaches `Kept` without passing through read.

## Transitions with reasons

- **The durable pending record is created before transport is invoked.** Sending first would lose the message if the process died between the call and the record.
- **A transient failure leaves outbound pending.** Deleting it would lose content the user believes they sent — crash survival is the entire reason pending is durable.
- **`TerminalCause` is diagnostic only.** Four ways to stop mattering, one retention answer; if they diverged, "transport-terminal" would stop being a single concept. `Published` in particular is terminal because broadcast has no per-recipient acknowledgement — a UI must not call it "delivered".
- **`Keep` is refused before read.** Otherwise a notification action could make durable something the human never looked at.
- **A read-unkept message can be kept while still in memory**, and not after the session ends. The content was deleted when it was read; that is the design, not a limitation.
- **Removing `Keep` deletes immediately**, not at some later cleanup.
- **Backup carries inbound content only.** All outbound is excluded so a restored or second device cannot become an implicit replay or delayed-send source.

## Storage degradation is a real state

If the store cannot durably accept unread content, `StorageHealth::Degraded` requires releasing the human endpoint and suspending local broadcast joins. Staying leased would accept messages the receiver is about to lose, which would make `AcceptedV2` a claim it cannot honour. This is an application reaction and alters no transport semantics.
