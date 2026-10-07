# Plugin/bridge lifecycle

## Startup

1. An MCP host launches the bridge over stdio with `--delivery push|pull` (required; the Claude Code plugin passes `push`, A 2026-10-07): a process started without it refuses to start and names both values.
2. Bridge loads plugin-local non-secret routing config: profile name/data-socket + configured local EndpointId.
3. Bridge connects only to daemon IPC v2 data socket.
4. Hello requests non-admin capabilities and claims the configured EndpointId.
5. Daemon grants endpoint lease/epoch or returns a clear conflict/configuration error.
6. Bridge reads the profile PeerId from the session's creation context (`local_peer`) and the effective limits from the grant, and re-takes the channel joins it held before a reconnect — none at first start: joins are made through the join tool, never by configuration; a re-join the daemon refuses leaves `joined_channels` and is held in `status.rejoin_refused` until the next join or leave of that channel — never composed into a channel event, whose body is the payload's content alone (`contracts/CHANNEL-EVENT.md` §Sanitization), and no other notification: the bridge declares no MCP `logging` capability — so Claude learns of it through `status` and joins again through the tool (A 2026-10-06).
7. Inbound delivery begins: in push mode the Channel notifications; in pull mode the bridge's bounded pull queue fills and `receive` takes from it; a full queue pauses the drain, session-bound tools refuse at once while it is paused, and a pause that outlasts the keepalive ends the session as wedged, with the pipeline's bounded content lost, after which the bridge reconnects keeping its own queue (`CHANNEL-EVENT.md` §Delivery).

The bridge never receives the profile private key and never becomes daemon owner merely because it started first.

## Endpoint conflict

If another live IPC client owns the configured EndpointId, startup reports `EndpointInUse` for direct routing. The bridge does not generate a random substitute or steal the route. Operator must stop the owner or configure another endpoint.

## Reconnect

Reconnect uses bounded exponential backoff. Each reconnect performs a **new** endpoint claim and receives a new lease epoch, then re-establishes bridge-owned joined channels. No missed network events are replayed.

Any pre-disconnect direct reply tokens are discarded/invalid because their local lease epoch is stale.

## Shutdown

Claude Code stops the bridge with **SIGINT**; if the process is still alive about 100 ms later it sends **SIGTERM**, and if still alive about 400 ms after that **SIGKILL**; stdin is never closed (SPIKE-001 fact 18 at d5ed3b76, Claude Code 2.1.285; A 2026-10-03 — this said "MCP stdin close/SIGTERM"). There is no graceful window to count on. The bridge treats the first signal as final: it does no work it cannot finish inside it, and the release of its EndpointId lease and bridge joins is the daemon's act when the IPC connection drops (LOCAL-IPC.md: a lease ends with its connection), never something the bridge must do after a signal. The stop is the bridge's only: daemon and PeerId remain alive.

A bridge process that exits during a session is **not restarted** by Claude Code: it starts once, its tools become unavailable to ToolSearch, the model reports a host "failed to connect" notice (not itself in the committed evidence), and the events it delivered stay in the conversation (fact 19). That is why §Session behavior of `CLAUDE-CODE-CHANNEL.md` requires the bridge to stay a functioning MCP server while the daemon is away: exiting on a daemon fault would end the channel for the session.

The bridge never connects to the admin socket and can never be granted `admin.shutdown` or `admin.endpoints` on its data-socket connection.


Identity recovery never runs through the bridge or daemon IPC. The bridge must be stopped with the daemon/profile identity operation before offline backup/restore.
