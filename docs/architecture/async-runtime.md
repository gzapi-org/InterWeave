# Async architecture

Tokio is the expected Rust runtime unless implementation research disproves the fit.

## Task ownership

```text
main / daemon supervisor
  |- IPC accept task
  |    `- per-client read/write tasks
  |- transport runtime coordinator
  |- libp2p Swarm task (single owner)
  |- DiscoveryManager supervisor
  |    |- provider task: cache
  |    |- provider task: mDNS
  |    `- provider task: static
  |- cache writer/debounce task
  `- observability sink
```

## Rules

- The Swarm is owned by one task; other components communicate through bounded channels.
- Provider tasks cannot block the Swarm loop.
- No network callback calls Claude/MCP synchronously.
- Bounded channels use explicit overflow behavior.
- A root cancellation token fans out to child tasks; provider-local tokens permit independent restart.
- Shutdown has phases: stop ingress -> cancel discovery/new dials -> settle bounded in-flight direct responses -> close Swarm/IPC -> flush advisory cache -> exit.

## Reconnection

ConnectionManager maintains peer-scoped exponential backoff with jitter and a maximum retry interval. Discovery updates can add addresses but do not reset a punitive backoff endlessly; a successful connection resets it.

## Provider restart

Transient provider failure transitions health to degraded/unavailable, waits provider-scoped backoff, then restarts if configured. Repeated failure does not restart the whole runtime.

## IPC event fan-out

Runtime emits one normalized message event. IPC server fans out only to interested local clients and copies/retains payload under bounded memory accounting. Slow clients drop their own events rather than blocking other clients.
