# Async architecture

Tokio is the expected Rust runtime unless implementation research disproves the fit.

## Task ownership

```text
main / daemon supervisor
  |- IPC accept task
  |    `- per-client read/write tasks + capability enforcement
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
- Connection commands and inbound connection retention consult `PeerTrustPolicy` before ordinary data-plane participation.
- GossipSub manual validation reports `Accept | Ignore | Reject` per ADR-0029 before accepted messages enter normalized delivery queues.
- Shutdown has phases: stop ingress -> cancel discovery/new dials -> settle bounded in-flight direct responses -> close Swarm/IPC -> flush advisory cache -> exit, and can be initiated over IPC only by an authorized administrative client.

## Reconnection

ConnectionManager maintains peer-scoped exponential backoff with jitter and a maximum retry interval **for authorized peers**. Discovery updates can add addresses but do not reset a punitive backoff endlessly; a successful connection resets it. Unauthorized candidates remain bounded observations and are not dialed.

## Provider restart

Transient provider failure transitions health to degraded/unavailable, waits provider-scoped backoff, then restarts if configured. Repeated failure does not restart the whole runtime. A provider configured enabled but unsupported by the active build is a configuration/startup failure, not a restartable provider outage.

## IPC event fan-out

Runtime emits one normalized message event. IPC server fans out only to interested local clients and copies/retains payload under bounded memory accounting. Slow clients drop their own events rather than blocking other clients. Serialized frames are checked against the fixed 131,072-byte JSON-body ceiling before write.
