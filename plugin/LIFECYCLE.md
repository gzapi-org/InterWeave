# Plugin/bridge lifecycle

## Startup

1. Claude Code launches the Channel bridge over stdio.
2. Bridge loads plugin-local non-secret routing config: profile name/socket + configured local EndpointId.
3. Bridge connects to daemon IPC v2.
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

MCP stdin close/SIGTERM stops only the bridge. Its IPC connection closes, releasing EndpointId lease and bridge joins. Daemon and PeerId remain alive.

The bridge is never granted `admin.shutdown` or `admin.endpoints`.


Identity recovery never runs through the bridge or daemon IPC. The bridge must be stopped with the daemon/profile identity operation before offline backup/restore.
