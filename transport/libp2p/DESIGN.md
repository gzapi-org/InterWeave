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

- backend-neutral policy inputs: target peer, reconnect intent, limits, backoff class;
- libp2p-specific execution: multiaddress selection, Swarm dialing, connection IDs, protocol negotiation.

The concrete manager therefore lives in the libp2p backend but presents only neutral state upstream.

## Address sources

DiscoveryManager emits candidate observations. The backend maintains its own dialable address book derived from those observations and connected-peer Identify information. Discovery implementations do not mutate the Swarm directly.
