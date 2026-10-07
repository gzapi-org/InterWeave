# Reuse official Claude Channel and Telegram patterns

**Status:** Accepted

## Context

The official Telegram Channel is the closest implementation reference and demonstrates how Claude expects a local messaging bridge to behave. Current Claude documentation also defines plugin `channels` packaging that must be revalidated at implementation time.

## Decision

Use the current Claude Code Channel contract: stdio MCP server, `claude/channel` capability, push `notifications/claude/channel`, ordinary tools for outbound actions, explicit Channel instructions, and sender/trust gating before notification delivery. Adopt Telegram's proven content/meta separation and terminal-only trust mutation principle. Adapt transport ownership into a daemon. Do not opt into remote permission relay in v1.

**Two delivery modes, one bridge (A 2026-10-07).** The Claude Code Channel contract above is the bridge's PUSH mode. The same bridge serves a host that speaks plain MCP in PULL mode: one bounded tool, `receive(max)`, that takes what the bridge holds for the session in the order it took it, and never waits — `LOCAL-CLIENT.md` §2's `events(max)` exposed to the host — through ONE bounded pull queue the bridge keeps because it must drain its IPC session continuously (`LOCAL-IPC.md` §Push events: a client that stops draining is closed as wedged). The queue holds direct messages and broadcasts only — session notices are bridge state and `status` in both modes, never delivered — and is bounded at the session's granted `event_queue`; the bridge DROPS NOTHING it took: when the queue is full it PAUSES draining, and what follows is the contract's existing fate for a client that stops draining, now reachable by design and therefore stated (`CHANNEL-EVENT.md` §Delivery): the pipeline behind the bridge keeps accepting up to `LOCAL-IPC.md`'s capacity, a direct message is refused to its sender as overloaded only once the session queue is full (`TRANSPORT.md` §Backpressure), an ordinary broadcast is dropped and counted at the daemon (`LOCAL-IPC.md` §Push events item 1), and a pause that outlasts the keepalive's miss threshold ends the session as wedged with the pipeline's content lost — bounded, never replayed — which the bridge reports in `status` and recovers by the reconnect of `LIFECYCLE.md`, keeping its own queue, which is in memory and lost, bounded, when the bridge process exits (every held direct message's reply token is stale after the reconnect; a broadcast token survives the re-join). The liveness clock starts only once the IPC client's own buffer has filled behind the paused bridge; a host that calls `receive` before it runs out loses no direct message acknowledged to its sender; `status` shows `paused_since`. While paused, a tool that needs the session (`send`, `reply`, `broadcast`, `join`, `leave`) is refused at once with a bridge-local error naming the state, never a stall and never `Overloaded`, and a call in flight when the queue fills is cancelled and answered the same, its outcome unknown since the cancel is advisory (a repeated `send` may go twice; a cancelled `leave` may have left); a paused bridge, reading nothing, learns of its session's end only at the next take; `identity`, `status` and `receive` answer, and `receive` lifts the pause. The mode is CONFIGURATION, not detection — a host's `initialize` capabilities give the bridge nothing to key on (Claude Code 2.1.285 declares `roots` and `elicitation` only, SPIKE-001's recordings), and `clientInfo.name`, which it does send, is an unverified product string any host may set, so the mode never depends on it — `--delivery push|pull`, required, no default, one mode per process: the bridge advertises `claude/channel` only in push mode and the `receive` tool only in pull mode, so no session can be delivered twice. Content and metadata rules, sanitisation and the reply token are the same in both modes (`CHANNEL-EVENT.md`); the token is minted when the host TAKES the event, so its TTL runs from the host's reading; `receive`'s result is one structured object `{events: [{kind: direct | broadcast, content, meta}], remaining, paused}` (`TOOL-SURFACE.md`; `max` defaults to the granted `event_queue` and is clamped to it). The Claude Code plugin passes `--delivery push`. Proof: a scripted plain-MCP client over stdio beside SPIKE-001's installed-Claude run.

## Alternatives considered

Inventing a parallel Claude integration; embedding P2P details in prompt tags; mechanically copying Telegram tools/state/poller architecture; remote permission relay by PeerId alone. For the pull mode (2026-10-07): MCP resources with subscription (hosts implement it unevenly; a tool works in every client); detecting the mode from the host's capabilities (nothing to key on); a second bridge binary (two surfaces to keep aligned); delivering without a bridge-side queue (the IPC drain obligation forbids it); dropping the oldest event past the queue's bound (an acknowledged direct message would be lost, which `TRANSPORT.md` §Backpressure forbids — the bridge pauses instead and the daemon's bounds decide).

## Consequences

Claude-facing behavior stays familiar and testable. Packaging remains version-sensitive and requires a pre-implementation compatibility spike.

## Security implications

The strongest adopted pattern is admission before Claude injection. Trust config cannot be changed merely because an inbound Channel message asks for it. Permission relay is deferred because PeerId is not equivalent to an authorized human approver.

## Operational implications

The bridge remains session-scoped and can be restarted independently. The daemon carries network continuity.

## Implementation implications

Future plugin package will bind its Channel to an MCP server according to the current plugin reference; implementation uses the MCP SDK version supported by the target Claude Code release.

## Revisit conditions

Revisit if Anthropic changes the Channel extension contract or graduates it from research preview with incompatible lifecycle/packaging semantics; or if MCP standardises a push delivery every host implements, which would retire the pull mode's queue.

## Amendments

Full notes: [`history/0002-amendments.md`](./history/0002-amendments.md).

| Date | Amendment | Effect |
|---|---|---|
| 2026-10-07 | Two delivery modes, one bridge: the Channel push and a bounded pull tool for a plain MCP host | Decision: the Claude Channel contract is the push mode; pull mode adds one tool, `receive(max)` over `events(max)`, through one bounded pull queue of direct messages and broadcasts (the granted `event_queue`; notices stay bridge state) that drops nothing — full, the bridge pauses draining, session-bound tools refuse at once with a bridge-local error, and a pause outlasting the keepalive ends the session as wedged with the pipeline's bounded content lost, the contract's existing fate for a non-draining client, stated; the mode is `--delivery push|pull`, required, one per process, capability and tool advertised only in their mode; content, meta, sanitisation and reply token shared, the token minted at the take; the plugin passes push; proof by a scripted plain-MCP client. ADR-0023 amended for the tool. Alternatives and Revisit conditions extended. |

