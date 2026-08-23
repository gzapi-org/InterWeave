# libp2p security boundary

## Pre-Noise listener admission

Before Noise completes there is no authenticated PeerId, so trust allowlists cannot protect the listener. The backend therefore applies a bounded pre-authentication admission layer to every directly accepted transport connection: 64 pending inbound handshakes globally and 8 per source-address bucket by default, 10-second handshake timeout, 30 starts/minute per source bucket, and 600 starts/minute globally. IPv4 buckets use the source address; IPv6 uses /64 by default. An IPv4-mapped IPv6 source (`::ffff:a.b.c.d`, which is how a dual-stack listener reports an IPv4 peer) is an IPv4 source and buckets by its IPv4 address: applying the /64 rule to it would place the entire IPv4 Internet in the single `::/64` bucket. The deprecated IPv4-compatible form (`::a.b.c.d`) is not unwrapped, because that range overlaps real IPv6 addresses such as `::1`. If a relayed inbound path does not expose the original source IP, it consumes a per-authenticated-relay transport/PeerId source bucket **and** the global pending/rate budget. This deliberately lets one abusive relay exhaust its own bucket rather than obtaining unbounded circuit-specific buckets; relay-server circuit quotas are complementary.

Admission failure closes early and does not create/update PeerId trust, discovery, or peer punitive-backoff state because no authenticated PeerId exists yet. Backend connection-limit behavior should be used where it provides the required bound, supplemented by listener/front-door rate limiting when needed. Internet-facing infrastructure deployments should also use deployment firewall/eBPF/OS controls because application-layer limits cannot eliminate handshake CPU exhaustion or distributed-source attacks.

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

Endpoint policy cannot widen PeerTrustPolicy. After Noise/profile trust admission, inbound direct v2 requests also pass a mandatory trusted-peer token bucket (default 120 requests/minute with burst 32 per PeerId, plus 1200/minute with burst 256 globally) before endpoint routing/queue work. Rate-limit overflow maps to coarse `overloaded`; source EndpointId is never used as the rate-limit principal.

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

## Address identity mismatch

A candidate address is not proof that it belongs to its advertised PeerId. If a dial intended for trusted PeerId A completes Noise as PeerId B, the connection is closed and the **address** is quarantined/failure-scored. That mismatch must not advance A's peer-wide punitive backoff while another known-good A address remains eligible. ConnectionManager records bounded provenance so poisoned mDNS/Kademlia/cache/static observations can be diagnosed without converting discovery metadata into trust.
