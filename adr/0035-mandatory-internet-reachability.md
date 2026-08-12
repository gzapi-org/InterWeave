# Mandatory v1 Internet reachability with AutoNAT v2, Circuit Relay v2, and DCUtR

**Status:** Accepted; supersedes ADR-0024

## Context

The previous architecture treated NAT traversal as conditional hardening. That is no longer sufficient for the product target: a standard v1 peer must remain usable from ordinary home, office, hotel, hotspot, CGNAT, and firewall-constrained networks without requiring every operator to provision a public listening address or manual port forwarding.

rust-libp2p provides three complementary mechanisms rather than one universal NAT primitive:

- AutoNAT v2 tests whether candidate direct addresses are externally dialable;
- Circuit Relay v2 supplies an inbound/outbound fallback path when direct dialing is not available;
- DCUtR coordinates a direct connection upgrade over an already-working relayed connection.

These mechanisms change connection establishment and address advertisement, but must not change transport identity, EndpointId routing, trust, discovery, or application delivery semantics.

## Decision

The **standard v1 build and release include the complete Internet-reachability stack**:

1. AutoNAT **v2 client** support is mandatory and active for every standard profile.
2. Circuit Relay **v2 client transport** and reservation management are mandatory and active for every standard profile.
3. DCUtR is mandatory and is attempted for eligible trusted relayed peer connections.
4. Identify remains mandatory and is explicitly wired to the reachability/address manager; rust-libp2p components are not assumed to integrate themselves implicitly.
5. AutoNAT-server and relay-server roles are supported by the standard build but are explicit infrastructure roles, disabled unless configured. Android first-party profiles are client-only for these roles.
6. `SPIKE-004` becomes a **release gate that validates/tunes this fixed architecture**. It no longer decides whether Phase 9 exists.
7. Phase 9 is required for the standard-v1 release. A build that omits AutoNAT v2, relay-client support, or DCUtR is a non-standard/reduced build and must advertise that limitation explicitly.

### Reachability state

The runtime tracks direct and relayed inbound reachability independently:

```text
DirectInbound = Unknown | VerifiedPublic | NotVerified
RelayInbound  = Unavailable | Partial | Ready
```

`VerifiedPublic` requires recent successful AutoNAT-v2 evidence from the configured minimum number of distinct authorized probe servers for at least one candidate direct address. Configured or Identify-observed addresses alone do not count as verified-public evidence.

`RelayInbound::Ready` requires the target number of active Circuit Relay v2 reservations. `Partial` means at least one reservation is active but the target redundancy is not met.

A peer may be simultaneously `VerifiedPublic` and `RelayInbound::Ready`; direct paths remain preferred while relays provide failover.

### Relay reservation policy

Default reservation targets are:

- direct reachability `Unknown` or `NotVerified`: **2** distinct relay PeerIds;
- direct reachability `VerifiedPublic`: **1** warm relay reservation;
- maximum active reservations: **4**.

Relays are selected from static configured relay addresses by default. Identify-learned relay/probe candidates require explicit opt-in (`use_authorized_identify_* = true`), and static candidates have selection precedence until they cannot meet the configured target. Relay service discovery never uses Kademlia provider/value records.

### Path selection

For a trusted destination:

1. reuse an existing healthy direct connection;
2. attempt known direct addresses;
3. after a bounded direct head-start, allow a known relay route to race/fallback;
4. first authenticated usable path may satisfy the pending operation;
5. if the winning path is relayed, DCUtR may attempt a direct upgrade in the background;
6. a failed DCUtR attempt does **not** tear down the working relay path;
7. after a stable direct upgrade, new streams prefer direct; the old relayed peer connection is retired only after a bounded grace period and without pretending existing streams migrated.

All explicit and behaviour-originated dials still pass the root `DialAdmissionGate` with an attributable dial origin such as `direct`, `relay-reservation`, `relay-circuit`, `autonat-probe`, or `dcutr-hole-punch`.

### Address advertisement

The runtime owns an address registry with provenance and expiry. Internet-facing advertisement rules are:

- AutoNAT-v2-verified direct addresses may be advertised as direct Internet addresses while evidence is fresh;
- active Circuit Relay v2 reservation addresses may be advertised only for the lifetime of the reservation;
- expired/closed relay reservations are removed immediately;
- private/LAN or merely observed direct addresses are not promoted as verified Internet addresses;
- relay-derived addresses are ephemeral and never treated as identity, trust, bootstrap authority, or durable endpoint presence.

### Infrastructure roles

A relay/probe server may be either a normal data-plane trusted peer or a connectivity-infrastructure-only peer as defined by ADR-0036. Merely authorizing a peer to provide relay or AutoNAT service never grants it Channel, direct-message, endpoint-directory, or Kademlia data-plane authority.

## Alternatives considered

- keep NAT traversal optional;
- require operators to provision public IPs or port forwarding;
- require Circuit Relay but omit AutoNAT/DCUtR;
- make every bootstrap node automatically a relay;
- use a centralized always-on broker instead of P2P reachability;
- make relay/probe servers ordinary data-plane-trusted peers by necessity.

## Consequences

The v1 Swarm is more complex and has more behaviours, address states, timers, and failure paths. In exchange, the standard product has a defined path for consumer-NAT operation instead of treating it as an optional later feature.

Relay fallback is a correctness/availability path, not only an optimization. DCUtR is an optimization when NATs permit direct hole punching. Failure to hole-punch is normal and retains relay connectivity.

## Security implications

Relay circuits are still authenticated to the actual remote PeerId through the libp2p secure transport. A relay is not a trust authority. Relay operators can observe PeerIds, timing, connection volume, and relay usage and can deny/drop service; they must not be treated as anonymous or metadata-private infrastructure.

AutoNAT results are advisory network evidence, not authorization. Multiple distinct authorized probe servers are required before the runtime labels a direct address verified-public, reducing dependence on one lying/misconfigured observer without claiming Byzantine consensus.

Relay/AutoNAT server roles require strict resource limits and protocol-scoped peer admission. Infrastructure-only peers cannot join the local GossipSub data plane or send application direct traffic merely because a control connection exists.

## Operational implications

Internet deployments should provision at least two independent public relay/probe servers for redundancy. One machine may host bootstrap, relay, and AutoNAT-server roles, but those are independent roles and must be configured/observed separately. Loss of all relays degrades a private peer but does not redefine identity or delete state.

Status/metrics must distinguish direct-public verification, active relay reservations, active relayed peer paths, hole-punch attempts/results, and relay-server capacity.

## Implementation implications

The libp2p composite behaviour adds AutoNAT v2 client, Circuit Relay v2 client, and DCUtR to the mandatory standard build, plus optional AutoNAT/relay server behaviours when configured. The Swarm task wires their events into a `ReachabilityManager`/`RelayManager` and shared address registry.

`ConnectionManager` gains path-aware candidate selection but remains the policy owner. `DialAdmissionGate` must account for behaviour-originated reachability dials and protocol-scoped infrastructure authorization.

`SPIKE-004` must exercise the actual selected rust-libp2p version across public, cone-NAT, symmetric/CGNAT-like, firewall, relay-loss, relay-capacity, and network-change scenarios before release.

## Revisit conditions

Revisit protocol versions/defaults when rust-libp2p deprecates the selected APIs, when deployment evidence shows a different transport such as QUIC materially improves hole-punch success, when relay operating cost requires new economics/admission, or when the application moves beyond static peer/infrastructure authorization.
