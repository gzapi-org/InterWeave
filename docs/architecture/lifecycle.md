# Lifecycle

## Profile lifecycle

A transport profile owns one persistent PeerId, configuration namespace, socket endpoint, mutable daemon state, and replaceable peer cache. Starting a daemon acquires a profile lock; a second daemon for the same profile fails fast and reports the existing socket/owner.

## Bridge lifecycle

1. Claude Code starts the MCP bridge over stdio.
2. Bridge resolves the configured profile/socket.
3. Bridge performs IPC version/capability handshake.
4. Bridge requests subscriptions required by its configuration/session.
5. Daemon pushes normalized events while the client remains connected.
6. MCP stdin close/shutdown stops only the bridge and releases its subscription handles.
7. Daemon remains alive unless explicitly configured for ephemeral service mode or stopped by the local operator/service manager.

## Daemon lifecycle

1. load validated normal configuration;
2. acquire profile lock;
3. securely load/generate identity key;
4. bind IPC endpoint with owner-only permissions;
5. start libp2p backend/listeners;
6. start DiscoveryManager providers independently;
7. begin ConnectionManager reconciliation;
8. accept local clients;
9. on shutdown: stop accepting commands, cancel providers, close direct requests, flush advisory cache best-effort, close swarm, remove socket/lock.

## Recovery

- bridge reconnect: resubscribe; no message replay;
- daemon restart: same PeerId if key is intact, cached candidates accelerate reconnect;
- identity key missing: generate only if profile is explicitly uninitialized; otherwise fail safe if config expects an existing identity;
- corrupt key: fail closed, never silently rotate;
- provider restart: provider-local backoff and health transitions; unrelated providers remain alive.
