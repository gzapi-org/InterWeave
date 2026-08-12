# Async architecture

Tokio is the expected Rust runtime unless implementation research disproves the fit.

## Task ownership

```text
main / daemon supervisor
  |- IPC accept task
  |    `- per-client read/write tasks + capability + endpoint-lease lifecycle
  |- transport runtime coordinator
  |    `- EndpointRegistry / endpoint policy / default-route state
  |- libp2p Swarm task (single owner)
  |    |- GossipSub
  |    |- direct request-response v2
  |    `- endpoint-directory request-response
  |- DiscoveryManager supervisor
  |    |- cache
  |    |- mDNS
  |    |- static
  |    `- Kademlia when configured (default enabled; explicit opt-out)
  |- cache writer/debounce
  `- observability sink
```

## Rules

- Swarm has one task owner; bounded channels connect it to runtime.
- Provider tasks cannot block Swarm.
- No network callback invokes Claude/human UI synchronously.
- EndpointRegistry is runtime-owned; libp2p codec does not own local process routing policy.
- IPC connection establishes at most one direct EndpointId lease.
- Direct inbound response is withheld until runtime confirms exact endpoint queue admission.
- Root cancellation and provider-local cancellation stay explicit.
- Connection/trust and GossipSub validation rules remain unchanged.

## Inbound direct asynchronous admission

```text
Swarm/direct_manager receives DirectMessageV2
   |
   v
bounded InboundDirectCandidate -> transport runtime
   |
   +-- trust/endpoint/default/policy/dedup/queue admission
   |
   v
LocalRouteAccepted(endpoint) | LocalRouteRejected(reason)
   |
   v
Swarm sends AcceptedV2 | RejectedV2
```

The admission response has a bounded internal deadline shorter than the overall direct request deadline. If runtime cannot answer due to overload/shutdown, remote receives coarse failure rather than false acceptance.

## IPC event routing

Broadcast: runtime sends to each joined local client independently.

Direct: runtime sends to exactly the IPC connection holding the resolved destination EndpointId lease. There is no fan-out. If that endpoint queue cannot accept the message, direct admission fails before `AcceptedV2`.

## Endpoint directory snapshot

Directory behavior requests a bounded immutable snapshot from EndpointRegistry containing only currently leased, `advertise: true`, requester-admissible EndpointIds. Snapshot generation must not block Swarm on slow IPC/application code.

Remote directory cache is bounded in-memory state maintained outside discovery; it never enters peer-cache persistence or Kademlia.

## Reconnection

ConnectionManager peer backoff rules remain unchanged. Local endpoint reconnect is independent: a new IPC lease does not reset remote peer backoff or transport identity.

## Kademlia task interaction

Kademlia driver/provider interaction uses bounded control channels and all behavior-originated dials pass root dial admission. Configured entries default enabled in standard v1; explicit opt-out instantiates neither side. Endpoint addressing is not a Kademlia responsibility.
