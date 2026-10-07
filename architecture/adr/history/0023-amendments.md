# ADR-0023 — amendment history

### Amendment 2026-10-07 — `receive` is the eighth tool, in pull delivery mode only

**Trigger.** ADR-0002 A 2026-10-07 gave the bridge a pull delivery mode
for a host that speaks plain MCP: inbound events reach such a host only
through a tool, since it has no Channel notification. The supplier
review of that amendment found the tool surface's own record, this ADR,
still said seven tools and "the seven-tool surface remains compact".

**What changed.** The Decision lists `receive(max?)` — pull mode only:
it takes the direct messages and broadcasts the bridge holds for the
session, in the order taken, never waits, and answers one structured
result `{events, remaining, paused}`; it is absent in push mode, where
the Channel notification delivers. `status` gains, in pull mode, the
pull queue's depth, whether the drain is paused and since when; while
the drain is paused, `send`, `reply`, `broadcast`, `join` and `leave`
refuse at once with a bridge-local error naming the state ("the pull
queue is full: call receive first"), never a stall and never
`Overloaded`, because a call awaited behind the paused drain would hold
the bridge's loop and the host's next `receive` with it. The Consequences say
seven tools in push mode, eight in pull mode, the one addition being the
delivery itself. Nothing administrative is exposed by it.

**Not changed.** The seven tools and their semantics; the bridge-owned
lease as the source route; the exclusions (trust, endpoints, identity,
daemon control, raw internals); `peer_endpoints` not a Claude tool.
