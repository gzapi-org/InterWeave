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
  |    |- Identify + address registry adapter
  |    |- AutoNAT v2 client (+ optional configured server role)
  |    |- Circuit Relay v2 client (+ optional configured server role)
  |    |- DCUtR
  |    |- GossipSub
  |    |- direct request-response v2
  |    |- endpoint-directory request-response
  |    `- Kademlia driver when configured
  |- ReachabilityManager / RelayManager / DCUtRManager
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
- Connection/data-plane trust, connectivity-infrastructure admission, and GossipSub validation rules remain explicit and separate.

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

## Mandatory reachability task interaction

AutoNAT-v2, Circuit Relay v2, and DCUtR behaviours are standard-v1 Swarm components. Their events are normalized into bounded manager inputs; no behaviour callback mutates trust, endpoint leases, or application state directly.

Every dial these behaviours cause crosses `DialAdmissionGate` under an attributable origin (`autonat-probe`, `relay-reservation`, `relay-circuit`, `dcutr-hole-punch`) — but they are not all announced the same way. `autonat-probe`, `relay-reservation` and `dcutr-hole-punch` are behaviour-originated: the behaviour emits the dial and a wrapper around it announces the origin. **`relay-circuit` is a command-path origin.** SPIKE-004 measured that `relay::client::Behaviour` emits no dial for a `/p2p-circuit` address — the relay TRANSPORT handles that address — so no behaviour wrapper ever sees the dial, and `GatedSwarm::dial` sets the origin from the address it was handed. Attribution built only into a behaviour wrapper would let circuit dials reach the gate under the wrong origin. Infrastructure-only PeerIds are admitted only for their control-plane origins — `autonat-probe` and `relay-reservation`. `relay-circuit` and `dcutr-hole-punch` are **data-plane origins and stay so regardless of destination**: each names the far end of an application path, so one toward an infrastructure-only peer is refused (ADR-0036 Amendment 2026-09-03), and one toward a trusted peer reached *through* infrastructure is admitted on that trusted destination's class. What varies is the destination's class, never the origin's plane. **This describes the target behaviour, not the shipped gate**: `DialOrigin::is_data_plane` omits both origins today, so a circuit or hole punch toward an infrastructure-only peer is still admitted — SPIKE-004's divergences D2 and D1, fixed in Stage 11 step 2.

Relay reservation events update the address registry synchronously inside the Swarm ownership domain, then emit bounded reachability-state changes upward. A reservation close removes its relay-derived listen address immediately.

DCUtR success creates a new direct connection. ConnectionManager waits for the configured direct stability period before retiring a redundant relayed peer path; existing streams are not modeled as migrated.
