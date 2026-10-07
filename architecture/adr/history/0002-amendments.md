# ADR-0002 — amendment history

### Amendment 2026-10-07 — Two delivery modes, one bridge: the Channel push and a bounded pull tool for a plain MCP host

**Trigger.** The owner approved generalising the bridge to any MCP host
(ADR-0001 A 2026-10-07). p2p-network-dev read the five-point design
against the tree before building and found two points that did not
hold: the mode cannot be read from the host's capabilities, because a
host's `initialize` declares the capabilities `roots` and `elicitation`
only — in every SPIKE-001 recording of Claude Code 2.1.285 — and its
`clientInfo.name` is an unverified product string; and the pull mode cannot be `events(max)` with no second
queue, because the bridge must drain its IPC session continuously or be
closed as wedged (`LOCAL-IPC.md` §Push events, `crates/local/ipc-client`
README). Both were ruled as they proposed, with one tightening each
(relay seq 18691).

**Ruled.** The Claude Channel contract is the bridge's push mode; pull
mode is one bounded tool, `receive(max)`, over `LOCAL-CLIENT.md` §2's
`events(max)`, through one bounded pull queue of direct messages and
broadcasts — session notices stay bridge state and `status` in both
modes, as the push path has always consumed them — bounded at the
session's granted `event_queue`. The bridge drops nothing it took: a full
queue pauses the drain. The second review round found what the pause
leads to on an idle host and had it stated: the pipeline behind the
bridge keeps accepting up to `LOCAL-IPC.md`'s capacity, the daemon
refuses a direct message to its sender only once the session queue
itself is full (`TRANSPORT.md` §Backpressure), an ordinary broadcast is
dropped and counted at the daemon (`LOCAL-IPC.md` §Push events item 1),
and a pause that outlasts the keepalive's miss threshold ends the
session as wedged with the pipeline's content lost — bounded, never
replayed, the contract's existing fate for a non-draining client, now
reachable by design — so `paused` means the bridge has stopped taking,
the liveness clock starting only once the IPC client's own buffer has
filled behind it, `status` shows `paused_since`, and a host that takes
before that clock runs out loses no direct message acknowledged to its
sender (broadcasts past the daemon's bound are dropped and counted
there); the bridge's own queue is in memory and lost, bounded, when the
process exits. While paused, `send`, `reply`, `broadcast`, `join` and
`leave` refuse at once with a bridge-local error naming the state ("the
pull queue is full: call receive first"), never `Overloaded` and never
a stall — p2p-network-dev's finding that a call awaited behind the
paused drain would hold the bridge's one loop and the host's next
`receive` with it; a call already in flight when the queue fills is
cancelled and answered the same (the third round's risk) — its outcome
unknown, since the cancel is advisory: a repeated `send` or `broadcast`
may go twice and is never re-issued (p2p-network-dev's limit at
71bb46f3), while a cancelled `join`, re-join or `leave` — whose
first draft left the bridge's join state apart from the daemon's, the
#221 review's P2 — is kept pending and re-issued once the pause lifts,
before the bridge drains again, because the daemon's join and leave
are idempotent; the answer fixes the state, a re-issue cancelled again
stays pending, `status.pull_queue.pending` lists them, and a reply on a
channel whose join is pending is `ChannelNotJoined` until it lands; and a
paused bridge, reading nothing, learns of its session's end only at the
next take, so `status` reports it true but late; `identity`,
`status` and `receive` answer, and `receive` lifts the pause. The
session's end is reported in `status` and recovered by reconnect with
the bridge's own queue kept; every held direct message's reply token is
stale after the reconnect, since it is minted at the take under the
epoch of the lease the event arrived under, while a broadcast token
survives the re-join. (The
first review round found the first draft's "drop the oldest ordinary
event" would have lost acknowledged direct messages and path notices;
this is the corrected rule.) The mode is configuration —
`--delivery push|pull`, required, no default, so a missing flag can never
put the proved harness into pull — one mode per process, `claude/channel`
advertised only in push mode and `receive` only in pull mode. The host's
`initialize` capabilities (`roots`, `elicitation`) give nothing to key
on, and its `clientInfo.name` is an unverified product string, rejected
as a key. `receive` returns one structured result `{events: [{kind:
direct | broadcast, content, meta}], remaining, paused}` in the order the
bridge took the events from the session, `max` defaulting to and clamped
at the granted `event_queue`; content and meta are what the push would
carry; the reply token is minted at the take. The Claude Code plugin's
`.mcp.json` passes `--delivery push`. ADR-0023's tool surface is amended
the same day for the eighth tool. Proof: a scripted plain-MCP client
over stdio in `tests/`, which also records how the host frames the tool
result, beside SPIKE-001's installed-Claude run for the push mode. Lands
on #221 with p2p-network-dev's tool, mode switch and test.

**Not changed.** The Channel contract itself, trust gating before
delivery, content/meta separation, terminal-only trust mutation, the
daemon owning the transport, no remote permission relay.
