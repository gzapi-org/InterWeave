# libp2p backend design

## Implementation target stack

```text
TCP
 -> Noise XX connection security
 -> Yamux stream multiplexing
 -> Identify
 -> GossipSub (broadcast)
 -> request-response /direct/2.0.0 (endpoint-addressed direct)
 -> request-response /endpoints/1.0.0 (optional trusted route directory)
 -> optional Kademlia behaviour (peer-routing only; default disabled)
```

Discovery behaviors remain behind `DiscoveryProvider`; endpoint directory is **not** a DiscoveryProvider.

## Internal ownership

One backend event loop owns the Swarm. Commands/events cross bounded channels; no Claude/human callback executes on Swarm loop.

ConnectionManager remains policy owner for trust admission, reconnect/backoff, retention, and global/per-peer limits. Root `DialAdmissionGate` applies to explicit scheduler dials and behavior-originated dials.

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

No endpoint descriptors are stored in Identify, GossipSub, Kademlia, peer cache, or application payload implicitly.

## Trust-gated data-plane connections

Discovery observations populate candidate state independently. Ordinary direct, endpoint-directory, GossipSub, and first-generation Kademlia participation require profile trust. Endpoint policy can only narrow direct route admission.

Trust revocation closes affected data-plane connections. A future untrusted control-plane connection class requires a separate ADR.

## Address sources

DiscoveryManager candidate observations plus trusted connected-peer Identify information feed backend address book. Discovery providers do not mutate Swarm directly.

## Optional Kademlia driver

Existing private/trust-bounded Kademlia design remains unchanged and default-disabled. It never stores or advertises EndpointIds.
