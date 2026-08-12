# Connectivity contract

Status: **Frozen for standard v1 implementation scaffolding**

This contract defines the backend-neutral Internet reachability surface. It does not expose AutoNAT, Circuit Relay, DCUtR, multiaddr manipulation, or Swarm internals to Claude/human clients.

## 1. Scope

Connectivity answers three questions:

1. Is this profile currently verified reachable by a direct inbound path?
2. Is a relay-backed inbound path currently available?
3. Which path class is being used/preferred for an application peer?

It does **not** discover application peers, grant trust, create endpoints, store messages, or guarantee delivery.

## 2. Required standard-v1 capabilities

A conforming standard-v1 backend reports:

```text
internet_reachability = true
relayed_connectivity = true
direct_path_upgrade = true
```

The initial libp2p backend realizes those capabilities with AutoNAT v2, Circuit Relay v2, and DCUtR. Another future backend may use different mechanisms while preserving this contract.

## 3. Normalized state

```text
DirectInboundState = unknown | verified_public | not_verified
RelayInboundState  = unavailable | partial | ready
PreferredPath      = direct_first
PeerPath           = direct | relayed | none

ConnectivitySummary {
  direct_inbound: DirectInboundState,
  relay_inbound: RelayInboundState,
  active_relay_reservations: u16,
  target_relay_reservations: u16,
  active_relayed_peer_paths: u16,
  hole_punch_inflight: u16,
  preferred_path_policy: PreferredPath,
  updated_at: InstantOrTimestamp,
}
```

`verified_public` means the active backend's configured direct-inbound verification policy has fresh successful evidence. For the initial backend, ADR-0035 requires multiple distinct authorized AutoNAT-v2 observers.

`not_verified` is not synonymous with “definitely private”; it means current evidence does not satisfy the direct-inbound proof rule.

`relay_inbound=ready` means the active relay-reservation target is met. `partial` means at least one active reservation exists but the current target is not met. `unavailable` means no usable active reservation exists.

## 4. Commands

### `connectivity()`

Returns the current `ConnectivitySummary` without causing a new probe, reservation, or hole-punch attempt.

It is safe for ordinary data-plane status clients.

Administrative/raw details—specific infrastructure PeerIds, addresses, probe traces, reservation failure codes—belong to diagnostics/admin surfaces and are not required by this neutral contract.

## 5. Events

```text
ConnectivityChanged { summary }
PeerConnected { peer, path, observed_at }
PeerPathChanged { peer, previous: direct | relayed, current: direct | relayed, reason, observed_at }
PeerDisconnected { peer, reason_class, observed_at }
```

`ConnectivityChanged` is edge/state notification, not an append-only history. Slow IPC clients may receive a coalesced latest state rather than every intermediate transition.

`PeerConnected` is emitted only when a logical application peer transitions from no usable application connection to at least one usable connection. A successful DCUtR hole punch for an already-connected relayed peer therefore **does not emit a second `PeerConnected`**. After the direct connection satisfies the configured stability gate, emit `PeerPathChanged { previous: relayed, current: direct, reason: dcutr }`; a coalesced `ConnectivityChanged` may accompany it. Existing streams are not claimed to migrate.

## 6. Path-selection semantics

For a trusted application peer:

1. prefer usable direct candidates;
2. if no direct path is established within the backend's bounded head-start, a usable authorized relay route may be raced/used;
3. a working relayed path may be upgraded to direct in the background;
4. failed upgrade keeps relay fallback;
5. successful upgrade is considered preferred only after the configured stability interval;
6. existing application streams are not promised to migrate between paths.

Application `send`, endpoint-directory requests and GossipSub participation do not choose AutoNAT/relay/DCUtR mechanisms directly.

## 7. Authorization classes

Connectivity uses the connection-class model from ADR-0036:

```text
DataPlaneTrusted
ConnectivityInfrastructureOnly
Unauthorized
```

`ConnectivityInfrastructureOnly` can be used only for the configured connectivity control protocols. It cannot grant or imply:

- direct application messaging;
- GossipSub membership/forwarding;
- endpoint directory access;
- Kademlia routing participation;
- Claude Channel delivery;
- EndpointId authority.

The authenticated **end application PeerId** of a relayed connection is checked under normal data-plane trust independently of the relay PeerId.

## 8. Failure semantics

Connectivity failure changes reachability/status; it does not create durable delivery semantics.

- AutoNAT probe failure: evidence becomes/stays `unknown` or `not_verified` according to the backend evidence state machine.
- relay reservation loss: remove that relay-derived advertised route immediately and reconcile the target.
- all relays unavailable: `relay_inbound=unavailable`; do not select an unapproved public relay automatically.
- hole punch failure: keep the existing relayed path when available and apply cooldown.
- network change: invalidate affected evidence and rebuild ephemeral path state.

## 9. Privacy and security

Connectivity status may reveal that a profile uses relays or lacks direct inbound verification. Ordinary status does not expose the full infrastructure topology.

Relays are availability/metadata infrastructure, not anonymity infrastructure or trust authorities. End-to-end peer authentication/encryption must remain intact across relayed paths.

## 10. Relayed inbound pre-auth accounting

Before the end-peer Noise handshake completes, no authenticated application PeerId exists. Direct-listener handshakes use the transport source-address bucket from the pre-auth admission policy. For an inbound Circuit Relay path where the original client IP is unavailable, the destination MUST charge pending/rate admission to the **authenticated relay transport connection / relay PeerId** plus the global pre-auth caps. It MUST NOT create unbounded pseudo-source buckets from circuit metadata. This intentionally lets one abusive relay exhaust its own bucket. Relay-server `max_circuits_per_source_peer` is complementary infrastructure-side protection, not a substitute for destination-side admission.

The rule is part of `TransportRuntime` and applies equally to desktop daemon and Android embedded deployment modes.

## 11. No durability

AutoNAT state, relay reservations, relay-derived addresses, DCUtR attempts and path preference are runtime state. They are rebuilt after restart/network change. None is a message queue, mailbox, delivery receipt, or durable membership record.
