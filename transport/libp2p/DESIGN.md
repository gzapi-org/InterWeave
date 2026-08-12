# libp2p backend design

## Implementation target stack

```text
TCP
 -> Noise XX connection security
 -> Yamux stream multiplexing
 -> Identify
 -> AutoNAT v2 client (mandatory)
 -> Circuit Relay v2 client transport/reservations (mandatory)
 -> DCUtR (mandatory)
 -> optional configured AutoNAT v2 server / Circuit Relay v2 server roles
 -> GossipSub (broadcast)
 -> request-response /direct/2.0.0 (endpoint-addressed direct)
 -> request-response /endpoints/1.0.0 (optional trusted route directory)
 -> Kademlia behaviour when configured (peer-routing only; default enabled, explicit opt-out)
```

Discovery behaviours remain behind `DiscoveryProvider`; endpoint directory and relay/AutoNAT service selection are **not** DiscoveryProviders.

## Internal ownership

One backend event loop owns the Swarm. Commands/events cross bounded channels; no Claude/human callback executes on the Swarm loop.

ConnectionManager remains policy owner for connection class, dial origin, reconnect/backoff, path preference, retention, and global/per-peer limits. Root `DialAdmissionGate` applies to explicit scheduler dials and behaviour-originated Kademlia/AutoNAT/relay/DCUtR dials.

Reachability logic is split into internal managers rather than a second Swarm:

- `reachability_manager` — AutoNAT-v2 evidence and normalized connectivity status;
- `relay_manager` — relay candidates, reservations, failover, ephemeral relay addresses, optional server-role state;
- `dcutr_manager` — bounded relayed-to-direct upgrade policy/cooldown;
- `address_registry` — direct/relay address provenance, verification, expiry, Identify advertisement view.

The integrated state machine is in [`CONNECTIVITY.md`](./CONNECTIVITY.md); mechanism-specific blueprints are [`AUTONAT.md`](./AUTONAT.md), [`RELAY.md`](./RELAY.md), and [`DCUTR.md`](./DCUTR.md).

## Connection classes

There are two authorized connection classes:

- `DataPlaneTrusted` from `trust.allowed_peers`;
- `ConnectivityInfrastructureOnly` from `transport.connectivity.infrastructure.allowed_peers` when the PeerId is not data-plane trusted.

Infrastructure-only connections may carry Identify/AutoNAT/relay control traffic but are excluded from GossipSub, direct v2, endpoint directory, Kademlia routing, and DCUtR application-destination use. This prevents mandatory relay/probe infrastructure from becoming an application peer by accident.

## Direct v2 / EndpointRegistry split

Backend `direct_manager` owns wire protocol mechanics but not local endpoint ownership.

```text
DirectMessageV2 decoded
 -> bounded runtime admission request
 -> EndpointRegistry/profile trust/policy/default/lease/queue decision
 -> LocalRouteAccepted(endpoint) | LocalRouteRejected(reason)
 -> AcceptedV2 | RejectedV2
```

This prevents libp2p code from deciding which local process represents `human` or `claude`.

## Endpoint directory

Backend exposes `/endpoints/1.0.0` as a separate bounded request-response protocol. It requests a snapshot from runtime EndpointRegistry and returns only active, advertise=true, requester-admissible EndpointIds.

No endpoint descriptors are stored in Identify, GossipSub, Kademlia, peer cache, relay service metadata, or AutoNAT.

## Trust and infrastructure admission

Discovery observations populate candidate state independently. Direct, endpoint-directory, GossipSub, and Kademlia participation require **data-plane** profile trust. Endpoint policy can only narrow direct route admission.

Relay and AutoNAT control paths may additionally use explicitly configured connectivity-infrastructure-only PeerIds per ADR-0036. Those peers are blacklisted/excluded from GossipSub and denied application protocol admission.

Revoking a data-plane or infrastructure authorization causes the relevant connections/protocol state/reservations to be torn down according to current role.

## Address sources

DiscoveryManager candidates, trusted/authorized Identify observations, AutoNAT-v2 evidence, and active relay reservations feed the backend address registry through explicit adapters. Discovery providers do not mutate Swarm directly.

Internet-facing advertised addresses are restricted to fresh AutoNAT-verified direct addresses and active relay-reservation addresses by default. Merely observed/configured public addresses remain candidates/diagnostics unless verified.

## Kademlia driver

The private/trust-bounded Kademlia design remains unchanged except ADR-0034 makes configured entries default-enabled in standard v1. It never stores or advertises EndpointIds and does not accept infrastructure-only relay/probe PeerIds as routing peers under the v1 `data-plane-trusted` policy.
