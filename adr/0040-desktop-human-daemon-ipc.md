# Desktop human client uses the shared daemon and IPC v2

**Status:** Accepted

## Context

Desktop can keep a long-lived profile daemon independently of a UI window and can share that daemon with Claude endpoints. This is exactly the lifecycle/isolation benefit selected earlier for Claude.

## Decision

Windows/macOS/Linux first-party human clients use the external Rust transport daemon. Messaging uses IPC v2 data socket and an exclusive configured EndpointId lease. Settings administration uses the separate admin socket. Human application history lives in the human client, never the daemon. Closing the UI releases its endpoint but does not stop the daemon by default.

## Alternatives considered

Embed libp2p in the desktop GUI; one daemon per human window; merge data/admin sockets; stop daemon whenever UI exits.

## Consequences

Claude and human endpoints may share one profile PeerId and network state deterministically. Desktop UI crash/restart does not rotate identity or necessarily tear down network connectivity.

## Security implications

The UI never receives transport key bytes. ADR-0037 socket-domain separation remains enforceable on desktop. Same-UID admin-socket access remains the documented residual boundary.

## Operational implications

Packaging includes human app, daemon, and transportctl; daemon autostart is operator/user policy.

## Implementation implications

Desktop uses the `LOCAL-CLIENT` semantics through an IPC client adapter; no direct `transport-libp2p` dependency is permitted.

## Revisit conditions

Revisit only if a desktop platform materially cannot support the daemon lifecycle or if sandbox/store distribution requires an explicit platform-specific broker.
