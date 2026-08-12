# Data and event flows

## Broadcast outbound

```text
Claude tool broadcast(channel,payload)
 -> bridge validates ChannelId/effective payload size
 -> caller join-reference check
 -> IPC request Broadcast
 -> daemon TransportRuntime
 -> subscription/config checks
 -> Libp2pBackend::publish
 -> signed GossipSub topic message
 -> trusted connected subscribed peers
```

If the calling bridge has no active join reference, the operation fails as `ChannelNotJoined` before backend publication. A successful local tool result means the backend accepted the publish operation. It does **not** mean every peer received or processed the message.

## Broadcast inbound

```text
trusted direct neighbor connection
 -> GossipSub signed message
 -> decode / objective protocol + signature/source validation
 -> original publisher PeerId extraction
 -> PeerTrustPolicy(original publisher)
 -> validation result per ADR-0029
      Reject: invalid -> stop + invalid diagnostics/scoring semantics
      Ignore: valid but unauthorized -> stop, no local delivery/forwarding
      Accept: valid + authorized -> continue
 -> payload + rate + normalized duplicate limits
 -> MessageReceived{mode=broadcast,...}
 -> local-interest routing
      joined clients for this channel -> independent bounded IPC queues
      no joined clients -> local drop diagnostic (no buffer/replay)
 -> bridge(s)
 -> notifications/claude/channel {content, meta}
```

A profile-level `channels.desired` subscription may keep the GossipSub mesh warm when no bridge is joined; it does not create a local consumer or offline mailbox.

The immediate propagation peer and the original publisher are different identities and remain distinct in backend diagnostics.

## Direct outbound

```text
Claude tool send(peer,payload)
 -> bridge validates effective payload size
 -> PeerTrustPolicy(peer)
      unauthorized -> UnauthorizedPeer, no dial
 -> IPC Send
 -> ConnectionManager resolves existing/known authorized connection
 -> if candidates absent: PeerUnknown
 -> else dial/reuse under deadline
 -> request-response substream /claude-p2p-channel/direct/1.0.0
 -> remote runtime validates + admits + queues local event
 -> remote responds ACCEPTED | REJECTED(reason)
 -> local tool returns transport result
```

If candidate addresses exist but the authorized peer cannot be connected/protocol-negotiated within the deadline, return `PeerUnreachable`. `ACCEPTED` means the remote transport accepted the payload into its bounded local event path. It does not prove Claude consumed it.

## Directed inbound

Inbound connection retention is trust-gated. The direct request receives the same source trust/resource pipeline before delivery. A request can be rejected without producing a Channel event. No automatic retry occurs at the direct-protocol layer.

After admission, direct `MessageReceived` is duplicated to every currently connected IPC client with message-event capability. With two bridges sharing one profile, both may therefore produce Channel events and independent reply tokens for the same network message. There is no hidden local-primary selection. If no local client is connected, the event is dropped after transport handling and is not buffered for later delivery.

## Reply

Inbound Channel metadata carries an opaque short-lived `reply_token` created by the bridge:

- direct inbound -> token resolves to direct `source_peer`;
- broadcast inbound -> token resolves to the same broadcast `channel`.

`reply` follows that route subject to current policy. A broadcast reply after the calling bridge has left the channel fails as `ChannelNotJoined`; the token does not recreate a subscription. Claude does not manipulate Multiaddr, connection IDs, or mesh peers. Explicit `send` and `broadcast` remain available when a different route is desired.


## Optional Kademlia discovery flow

Kademlia remains disabled by default. When a supporting build is explicitly enabled:

```text
peer-cache/static/mDNS candidate + protocol-capability hints
 -> DiscoveryManager / KademliaDiscovery seed eligibility
 -> PeerTrustPolicy + address/exact-protocol checks
 -> neutral bounded kademlia-control-api port
 -> Swarm-owned Kademlia driver
 -> manual Behaviour::add_address
 -> bootstrap / get_n_closest_peers query
 -> any behaviour-originated dial request
      -> DialAdmissionGate(ConnectionManager trust/backoff/limits)
 -> query progress / PeerInfo results
 -> KademliaDiscovery normalization + TTL
 -> CandidatePeer{source=kademlia}
 -> DiscoveryManager merge
```

The provider does not own dial policy and Kademlia results never grant trust. The underlying Kademlia behavior may request dials while driving an iterative query, but those attempts are subject to the same root admission gate as ordinary dials. Client-mode DHT peers are not treated as generally discoverable through peer routing; targeted lookup requires fresh advisory evidence that the trusted target advertised the exact current Kademlia server protocol.
