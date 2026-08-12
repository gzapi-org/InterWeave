# DCUtR / hole-punch design

Status: **Required standard-v1 libp2p backend component**

## 1. Purpose

DCUtR attempts to upgrade an already working relayed application-peer connection to a direct connection. It is an optimization of path quality/dependency, not a prerequisite for message correctness: relay remains the fallback when hole punching fails.

## 2. Eligibility

A hole-punch attempt is eligible only when:

- there is a current relayed connection to a `DataPlaneTrusted` application PeerId;
- the connection/protocol negotiation indicates DCUtR support;
- no stable preferred direct path already exists;
- per-peer cooldown has elapsed;
- global/per-peer attempt permits are available;
- root dial/connection policy permits the generated work.

Never initiate DCUtR merely with `ConnectivityInfrastructureOnly` as the application destination.

## 3. Limits

Defaults:

```text
max_inflight            4
max_inflight_per_peer   1
retry_cooldown          5m
direct_stability        10s
```

All dials created by hole punching use origin `dcutr-hole-punch` and count against total/per-peer connection limits.

## 4. State machine

```text
relayed
  |
  | eligible
  v
punching -------- failure --------> relayed + cooldown
  |
 success
  v
direct_candidate
  |
  | stable for configured interval
  v
direct_preferred ----> retire redundant relay path when policy permits
```

If the new direct connection dies during the stability interval, retain/re-establish relay preference and apply normal cooldown/backoff.

## 5. Stream semantics

Do **not** claim transparent migration of an already-open direct-v2 request/response stream, GossipSub substream, or endpoint-directory stream from relay to direct.

Connection/path preference affects subsequent stream/connection selection. Application delivery/retry semantics remain those of the existing transport contracts.

## 6. Address exchange and privacy

Hole punching necessarily coordinates candidate network addresses between the two end peers through the relayed connection. Treat those addresses as transport metadata. Do not expose them to Claude Channel content or EndpointId directory responses.

DCUtR does not authenticate a human/application endpoint. The PeerId security session and profile trust remain authoritative.

## 7. Failure behavior

- protocol unsupported -> keep relay; mark peer/path ineligible until fresh protocol evidence/change;
- timeout or simultaneous-open failure -> keep relay, record failure class, enter cooldown;
- generated direct dial denied by root policy/resource limit -> keep relay, do not reset punitive backoff;
- relay disappears during punch -> normal connectivity recovery may establish another relay/direct path; no durability guarantee;
- success -> wait for direct stability, then emit `PeerPathChanged{relayed->direct, reason=dcutr}` for the existing logical peer before redundant relay retirement; do not emit a second `PeerConnected`.

## 8. Observability

```text
dcutr_attempts_total{outcome}
dcutr_inflight
dcutr_cooldown_peers
direct_upgrade_success_total
direct_upgrade_stability_failures_total
peer_path{direct|relayed|none}
```

Diagnostics attribute resulting dials to `dcutr-hole-punch`.

## 9. Tests

Required tests include success, NAT-induced failure, unsupported peer, concurrent-limit exhaustion, cooldown enforcement, root dial denial, relay survival after failed punch, direct stability rollback, network change, exactly one logical `PeerConnected` plus `PeerPathChanged` on stable upgrade, and no change to Model-B EndpointId routing semantics.
