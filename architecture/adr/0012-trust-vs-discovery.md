# Deny-by-default static PeerId trust policy

**Status:** Accepted

## Context

Authenticated PeerId is necessary but not sufficient authorization. A small static model is safer than inventing distributed membership in the transport layer. The policy must also define outbound and connection behavior, not only whether an inbound payload reaches Claude.

## Decision

Use a `PeerTrustPolicy` abstraction. The initial implementation uses a static allowlist of transport PeerIds, deny by default. Discovery never mutates it. Trust administration is a local privileged path, not a Claude Channel tool triggered by remote content.

Authorization applies consistently to the **application data plane**. ADR-0036 additionally defines a separate connectivity-infrastructure authorization set that permits only reachability-control protocols and does not widen this trust policy.

Authorization applies consistently to the data plane:

- **connection admission:** only allowlisted PeerIds may be dialed/retained for ordinary direct/GossipSub data-plane connectivity;
- **inbound message admission:** only allowlisted original message sources may reach normalized `MessageReceived` delivery; direct endpoint policy may then narrow admission further;
- **outbound direct send:** `send({peer, endpoint?}, ...)` to a non-allowlisted PeerId returns `UnauthorizedPeer` locally before dialing; endpoint-specific outbound policy may only narrow that set;
- **broadcast propagation:** GossipSub messages whose authenticated original source is not locally trusted are handled as `Ignore`, not `Reject`, per ADR-0029;
- **revocation:** removing a PeerId from the allowlist evicts active data-plane connectivity and emits operational trust/connection events.

The profile's own local PeerId is intrinsically self-authorized for local runtime identity checks and is not required in `allowed_peers`; the allowlist governs remote transport identities. This does **not** make self-directed network messaging meaningful: `send(local PeerId)` is `InvalidArgument` and never attempts a libp2p self-dial.

Static bootstrap configuration remains reachability input only and does not add the bootstrap PeerId to the allowlist. Kademlia also uses this data-plane allowlist to admit DHT routing/query peers.

Mandatory Internet reachability is different: an operator may place a relay/AutoNAT service PeerId in `transport.connectivity.infrastructure.allowed_peers`. Such a peer may establish only the protocol-scoped control connection defined by ADR-0036 and is **not** authorized for GossipSub, direct v2, endpoint directory, Kademlia routing, Channel delivery, or EndpointId policy. A peer in both sets is data-plane trusted. Discovery/Identify observations never mutate either set.

## Alternatives considered

AllowAll default; trust every discovered/bootstrap peer; TOFU default; project secret as implicit identity; DHT membership; inbound-only trust while outbound/connectivity remain unrestricted.

## Consequences

Operators must distribute PeerIds out-of-band. This is less convenient at scale and asymmetric allowlists may interrupt GossipSub propagation paths, but the security boundary is clear and auditable. Future policy implementations remain possible.

## Security implications

Strongly reduces rogue-peer injection and prevents discovery or reachability infrastructure from joining the local data-plane overlay solely by being connected. Infrastructure-only connections must be excluded from GossipSub and rejected by direct/endpoint/Kademlia admission. Key theft still impersonates whichever locally authorized PeerId class the stolen key belongs to; application role binding is out of scope.

## Operational implications

Trust changes can be reloaded locally and audited. Removing a peer disconnects it and can alter mesh/reachability. Outbound `UnauthorizedPeer` is a local policy error, not a dial or remote failure.

## Implementation implications

Define trust-core without discovery/endpoint dependencies. Query profile data-plane trust at connection admission, outbound direct dispatch, GossipSub source validation, Kademlia routing admission, and local MessageReceived admission. A separate normalized connectivity-infrastructure authorization view is consumed only by the root dial/protocol admission path. EndpointRegistry applies only an additional narrowing filter and must never widen PeerTrustPolicy. Emit `TrustPolicyChanged { revision, at }`; when revocation affects a connection also emit `PeerDisconnected { reason_class: policy, ... }`.

## Revisit conditions

Revisit for enterprise scale or usability after designing signed membership/enterprise policy with explicit revocation and protocol-scoped connection semantics.
