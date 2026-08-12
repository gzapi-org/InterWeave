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
  |    |- provider task: static
  |    `- provider task: Kademlia (optional; only when supported + enabled)
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

ConnectionManager maintains peer-scoped exponential backoff with jitter and a maximum retry interval **for authorized peers**. Discovery updates can add addresses but do not reset a punitive backoff endlessly; a successful connection resets it. Unauthorized candidates remain bounded observations. Explicit scheduler dials are not issued for them, and behaviour-originated dial attempts are denied by the root admission gate.

## Provider restart

Transient provider failure transitions health to degraded/unavailable, waits provider-scoped backoff, then restarts if configured. Repeated failure does not restart the whole runtime. A provider configured enabled but unsupported by the active build is a configuration/startup failure, not a restartable provider outage.

## IPC event fan-out

Runtime emits one normalized message event. IPC interest is mode-specific: broadcast goes only to clients holding that ChannelId join reference; direct goes independently to every connected message-event client. A profile-desired backend subscription is not local interest. If no eligible client exists, the event is not retained for replay. Slow clients drop their own queued events rather than blocking other clients. Serialized frames are checked against the fixed 131,072-byte JSON-body ceiling before write.


## Kademlia task interaction

The optional Kademlia provider task never polls the Swarm. It sends bounded commands through `kademlia-control-api` to the Swarm-owned Kademlia driver and consumes normalized driver events. Query rate/concurrency permits are acquired before commands are sent. Kademlia's behaviour may request outbound dials during query execution; those attempts pass the root `DialAdmissionGate` backed by ConnectionManager state. Driver-event overflow must not block the Swarm; it marks the provider degraded and coalesces/drops noncritical diagnostics under explicit counters.

`enabled: false` means no provider task, no Kademlia query scheduling, and no project Kademlia protocol participation.
