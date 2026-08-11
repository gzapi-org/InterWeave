# Channel bridge lifecycle

## Startup

1. MCP process starts under Claude Code over stdio.
2. Bridge loads only plugin-facing configuration: profile name/socket locator and safe defaults.
3. Connect to daemon IPC and negotiate version/capabilities.
4. Verify transport contract compatibility.
5. Register session-requested channel subscriptions.
6. Start event-forwarding task.

If the daemon cannot be reached, the bridge starts in degraded mode if MCP registration can still complete; network tools return daemon-unavailable until reconnect.

## Shutdown

On stdio EOF/close or termination signal:

- stop accepting new tool calls;
- cancel IPC read/write tasks;
- release local subscription references best-effort;
- close IPC;
- exit promptly.

Do **not** stop the daemon by default.

## Reconnect

Reconnect uses exponential backoff with jitter, capped at 15 seconds. After reconnect the bridge performs a fresh handshake and re-establishes desired subscriptions. No missed network events are replayed.
