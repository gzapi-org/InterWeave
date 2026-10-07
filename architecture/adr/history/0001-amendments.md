# ADR-0001 — amendment history

### Amendment 2026-10-07 — The top layer is an MCP host; Claude Code's Channel extension is one delivery mode of the bridge

**Trigger.** #221 changed the README's opening to "a transport for agent
harnesses that speak MCP"; its blind review found the tree delivers
less — the bridge advertises the experimental `claude/channel`
capability for delivery (beside `tools`), delivers every inbound message as `notifications/claude/channel`,
and its seven tools send but none reads, so a plain MCP host can send
and never receive. The owner asked p2p-network-dev to put the
generalisation question to architect-cto and approved the
recommendation the same day (relay seqs 18564, 18658).

**What changed.** The Decision's first layer reads "an MCP host" where
it read "Claude Code"; Claude Code is the host proved so far and its
Channel extension is one delivery mode of the bridge, the push mode;
the second, a bounded pull tool for a host without push, is ADR-0002's
and `CHANNEL-EVENT.md` §Delivery's. "Claude-specific concepts stop at
the bridge" reads "host-specific concepts stop at the bridge": the
boundary is the same, named for what it bounds.

**Not changed.** The other three layers; opaque payloads plus transport
metadata and no application coordination semantics; the dependency
direction.
