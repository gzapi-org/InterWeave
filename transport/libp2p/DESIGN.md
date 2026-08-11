# libp2p backend design

## v1 stack

```text
TCP
 -> Noise XX connection security
 -> Yamux stream multiplexing
 -> Identify (connection metadata/address observation)
 -> GossipSub (broadcast)
 -> request-response custom codec (direct)
```

Discovery behaviors are composed behind `DiscoveryProvider`; they are not directly exposed to transport consumers.

## Internal ownership

One backend event loop owns the libp2p Swarm. Commands enter through a bounded channel. Swarm events are normalized immediately into backend-internal events and forwarded through bounded channels; no Claude callbacks execute on the Swarm loop.

`ConnectionManager` is split:

- backend-neutral policy inputs: target peer, trust decision, reconnect intent, limits, backoff class;
- libp2p-specific execution: multiaddress selection, Swarm dialing, connection IDs, protocol negotiation.

The concrete manager therefore lives in the libp2p backend but presents only neutral state upstream.

## Trust-gated data-plane connections

Discovery observations populate candidate knowledge independently of trust. Before ConnectionManager dials or retains an inbound ordinary data-plane connection, it queries the active `PeerTrustPolicy`. Unauthorized PeerIds may remain in bounded discovery diagnostics/address state but are not admitted to direct or GossipSub participation.

Trust revocation closes an affected data-plane connection. A future protocol that needs a limited control-plane connection to an untrusted peer must define a separate scoped policy rather than reusing ordinary data-plane admission.

## Address sources

DiscoveryManager emits candidate observations. The backend maintains its own dialable address book derived from those observations and connected-peer Identify information. Discovery implementations do not mutate the Swarm directly.
