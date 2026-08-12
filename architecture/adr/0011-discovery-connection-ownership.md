# Discovery and connection management are separate

**Status:** Accepted

## Context

Discovery mechanisms produce information; connection policy depends on trust, topology, limits, retry/backoff state, and protocol needs. Combining them makes providers control the Swarm and prevents composition. The v1 static trust model also needs a clear ruling on whether candidate discovery alone is sufficient to create a connection.

A libp2p-specific complication is that a `NetworkBehaviour` can request a dial from the Swarm while driving its own protocol. Kademlia iterative queries are one such case. Therefore "the provider does not call the ordinary dial scheduler" is not sufficient to guarantee that global connection limits and punitive per-peer backoff are respected.

## Decision

DiscoveryManager owns candidate knowledge. **ConnectionManager owns connection policy**, including trust admission, reconnect policy, per-peer backoff, retention, and connection/dial limits. Libp2p-specific execution lives in the backend; normalized connection state is reported upward.

For v1 ordinary data-plane operation, connection policy is trust-gated. Mandatory Phase 9 adds one explicit, narrower connection class from ADR-0036:

- a data-plane candidate PeerId is not intentionally dialed unless the active `PeerTrustPolicy` authorizes that peer for data-plane connectivity;
- a PeerId in `transport.connectivity.infrastructure.allowed_peers` may be dialed/retained **only** for the protocol-scoped Identify/AutoNAT/relay control purposes defined by ADR-0036;
- an authenticated PeerId in neither authorization set is closed/rejected;
- infrastructure-only connectivity never grants direct, GossipSub, endpoint-directory, or Kademlia data-plane participation;
- trust/infrastructure revocation of a connected peer triggers appropriate protocol eviction/disconnect;
- discovery can still observe bounded candidate metadata for unauthorized peers without authorizing a connection.

### Swarm-wide dial admission

ConnectionManager policy applies to **every outbound Swarm dial**, not only calls initiated by the ordinary candidate dial scheduler. `transport-libp2p` therefore includes an internal, synchronous **DialAdmissionGate** (or equivalent root-behaviour hook) fed from ConnectionManager state. Dial failure accounting is split into **peer-scoped policy/backoff state** and **address-scoped reachability/authentication state** so a poisoned address cannot unnecessarily suppress a trusted peer's known-good route. Before a Swarm dial is allowed, the gate enforces at least:

1. destination connection class (`DataPlaneTrusted`, `ConnectivityInfrastructureOnly`, or unauthorized) and the requested dial origin/purpose;
2. current `PeerTrustPolicy` / connectivity-infrastructure authorization when the PeerId is known;
3. per-peer punitive/retry backoff;
4. global pending-dial and connection limits;
5. profile shutdown/drain state;
6. address/path policy checks available at that boundary.

A protocol behaviour such as Kademlia may *request* a dial as part of an iterative query, but it does not own the decision to permit that connection. A denied behaviour-originated dial is observable as policy/backoff/limit denial and must not silently reset ConnectionManager retry state.

### Address-scoped failure and poisoned-address resistance

ConnectionManager tracks failure/backoff for each normalized dial address separately from peer-level punitive state. Recently authenticated-successful addresses are preferred over never-successful addresses. A never-successful address failure does not advance the whole PeerId into punitive backoff while another eligible known-good address exists. If Noise authenticates a different PeerId than the dial target, that is an **address identity mismatch**: close the connection, quarantine that address for 30 minutes by default, record the provenance/source that supplied it, and do not penalize the expected trusted PeerId's peer-level backoff. Peer-level backoff advances only for failures that remain meaningfully peer-scoped after eligible address alternatives are considered.

Address failure state remains bounded by the address-book limits. A successful authenticated connection resets the successful address's failure state; it does not automatically rehabilitate unrelated quarantined addresses.

This preserves the architectural invariant while acknowledging libp2p execution reality:

> ConnectionManager owns connection policy; the Swarm/backend executes dials. Protocol behaviours may generate dial requests only through the same Swarm-wide admission policy.

ADR-0036 is the first explicit protocol-scoped exception: connectivity-infrastructure-only peers may carry Identify/AutoNAT/Circuit-Relay control traffic but remain excluded from the application data plane. **ADR-0009's Kademlia integration does not use this exception** and still requires data-plane trust for routing peers.

## Alternatives considered

Providers dial directly; DiscoveryManager owns Swarm; Transport core implements multiaddress dialing itself; connect to every discovered peer but gate only local message delivery; exempt Kademlia-generated dials from ConnectionManager backoff; attempt to force all protocol queries through the ordinary explicit-dial API.

## Consequences

There is an explicit handoff and policy-snapshot synchronization cost, but failure ownership is clear and testable. Small/asymmetric trust sets can constrain overlay connectivity; that is an accepted consequence of the v1 deny-by-default model.

Kademlia query progress may cause dial requests that were not scheduled by the ordinary candidate dial loop. Those attempts still consume global connection resources and obey backoff through `DialAdmissionGate`. Diagnostics therefore distinguish **dial origin** (`connection-manager`, `kademlia-query`, `relay-reservation`, `relay-circuit`, `autonat-probe`, `dcutr-hole-punch`) where the backend can attribute it.

## Security implications

Untrusted discovery cannot force successful connections merely by being discovered or returned in a Kademlia response. Swarm-wide admission applies trust and resource policy before a behaviour-originated connection is established, reducing amplification, connection storms, and unintended GossipSub exposure. Address-scoped mismatch quarantine prevents an attacker who can inject a bogus address for a trusted PeerId from turning that one address failure into peer-wide punitive backoff while a known-good route remains available.

## Operational implications

Backoff and global limits are consistent across explicit candidate dials and protocol-generated dials. Provider outages do not tear down good trusted connections. Trust reload may intentionally disconnect peers and change mesh/routing topology; this is observable via `TrustPolicyChanged` and peer-disconnect diagnostics.

SPIKE-003 must measure Kademlia-originated dial attempts under this gate. Mandatory `SPIKE-004` must do the same for AutoNAT/relay/DCUtR behaviour-originated dials and prove that infrastructure-only authorization cannot leak into GossipSub/direct/endpoint/Kademlia participation.

## Implementation implications

Backend consumes normalized candidate updates and maintains a bounded dialable address book containing provenance, last authenticated success, address-scoped failure/backoff, and identity-mismatch quarantine. ConnectionManager publishes an atomically readable policy snapshot to the Swarm task / `DialAdmissionGate`; the gate must not block on async policy calls while the Swarm is being polled. Policy revision changes invalidate stale authorization/backoff snapshots promptly.

Before retaining an inbound data-plane connection, ConnectionManager applies the same current authorization policy. Successful observations report back for cache hints. Unauthorized candidates remain diagnostics/discovery state, not active transport peers.

The Kademlia driver remains Swarm-owned. Its iterative queries may produce `ToSwarm::Dial` requests, but those requests are subject to `DialAdmissionGate`; the provider itself still does not dial.

## Revisit conditions

Revisit if a backend cannot enforce a root-level outbound dial gate, if a future protocol needs an explicit non-data-plane connection class, or if empirical evidence shows behaviour-generated dial attribution/backoff cannot be enforced without a different Swarm composition. Do not weaken discovery-versus-connection ownership implicitly.
