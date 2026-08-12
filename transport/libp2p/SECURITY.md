# libp2p security boundary

## Noise

rust-libp2p Noise authenticates the connection PeerId and encrypts each peer link. It does not authenticate human/application EndpointIds, authorize application actions, define channel membership, or provide GossipSub end-to-end secrecy.

## Connection admission

```text
Noise-authenticated PeerId
 -> PeerTrustPolicy
 -> retain ordinary data-plane connection OR close
```

All ordinary direct, endpoint-directory, GossipSub, and Kademlia participation stays under this profile data-plane trust boundary.

## Direct endpoint admission

```text
trusted Noise-authenticated PeerId
 -> DirectMessageV2 structural validation
 -> source EndpointId grammar (peer-asserted route label)
 -> explicit/default destination resolution
 -> endpoint inbound policy intersection
 -> active local endpoint lease
 -> size/rate/dedup/queue admission
 -> exactly one local direct event
 -> AcceptedV2
```

EndpointId does not add a cryptographic authentication layer. `source_endpoint=human` means only that the authenticated PeerId sent that route string.

Endpoint policy cannot widen PeerTrustPolicy.

## Endpoint directory admission

Only trusted peers may query. Response contains only active, opt-in, requester-admissible EndpointIds and no application labels/roles. Directory results are short-lived advisory metadata.

## GossipSub path

Unchanged ADR-0029 mapping:

```text
trusted neighbor
 -> signed message/source validation
 -> original publisher trust
 -> Reject objectively invalid
 -> Ignore valid but locally unauthorized
 -> Accept valid + authorized
```

EndpointId is not inserted into transport GossipSub messages.

## Group encryption

Still deferred. Human-client availability does not change the per-hop Noise / trusted-forwarder plaintext boundary.


## Connectivity infrastructure admission

Mandatory Phase 9 adds a narrower connection class:

```text
Noise-authenticated PeerId
 -> connection-class policy
    -> DataPlaneTrusted: ordinary application protocols + eligible control protocols
    -> ConnectivityInfrastructureOnly: Identify/AutoNAT/Relay control only
    -> Unauthorized: close/deny
```

Infrastructure-only peers are excluded from GossipSub peer participation, direct v2, endpoint directory, Kademlia routing, and DCUtR as an application destination. Every behaviour-originated dial still passes the root origin/class gate.

For a relayed application connection, authenticate and authorize the **end PeerId** independently from the relay PeerId. A relay's infrastructure authorization is not transitive.

Relay paths preserve the secure end-peer transport but do not provide anonymity: relay operators remain metadata/availability observers. AutoNAT probe results are reachability evidence only and never feed trust.
