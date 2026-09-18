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

**Note (2026-09-18, step 8).** The pinned `libp2p-dcutr` 0.14.1 has none of these knobs — `Behaviour::new` takes a PeerId — and no notion of an attempt: its `MAX_NUMBER_OF_UPGRADE_ATTEMPTS = 3` is a retry count per relayed connection on the initiating side, and SPIKE-004 measured that one punch is a dial at both ends, so no single gate sees the attempt. The limits above are therefore enforced by `HolePunchScope` (`crates/transport/libp2p/src/hole_punch.rs`) around the crate, and the UNIT is the relayed connection: an attempt begins at its establishment, where §2's eligibility is decided — a relayed connection that fails it is given a handler that speaks no DCUtR, so the far end's CONNECT finds no protocol — and ends on the crate's outcome, on any direct connection to the peer coming up (the crate reports a success only for its own dial, and the initiating end of a punch the responder's dial completed would otherwise report the attempt failed beside a working path), on the relayed connection closing (no cooldown, §7), or at a ninety-second horizon, since the crate tells the responding end nothing of a failed punch. Two things the crate does that §2 does not say: it learns the addresses it sends in CONNECT only from what a peer's Identify observed, so the substrate also offers the listeners this profile bound; and a punch dial's failure makes it open another CONNECT round, so the wrapper forwards such a failure only while the attempt is in flight. Which peers may punch at all is the data-plane class gate's decision, outside the wrapper; `direct_stability` is carried in the settings and read by step 9.

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

**A candidate is dialled inside an address-class boundary (ADR-0052; architect-cto's decision of 2026-09-18).** Trust in the far end is trust with the data plane, not with where this host opens sockets: a punch candidate is a peer-supplied address this profile would TCP-connect to, the same shape as a dial-back target, and `AUTONAT.md` §7's boundary applies to it with one difference the punch needs:

- a candidate is dialled only if it is a literal IP multiaddr — no DNS name (the resolver is an oracle), no `p2p-circuit` component;
- refused always, whoever supplies it: loopback, unspecified, multicast, link-local (so `169.254.169.254` never), and the other special-use ranges §7 names;
- a global address is admitted — the ordinary punch;
- RFC 1918 IPv4 and IPv6 ULA are admitted ONLY when this node itself holds a non-loopback listener in a private range of the same family. A LAN punch is the legitimate case for a private candidate and both ends of one sit on private networks; a host with only global listeners has no LAN to punch across, and a private candidate handed to it is exactly the internal-network probe §7 refuses. This is the one place §6 and §7 differ;
- there is no source-equality clause: unlike a dial-back, a punch candidate legitimately differs from the relayed connection's observed address — that is what NAT means — so §7's second bullet has no analogue here.

**Where it runs (2026-09-19, step 8).** `is_punchable_address` (`crates/transport/runtime/src/reachability.rs`, beside `is_probeable_address`, with a test that everything §7 refuses §6 refuses too) is applied by `HolePunchScope` three times: to the candidates the Swarm reports (a peer's Identify observed this profile on loopback as readily as on a public address), so what this profile SENDS in a CONNECT is inside the boundary; to the listeners the runtime offers, for the same reason; and at the pending outbound hook of every punch dial, before any socket, the `ProbeServer` shape. The hook admits or denies a dial whole — it can add addresses to the Swarm's list, never remove one — so a candidate list carrying one refused address is refused with it; the attempt ends `refused_by_class` (§8's `dcutr_attempts_total{outcome=refused_by_class}`), the diagnostic names the class and never the address, and the peer enters the cooldown as for any failure. The outbound gate's hook runs first and takes its ticket back on the refusal. On loopback the substrate therefore shows a loopback candidate REFUSED, not a punch made; the punch-made test runs over a private-range pair, which is what two runtimes on one host's private address are.

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
