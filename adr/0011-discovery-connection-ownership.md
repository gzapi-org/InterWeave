# Discovery and connection management are separate

**Status:** Accepted

## Context

Discovery mechanisms produce information; connection policy depends on trust, topology, limits, retry/backoff state, and protocol needs. Combining them makes providers control the Swarm and prevents composition. The v1 static trust model also needs a clear ruling on whether candidate discovery alone is sufficient to create a connection.

A libp2p-specific complication is that a `NetworkBehaviour` can request a dial from the Swarm while driving its own protocol. Kademlia iterative queries are one such case. Therefore "the provider does not call the ordinary dial scheduler" is not sufficient to guarantee that global connection limits and punitive per-peer backoff are respected.

## Decision

DiscoveryManager owns candidate knowledge. **ConnectionManager owns connection policy**, including trust admission, reconnect policy, per-peer backoff, retention, and connection/dial limits. Libp2p-specific execution lives in the backend; normalized connection state is reported upward.

For v1 ordinary data-plane operation, connection policy is trust-gated:

- a candidate PeerId is not intentionally dialed unless the active `PeerTrustPolicy` authorizes that peer for data-plane connectivity;
- an inbound connection that authenticates to an unauthorized PeerId is closed before that peer participates in direct, GossipSub, or configured Kademlia protocol activity;
- trust revocation of a connected peer triggers data-plane eviction/disconnect;
- discovery can still observe and retain bounded candidate metadata for unauthorized peers without connecting to them.

### Swarm-wide dial admission

ConnectionManager policy applies to **every outbound Swarm dial**, not only calls initiated by the ordinary candidate dial scheduler. `transport-libp2p` therefore includes an internal, synchronous **DialAdmissionGate** (or equivalent root-behaviour hook) fed from ConnectionManager state. Before a Swarm dial is allowed, the gate enforces at least:

1. current `PeerTrustPolicy` authorization when the PeerId is known;
2. per-peer punitive/retry backoff;
3. global pending-dial and connection limits;
4. profile shutdown/drain state;
5. address-policy checks available at that boundary.

A protocol behaviour such as Kademlia may *request* a dial as part of an iterative query, but it does not own the decision to permit that connection. A denied behaviour-originated dial is observable as policy/backoff/limit denial and must not silently reset ConnectionManager retry state.

This preserves the architectural invariant while acknowledging libp2p execution reality:

> ConnectionManager owns connection policy; the Swarm/backend executes dials. Protocol behaviours may generate dial requests only through the same Swarm-wide admission policy.

Future control-plane protocols that genuinely require limited connectivity to untrusted peers must define an explicit protocol-scoped connection class rather than weakening this v1 rule implicitly. **ADR-0009's first Kademlia integration does not take that exception.**

## Alternatives considered

Providers dial directly; DiscoveryManager owns Swarm; Transport core implements multiaddress dialing itself; connect to every discovered peer but gate only local message delivery; exempt Kademlia-generated dials from ConnectionManager backoff; attempt to force all protocol queries through the ordinary explicit-dial API.

## Consequences

There is an explicit handoff and policy-snapshot synchronization cost, but failure ownership is clear and testable. Small/asymmetric trust sets can constrain overlay connectivity; that is an accepted consequence of the v1 deny-by-default model.

Kademlia query progress may cause dial requests that were not scheduled by the ordinary candidate dial loop. Those attempts still consume global connection resources and obey backoff through `DialAdmissionGate`. Diagnostics therefore distinguish **dial origin** (`connection-manager`, `kademlia-query`, other future protocol) where the backend can attribute it.

## Security implications

Untrusted discovery cannot force successful connections merely by being discovered or returned in a Kademlia response. Swarm-wide admission applies trust and resource policy before a behaviour-originated connection is established, reducing amplification, connection storms, and unintended GossipSub exposure.

## Operational implications

Backoff and global limits are consistent across explicit candidate dials and protocol-generated dials. Provider outages do not tear down good trusted connections. Trust reload may intentionally disconnect peers and change mesh/routing topology; this is observable via `TrustPolicyChanged` and peer-disconnect diagnostics.

SPIKE-003 must measure Kademlia-originated dial attempts, denials, and successful connections under active backoff and connection-limit pressure because these attempts originate below the ordinary scheduler API.

## Implementation implications

Backend consumes normalized candidate updates and maintains a bounded dialable address book. ConnectionManager publishes an atomically readable policy snapshot to the Swarm task / `DialAdmissionGate`; the gate must not block on async policy calls while the Swarm is being polled. Policy revision changes invalidate stale authorization/backoff snapshots promptly.

Before retaining an inbound data-plane connection, ConnectionManager applies the same current authorization policy. Successful observations report back for cache hints. Unauthorized candidates remain diagnostics/discovery state, not active transport peers.

The Kademlia driver remains Swarm-owned. Its iterative queries may produce `ToSwarm::Dial` requests, but those requests are subject to `DialAdmissionGate`; the provider itself still does not dial.

## Revisit conditions

Revisit if a backend cannot enforce a root-level outbound dial gate, if a future protocol needs an explicit non-data-plane connection class, or if empirical evidence shows behaviour-generated dial attribution/backoff cannot be enforced without a different Swarm composition. Do not weaken discovery-versus-connection ownership implicitly.
