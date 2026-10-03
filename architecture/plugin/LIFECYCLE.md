# Plugin/bridge lifecycle

## Startup

1. Claude Code launches the Channel bridge over stdio.
2. Bridge loads plugin-local non-secret routing config: profile name/data-socket + configured local EndpointId.
3. Bridge connects only to daemon IPC v2 data socket.
4. Hello requests non-admin capabilities and claims the configured EndpointId.
5. Daemon grants endpoint lease/epoch or returns a clear conflict/configuration error.
6. Bridge obtains profile PeerId/effective limits and establishes its channel joins.
7. Inbound Channel notifications begin.

The bridge never receives the profile private key and never becomes daemon owner merely because it started first.

## Endpoint conflict

If another live IPC client owns the configured EndpointId, startup reports `EndpointInUse` for direct routing. The bridge does not generate a random substitute or steal the route. Operator must stop the owner or configure another endpoint.

## Reconnect

Reconnect uses bounded exponential backoff. Each reconnect performs a **new** endpoint claim and receives a new lease epoch, then re-establishes bridge-owned joined channels. No missed network events are replayed.

Any pre-disconnect direct reply tokens are discarded/invalid because their local lease epoch is stale.

## Shutdown

Claude Code stops the bridge with **SIGINT**, and does not close its stdin first (SPIKE-001 fact 18, Claude Code 2.1.285; A 2026-10-03 — this said "MCP stdin close/SIGTERM"). The bridge therefore treats SIGINT as the orderly stop and must release its EndpointId lease and bridge joins on it; stdin reaching EOF or SIGTERM are handled the same way but are not what the host sends. The stop is the bridge's only: its IPC connection closes, releasing the lease and the joins; daemon and PeerId remain alive.

A bridge process that exits during a session is marked failed by Claude Code and is **not restarted**: its tools become unavailable, the model is told to reconnect it with `/mcp`, and the events it delivered stay in the conversation (fact 19). That is why §Session behavior of `CLAUDE-CODE-CHANNEL.md` requires the bridge to stay a functioning MCP server while the daemon is away: exiting on a daemon fault would end the channel for the session.

The bridge never connects to the admin socket and can never be granted `admin.shutdown` or `admin.endpoints` on its data-socket connection.


Identity recovery never runs through the bridge or daemon IPC. The bridge must be stopped with the daemon/profile identity operation before offline backup/restore.
