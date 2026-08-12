# libp2p backend design

## v1 stack

```text
TCP
 -> Noise XX connection security
 -> Yamux stream multiplexing
 -> Identify (connection metadata/address observation)
 -> GossipSub (broadcast)
 -> request-response custom codec (direct)
 -> optional Kademlia behaviour (peer-routing discovery only; default disabled)
```

Discovery behaviors are composed behind `DiscoveryProvider`; they are not directly exposed to transport consumers.

## Internal ownership

One backend event loop owns the libp2p Swarm. Commands enter through a bounded channel. Swarm events are normalized immediately into backend-internal events and forwarded through bounded channels; no Claude callbacks execute on the Swarm loop.

`ConnectionManager` is the policy owner for trust admission, reconnect/backoff, retention, and global/per-peer limits. Low-level network execution remains in the Swarm/backend. This distinction matters because a `NetworkBehaviour` can request a dial on its own.

The backend therefore maintains a synchronous `DialAdmissionGate` in the root Swarm behaviour path. ConnectionManager publishes current policy state to that gate. Explicit scheduler dials **and** behaviour-originated dials (including Kademlia iterative-query dials) must pass the same trust/backoff/limit/shutdown checks before connection establishment.

## Trust-gated data-plane connections

Discovery observations populate candidate knowledge independently of trust. Before an ordinary outbound connection is admitted, `DialAdmissionGate` applies ConnectionManager's current `PeerTrustPolicy`/backoff/limit state. Before an inbound connection is retained, the backend applies the same authorization policy. Unauthorized PeerIds may remain in bounded discovery diagnostics/address state but are not admitted to direct, GossipSub, or first-generation Kademlia participation.

Trust revocation closes an affected data-plane connection. A future protocol that needs a limited control-plane connection to an untrusted peer must define a separate scoped policy rather than reusing ordinary data-plane admission.

## Address sources

DiscoveryManager emits candidate observations. The backend maintains its own dialable address book derived from those observations and connected-peer Identify information. Discovery implementations do not mutate the Swarm directly.


## Optional Kademlia driver

The concrete Kademlia `NetworkBehaviour` lives in this backend because the Swarm has one owner. The generic provider remains outside the Swarm. `transport-libp2p` and `discovery-kademlia` communicate through the tiny neutral internal `kademlia-control-api`; the provider does not compile-depend on this backend or on libp2p.

The driver applies a custom private protocol namespace, explicit client/server mode, manual K-bucket insertion, record filtering/no-record policy, and query/routing events described in `../../docs/architecture/kademlia-integration.md`. Iterative queries may emit behaviour-originated dial requests; these are tagged/observed where possible and must pass `DialAdmissionGate`, including trust and punitive backoff.

The first integration does not use a special untrusted control-plane connection class: Kademlia routing/query peers must already pass `PeerTrustPolicy`, preserving the multiplexed-connection confidentiality assumptions used by GossipSub/direct transport.
