# Discovery and connection management are separate

**Status:** Accepted

## Context

Discovery mechanisms produce information; dialing policy depends on trust, current topology, limits, and retry state. Combining them makes providers control the Swarm and prevents composition. The v1 static trust model also needs a clear ruling on whether candidate discovery alone is sufficient to create a data-plane connection.

## Decision

DiscoveryManager owns candidate knowledge. ConnectionManager alone decides dialing, reconnect, backoff, retention, and connection limits. Libp2p-specific execution lives in the backend; normalized connection state is reported upward.

For v1 ordinary data-plane operation, ConnectionManager is **trust-gated**:

- it does not dial a candidate PeerId unless the active `PeerTrustPolicy` authorizes that peer for data-plane connectivity;
- an inbound connection that authenticates to an unauthorized PeerId is closed before that peer participates in the direct-message or GossipSub data plane;
- trust revocation of a connected peer triggers data-plane eviction/disconnect;
- discovery can still observe and retain bounded candidate metadata for unauthorized peers without connecting to them.

Future control-plane protocols that genuinely require limited connectivity to untrusted peers must define an explicit protocol-scoped connection policy rather than weakening this v1 rule implicitly. **ADR-0009's first Kademlia integration does not take that exception:** Kademlia routing/query peers must already be authorized by `PeerTrustPolicy`. Open discovery-only DHT connections remain a separately reviewable future design.

## Alternatives considered

Providers dial directly; DiscoveryManager owns Swarm; Transport core implements multiaddress dialing itself; connect to every discovered peer but gate only local message delivery.

## Consequences

There is an explicit handoff and address-book synchronization cost, but failure ownership is clear and testable. Small/asymmetric trust sets can constrain overlay connectivity; that is an accepted consequence of the v1 deny-by-default model.

## Security implications

Untrusted discovery cannot force unlimited dials or enter the local data-plane overlay merely by being discovered. ConnectionManager applies trust, limits, and policy before dialing/retaining data-plane connections, reducing amplification, connection-storm, and unintended GossipSub exposure risk.

## Operational implications

Dial backoff and limits are consistent across providers. Provider outages do not tear down good trusted connections. Trust reload may intentionally disconnect peers and change mesh topology; this is observable via `TrustPolicyChanged` and peer-disconnect diagnostics.

## Implementation implications

Backend consumes normalized candidate updates and maintains a bounded dialable address book. Before outbound dialing or retaining an inbound data-plane connection, ConnectionManager queries `PeerTrustPolicy`. Successful observations report back for cache hints. Unauthorized candidates remain diagnostics/discovery state, not active transport peers.

## Revisit conditions

Revisit if a backend or future discovery/control protocol requires a tightly coupled connection for non-data-plane purposes; adapt with explicitly scoped connection classes without changing the discovery-versus-connection ownership rule.
