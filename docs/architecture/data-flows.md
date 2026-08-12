# Data and event flows

## Broadcast inbound

```text
GossipSub message
 -> signature/source validation
 -> GossipSub Accept|Ignore|Reject mapping (ADR-0029)
 -> profile trust admission
 -> payload/resource limits
 -> broadcast dedup
 -> normalized MessageReceived(channel,...)
 -> IPC fan-out only to clients with local join reference
 -> Claude Channel event and/or human-client event
```

`channels.desired` may keep the backend mesh warm with zero joined clients. With no joined local consumer, local delivery is dropped and never buffered/replayed.

## Direct endpoint-addressed outbound

```text
local app owns EndpointId lease
 -> send({peer, endpoint?}, payload)
 -> endpoint outbound narrowing policy
 -> profile PeerTrustPolicy
 -> ConnectionManager / dial-admission policy
 -> request-response /claude-p2p-channel/direct/2.0.0
       source_endpoint = caller lease
       destination_endpoint = explicit or absent(default request)
 -> remote route admission
 -> AcceptedV2(resolved_endpoint) | RejectedV2(...)
 -> local result
```

Omitted destination endpoint asks the remote node for its configured default. It never means local fan-out.

## Direct endpoint-addressed inbound

```text
Noise-authenticated remote PeerId
 -> profile trust
 -> DirectMessageV2 framing/limits
 -> resolve explicit destination or profile default
 -> endpoint inbound narrowing policy
 -> active EndpointId lease?
 -> endpoint-aware dedup
 -> target client's bounded event queue has capacity?
 -> enqueue exactly one MessageReceived
 -> AcceptedV2(resolved_endpoint)
```

Failure to resolve/admit a route produces coarse remote `no_route`. An unavailable endpoint is not acknowledged then dropped and is never buffered for later.

## Human vs Claude under one PeerId

```text
Remote peer
   |
   +-- send(P/human) ---> daemon ---> human IPC lease only
   |
   +-- send(P/claude) --> daemon ---> Claude bridge IPC lease only
```

Broadcast remains independent:

```text
channel project-alpha
  -> human receives only if human client joined
  -> Claude receives only if Claude bridge joined
```

## Endpoint directory

```text
human client -> peer_endpoints(P)
 -> profile trust check
 -> ConnectionManager
 -> /claude-p2p-channel/endpoints/1.0.0
 -> remote snapshot of active advertise=true routes allowed for requester
 -> bounded in-memory cache / UI result
```

Directory data never grants trust and never carries app/human identity assertions.

## Direct reply route

Inbound at local endpoint `claude` from `RemotePeer/human` yields:

```text
reply route:
  remote_peer = RemotePeer
  remote_endpoint = human
  local_endpoint = claude
  local_lease_epoch = E
```

Reply succeeds only while the bridge still owns `claude` at epoch `E`. It never falls back to another endpoint or remote default.

## Broadcast reply route

Broadcast reply token maps only to ChannelId/mode. The caller must still be joined; otherwise `ChannelNotJoined`.


## Mandatory Internet reachability flows

### Reachability classification and relay readiness

```text
configured/Identify-authorized probe servers
        |
        v
AutoNAT v2 probes --bounded evidence--> ReachabilityManager
        |                                  |
        |                                  +--> direct_inbound state
        |                                  |
        v                                  v
authorized relay candidates ----------> RelayManager
                                           |
                                           +--> maintain target reservations
                                           +--> add/remove active relay addresses
                                           +--> ConnectivityChanged
```

### Outbound application connection

```text
trusted target PeerId
      |
      v
ConnectionManager / DialAdmissionGate
      |
      +--> direct candidate first -------------------+
      |                                              |
      +-- after bounded head start --> relay route --+--> authenticated peer connection
                                                     |
                                                     +--> direct v2 / endpoint directory / GossipSub
```

Relay PeerId authorization is checked separately from the authenticated application target. A permitted relay never authorizes the remote application peer.

### DCUtR upgrade

```text
working relayed connection
       |
       v
DCUtRManager --bounded/cooldown--> simultaneous direct attempt
       |
       +-- failure --> retain relay path
       |
       `-- success --> direct path --stability timer--> retire redundant relay path
```

There is no claim that existing streams migrate. New work prefers the stable direct path.
