# Protocol-scoped connectivity infrastructure peers

**Status:** Accepted

## Context

Mandatory relay and AutoNAT support requires connections to infrastructure peers that may not be application participants. Requiring every relay/probe server to enter `trust.allowed_peers` would accidentally authorize it for the data plane: GossipSub, direct messages, endpoint directory, and Kademlia routing. That violates the architecture's separation between reachability and application trust.

The earlier v1 trust model intentionally rejected untrusted connections because a multiplexed libp2p connection can expose multiple behaviours. Mandatory connectivity infrastructure now creates a legitimate second connection class, so the allowed protocol surface must be explicit.

## Decision

Introduce a local, static **connectivity-infrastructure authorization set**:

```text
transport.connectivity.infrastructure.allowed_peers
```

It is separate from:

```text
trust.allowed_peers
```

Connection class is computed locally:

```text
DataPlaneTrusted
    PeerId in trust.allowed_peers

ConnectivityInfrastructureOnly
    PeerId not in trust.allowed_peers
    AND PeerId in transport.connectivity.infrastructure.allowed_peers

Unauthorized
    neither set
```

If a PeerId belongs to both sets, `DataPlaneTrusted` wins and the peer may also provide configured connectivity services.

### Protocol-admission matrix

| Protocol/role | DataPlaneTrusted | ConnectivityInfrastructureOnly |
|---|---:|---:|
| Noise/Yamux transport | yes | yes |
| Identify / bounded ping | yes | yes |
| AutoNAT v2 probe control | yes when eligible | yes when eligible |
| Circuit Relay v2 reservation/circuit control | yes when eligible | yes when eligible |
| Relay v2 circuit with that peer as application destination | yes | **no** |
| DCUtR with that peer as application destination | yes | **no** |
| GossipSub | yes | **no** |
| direct `/direct/2.0.0` | yes | **no** |
| endpoint directory `/endpoints/1.0.0` | yes | **no** |
| Kademlia routing peer | yes subject to ADR-0009 | **no** |
| Channel/application trust | yes subject to higher policy | **no** |

Infrastructure-only connections are therefore **control-plane connections**, not data-plane membership.

Relay is the one protocol with two rows above, and that pair is the distinction the matrix turns on: **who the exchange is WITH is a different question from who it is FOR.** Reserving a slot on a relay, or renewing that reservation, is an exchange *with* the infrastructure peer for the purpose it was authorized for — eligible. Opening a circuit whose far end *is* that peer uses it as an application destination and is refused, because a circuit carries the data plane by construction. A relay may carry a circuit; it does not thereby become a party the circuit may terminate at. DCUtR has a single row and it is already the destination one — there is no DCUtR control exchange with an infrastructure peer for a second row to describe — so the new relay row states for circuits exactly what that row has always stated for hole punches.

### Enforcement

The root dial gate evaluates both requested dial purpose and destination class. It must not authorize a generic application dial merely because the PeerId is an infrastructure peer.

On an established infrastructure-only connection:

- GossipSub must blacklist/exclude that PeerId from message exchange and mesh participation;
- direct and endpoint-directory managers reject inbound/outbound application operations before payload admission;
- Kademlia never inserts the peer into routing tables under the v1 `data-plane-trusted` routing policy;
- endpoint and Channel state never use the infrastructure allowlist;
- only the explicitly allowed connectivity behaviours plus Identify/bounded liveness may use the connection.

Inbound relayed **destination** connections are evaluated against the authenticated remote application PeerId, not merely the relay PeerId. A trusted relay cannot smuggle an unauthorized source into the data plane.

### Static service references

Every configured static AutoNAT server or relay PeerId must be present in either `trust.allowed_peers` or `transport.connectivity.infrastructure.allowed_peers`. Configuration fails closed otherwise.

Discovery of an address or Identify observation of relay/AutoNAT support never modifies either authorization set.

## Alternatives considered

- put all infrastructure peers in `trust.allowed_peers`;
- allow arbitrary public relay/probe peers;
- operate a second independent Swarm/identity for infrastructure control;
- keep Phase 9 optional to avoid the distinction.

## Consequences

There are now two locally authorized connection classes, but still only one application trust class. Connection diagnostics must report the class and dial purpose without implying infrastructure peers are application members.

Some libp2p behaviours may require adapter-level filtering rather than a single universal per-substream hook. SPIKE-004 must prove the selected rust-libp2p composition can enforce the matrix, especially GossipSub exclusion and behaviour-originated dials.

## Security implications

This decision prevents a relay/probe operator from receiving plaintext trusted GossipSub traffic or invoking direct/endpoint protocols solely because it provides connectivity. It does not make infrastructure anonymous: the relay still sees metadata and can deny service.

A compromised infrastructure-only peer can attack availability, probes, timing, and relay capacity, but must not acquire application-data authority.

## Operational implications

Operators maintain a separate infrastructure PeerId allowlist. UI/CLI must label it `connectivity infrastructure`, never `trusted contact/member`. Moving a peer from infrastructure-only to data-plane trust is an explicit privileged configuration change.

## Implementation implications

`DialAdmissionGate`, `ConnectionManager`, GossipSub peer handling, direct/endpoint admission, Kademlia insertion, AutoNAT server selection, and relay selection all consume the same computed connection-class decision. Do not duplicate independent allowlist interpretations across behaviours.

A runtime class change is reconciled atomically inside the Swarm owner. Infrastructure-only -> data-plane trust removes GossipSub blacklist/exclusion before application participation is allowed; data-plane -> infrastructure-only removes the peer from GossipSub/Kademlia/application protocol state before retaining any eligible connectivity-control connection. If atomic in-place reconciliation is not safe in the pinned library, close the connection and re-establish it under the new class rather than allowing a transient privilege mix.

## Revisit conditions

Revisit if rust-libp2p gains a stronger generic protocol-gating abstraction, if infrastructure is moved to a physically separate daemon/Swarm, or if the project adopts signed role/membership credentials that can replace local static infrastructure authorization.

## Standard service admission

AutoNAT-server and Circuit Relay-server roles use the same standard admission classes: only `DataPlaneTrusted` or `ConnectivityInfrastructureOnly` peers receive project service. Open anonymous relay/probe service is not implied by enabling a server role and would require a separate deployment/security policy. AutoNAT dial-back additionally applies the observed-source-IP/special-address restriction in `transport/libp2p/AUTONAT.md`.

## Amendments

| Date | Amendment | Effect |
|---|---|---|
| 2026-09-03 | Relay circuit toward an infrastructure-only destination | The protocol-admission matrix now answers the case it was silent on: a circuit whose far end is the infrastructure-only peer is refused, as DCUtR toward that peer already was. A dial gate may no longer read the "reservation/circuit control" row as permitting it. |
