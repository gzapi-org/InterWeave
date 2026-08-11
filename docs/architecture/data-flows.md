# Data and event flows

## Broadcast outbound

```text
Claude tool broadcast(channel,payload)
 -> bridge validates ChannelId/payload size
 -> IPC request Broadcast
 -> daemon TransportRuntime
 -> trust/subscription/config checks
 -> Libp2pBackend::publish
 -> signed GossipSub topic message
 -> connected subscribed peers
```

A successful local tool result means the backend accepted the publish operation. It does **not** mean every peer received or processed the message.

## Broadcast inbound

```text
GossipSub message
 -> signature / protocol validation
 -> source PeerId extraction
 -> PeerTrustPolicy
 -> payload + rate + duplicate limits
 -> normalized MessageReceived{mode=broadcast,...}
 -> bounded IPC client queue
 -> bridge
 -> notifications/claude/channel {content, meta}
```

## Direct outbound

```text
Claude tool send(peer,payload)
 -> IPC Send
 -> ConnectionManager resolves existing/known connection
 -> request-response substream /claude-p2p-channel/direct/1.0.0
 -> remote runtime validates + admits + queues local event
 -> remote responds ACCEPTED | REJECTED(reason)
 -> local tool returns transport result
```

`ACCEPTED` means the remote transport accepted the payload into its bounded local event path. It does not prove Claude consumed it.

## Directed inbound

The same trust/resource pipeline is applied before delivery. A direct request can be rejected without producing a Channel event. No automatic retry occurs at the direct-protocol layer.

## Reply

Inbound Channel metadata carries an opaque short-lived `reply_token` created by the bridge:

- direct inbound -> token resolves to direct `source_peer`;
- broadcast inbound -> token resolves to the same broadcast `channel`.

`reply` follows that route. Claude does not manipulate Multiaddr, connection IDs, or mesh peers. Explicit `send` and `broadcast` remain available when a different route is desired.
