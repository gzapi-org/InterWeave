# Mandatory Internet reachability design

Status: normative architecture for the standard-v1 rust-libp2p backend. No implementation exists in this repository.

ADR-0035 supersedes the earlier conditional reachability scope. The standard v1 build includes **AutoNAT v2 client + Circuit Relay v2 client + DCUtR**. Relay-server and AutoNAT-server roles are supported infrastructure modes but are not automatically enabled on every peer.

## 1. Goals

The reachability layer must let an authorized peer participate when it is:

- directly reachable on a public address;
- behind ordinary home/office NAT;
- behind a firewall that permits outbound connections;
- behind NATs where hole punching works;
- behind NATs/CGNAT where hole punching does not work but a relay is reachable;
- moving between networks while the daemon/PeerId remains stable.

It must preserve these existing invariants:

- PeerId remains the only transport authentication principal;
- EndpointId routing is above connection establishment;
- discovery does not grant trust;
- relay/probe infrastructure does not automatically become data-plane trust;
- ConnectionManager/root dial admission still owns connection policy;
- no durable message queue appears merely because a path is temporarily unavailable;
- broadcast remains GossipSub and direct remains `/direct/2.0.0`.

It does **not** promise universal direct connectivity. Relay fallback is the standard availability path when direct reachability/hole punching fail.

## 2. Selected protocol stack

| Mechanism | Standard-v1 role |
|---|---|
| TCP listen/dial | required base transport |
| Noise + Yamux | required secure/multiplexed connection |
| Identify | required capability/address observation; explicitly wired |
| AutoNAT v2 client | **required** direct-reachability evidence |
| AutoNAT v2 server | supported explicit infrastructure role |
| Circuit Relay v2 client transport | **required** relay fallback/reservation client |
| Circuit Relay v2 server | supported explicit infrastructure role |
| DCUtR | **required** relayed-to-direct upgrade attempt |
| mDNS | optional LAN peer discovery |
| static bootstrap | discovery only, not relay authority |
| Kademlia | default-on peer-routing discovery when configured; no relay/provider records |

AutoNAT v1 is not the new implementation target. The design targets AutoNAT v2 and uses SPIKE-004 to pin the exact rust-libp2p API/version behavior before production implementation.

## 3. Component ownership

```text
                         TransportRuntime
                              |
                              v
                      ConnectionManager
                              |
                +-------------+--------------+
                |                            |
        ReachabilityManager             AddressBook
                |                            |
       +--------+---------+                  |
       |        |         |                  |
    AutoNAT   Relay     DCUtR <--------------+
       |      Manager   Manager
       |        |         |
       +--------+---------+
                |
                v
           Swarm task
```

### ReachabilityManager owns

- normalized direct-inbound reachability evidence;
- AutoNAT observation aggregation/expiry;
- aggregate connectivity health;
- target relay reservation count based on current direct evidence;
- state transitions caused by network/address change.

It must not own PeerId trust, peer discovery, endpoint routing, or application retries.

### RelayManager owns

- eligible relay candidates;
- reservation acquisition/renewal/failover;
- active reservation state and relay-derived listen addresses;
- relay-server resource state when that role is configured;
- relay circuit diagnostics.

It does not grant trust and is not a DiscoveryProvider.

### DCUtRManager owns

- eligibility/cooldown for direct-upgrade attempts on authenticated trusted relayed connections;
- bounded concurrent hole-punch attempts;
- success/failure diagnostics;
- notification to ConnectionManager that a direct path exists.

It never closes a working relay path merely because an upgrade attempt failed.

### ConnectionManager remains policy owner

ConnectionManager:

- decides/records preferred path class;
- applies per-peer/global connection limits;
- applies direct/relay retry/backoff;
- owns root dial-admission state;
- retires redundant relay peer connections after a successful stable direct upgrade;
- exposes path state to transport health/diagnostics.

NetworkBehaviour-originated dial requests from AutoNAT/relay/DCUtR remain subject to the same root gate and diagnostic attribution. Dial result accounting follows ADR-0011's address-scoped policy: a Noise identity mismatch quarantines/failure-scores the attempted address, not the expected trusted PeerId, and never-successful poisoned addresses cannot peer-wide suppress an eligible known-good route.

Inbound Internet listeners also apply the pre-Noise pending/rate/timeout admission limits from `transport/libp2p/SECURITY.md` before any connection class can be known.

## 4. Connection classes and protocol admission

Mandatory Phase 9 adds a legitimate control-plane peer class from ADR-0036.

```text
DataPlaneTrusted:
    peer in trust.allowed_peers

ConnectivityInfrastructureOnly:
    peer NOT in trust.allowed_peers
    AND peer in transport.connectivity.infrastructure.allowed_peers

Unauthorized:
    neither
```

A data-plane trusted peer may also serve relay/AutoNAT roles. An infrastructure-only peer may not become an application peer simply because a Noise/Yamux connection exists.

### Protocol matrix

| Protocol | data-plane | infrastructure-only |
|---|---:|---:|
| Identify / bounded ping | yes | yes |
| AutoNAT v2 | eligible | eligible |
| Relay v2 control | eligible | eligible |
| Relay v2 circuit as destination peer | yes | no |
| GossipSub | yes | no |
| direct v2 | yes | no |
| endpoint directory | yes | no |
| Kademlia routing | trusted policy only | no |
| DCUtR as destination peer | yes | no |

Relay is the one protocol with two rows, and that pair is the distinction the table turns on: **who an exchange is WITH is a different question from who it is FOR.** Reserving or renewing a slot on a relay is an exchange with that peer for the purpose it was authorized for. A circuit whose far end *is* that peer uses it as an application destination and is refused — a circuit carries the data plane by construction. A relay may carry a circuit without becoming a party the circuit may terminate at. DCUtR has a single row and it is already the destination one, so the circuit row says for circuits what that row has always said for hole punches. (ADR-0036 Amendment 2026-09-03.)

GossipSub must blacklist/exclude infrastructure-only PeerIds. Direct/endpoint managers reject their application requests. Kademlia manual insertion rejects them. No network content can modify either allowlist.

## 5. Direct reachability evidence

Reachability is evidence, not identity/trust.

### State

```text
DirectInboundState =
    Unknown
  | VerifiedPublic { verified_addrs, evidence_until }
  | NotVerified { last_failure_at? }
```

`NotVerified` deliberately does not claim a particular NAT type. It means the current evidence does not verify direct inbound reachability.

### Candidate addresses

The manager may test candidate direct addresses from:

- configured external addresses;
- local listener/external-address manager candidates;
- authenticated Identify observations that are plausible Internet addresses;
- implementation-specific externally observed addresses accepted by the address manager.

Private/LAN addresses are never promoted to Internet-public solely because they were configured or echoed by a peer.

### AutoNAT-v2 evidence rule

Per-address observations are keyed by:

```text
(tested_address, probe_server_peer_id)
```

Each observation contains success/failure, time, and expiry.

Default architecture targets:

- minimum distinct successful authorized servers for `VerifiedPublic`: **2**;
- success evidence TTL: **15 min**;
- retry cadence while unknown/not verified: **30 s**, exponentially/backoff bounded by **5 min**;
- refresh cadence while verified: **5 min**;
- max concurrent client probes: **2**;
- max candidate addresses tested per evaluation cycle: **4**.

`VerifiedPublic` for an address is entered only when at least two distinct currently authorized AutoNAT servers have recent success for that exact normalized address. One success remains useful diagnostics but keeps aggregate state `Unknown`/not fully verified.

A fresh contradiction does not instantly flap the state. The manager uses bounded hysteresis: verified state remains until evidence expires unless two distinct authorized recent failures invalidate it sooner. Network-interface/listener changes immediately invalidate evidence for addresses no longer owned/listened.

A lying/misconfigured authorized server can influence evidence, so this is not a Byzantine reachability proof.

## 6. AutoNAT v2 server role

Every standard build supports the server role; profiles enable it explicitly for infrastructure capable of dial-back service.

Server policy:

- serve only PeerIds locally authorized as data-plane or connectivity infrastructure;
- reject arbitrary Internet clients in this private-network v1 design;
- prefer globally routable observed remote addresses for Internet probes;
- use explicit concurrency/rate limits;
- never grant data-plane trust based on successful probing;
- never persist application payloads;
- log only bounded probe metadata, never application data.

Default architecture budgets:

- concurrent inbound probe services: **8**;
- per-client probe starts: **2/min**;
- global probe starts: **60/min**;
- probe timeout: **15 s**.

AutoNAT-v2 traffic itself is connectivity control traffic and is separately accounted from direct/GossipSub payload traffic.

## 7. Relay candidates

Relay candidates come from two sources only:

1. `transport.connectivity.relay.client.static_relays` — operator-configured multiaddrs containing relay PeerId;
2. only when explicitly enabled, fresh Identify capability observations for already known/authorized peers advertising the relay HOP/server protocol.

`use_authorized_identify_relays` defaults **false**. Static candidates have strict selection precedence until they cannot satisfy the configured active-reservation target; Identify-learned candidates are fallback topology hints, never automatic promotion of trusted contacts into infrastructure use.

Kademlia provider/value records, ChannelIds, EndpointIds, and application payloads are **never** used as relay service advertisements.

A candidate is eligible only when:

- its PeerId is DataPlaneTrusted or ConnectivityInfrastructureOnly;
- at least one usable address is known;
- relay-server protocol support is configured or freshly observed;
- it is not in relay retry backoff;
- adding it does not exceed reservation/connection/resource limits.

Fresh Identify evidence supersedes cached capability observations. `use_authorized_identify_servers` likewise defaults **false**; static AutoNAT observer configuration is preferred and Identify-learned authorized servers are considered only after explicit opt-in and only when static observer targets cannot be met. Advisory capability observations may be cached with bounded freshness using the existing peer-cache capability mechanism, but capability does not imply availability or infrastructure consent.

## 8. Relay reservation lifecycle

### Targets

Default target reservations:

```text
DirectInbound Unknown/NotVerified -> 2 distinct relays
DirectInbound VerifiedPublic      -> 1 warm relay
maximum                           -> 4
```

The target is also capped by the eligible authorized relay population. A small deployment with only one eligible relay can be `Partial`, not stuck in an infinite acquisition storm.

### State per relay

```text
Candidate
  -> DialingControlConnection
  -> Reserving
  -> Active { relay_addr, expires_at, limits? }
  -> Renewing
  -> Active

failure -> Backoff -> Candidate
closed/expired -> remove advertised relay address immediately
```

Reservation retries use exponential backoff with jitter:

- minimum **5 s**;
- maximum **5 min**;
- successful reservation resets only that relay's reservation backoff;
- a denied/capacity-limited relay does not reset unrelated dial backoff.

Renewal starts before reservation expiry using the protocol/library-provided lifetime; if exact lifetime is unavailable at the abstraction boundary, renewal behavior follows the rust-libp2p client contract and is observed through reservation events. The architecture does not invent a permanent reservation.

### Relay address

An active reservation yields an ephemeral reachability address conceptually:

```text
<relay-address>/p2p/<relay-peer>/p2p-circuit/p2p/<local-peer>
```

The exact canonical multiaddr is produced/validated by the selected libp2p implementation. It is registered in the local address registry with provenance `relay-reservation`, relay PeerId, and expiry.

It is removed when:

- reservation closes/expires;
- relay becomes unauthorized;
- local profile shuts down;
- address is superseded/invalidated by protocol event.

No relay address is a durable endpoint/mailbox.

## 9. Relay-server role

Relay-server mode is an explicit infrastructure deployment role, independent of bootstrap/Kademlia roles.

Default architecture limits:

| Relay server resource | Default | Ceiling |
|---|---:|---:|
| reservations | 64 | 512 |
| reservations / PeerId | 1 | 4 |
| reservation duration | 1 h | 24 h |
| active circuits | 128 | 1024 |
| circuits / source PeerId | 4 | 16 |
| circuit duration | 1 h | 24 h |
| bytes / circuit | 64 MiB | 1 GiB |
| pending HOP/STOP operations | 64 | 512 |

The implementation should use rust-libp2p relay server limits/rate-limiter hooks where available and enforce any project-level cap outside the behaviour if necessary.

Server authorization:

- reservation source must be locally authorized as data-plane or connectivity infrastructure;
- source/destination circuit requests are subject to bounded server policy;
- serving a relay request does not imply Channel/application membership;
- infrastructure-only clients do not become GossipSub peers of the relay daemon.

Relay server health reports capacity/denial reasons without exposing payload content.

## 10. Relayed destination connection admission

A trusted relay path does **not** authenticate the remote application peer by itself.

Inbound path:

```text
relay circuit arrives
  -> libp2p end-to-end secure connection negotiates
  -> remote destination/source PeerId authenticated
  -> root connection classification for that remote PeerId
  -> if DataPlaneTrusted: ordinary direct/GossipSub/endpoint protocol admission
  -> otherwise: close/reject data-plane connection
```

The relay PeerId and remote application PeerId are separate principals in diagnostics and policy.

When a relayed inbound transport does not expose the original source IP before the end-peer Noise handshake, the destination pre-auth layer accounts pending/rate limits against the **authenticated relay transport connection / relay PeerId** plus the global pre-auth caps. It must not mint unbounded pseudo-source buckets from circuit metadata. Relay-server `max_circuits_per_source_peer` is complementary at the infrastructure node.

## 11. Dial ownership and origins

All outbound attempts are tagged conceptually with one origin:

```text
direct-user-command
connection-reconcile
discovery-reconnect
kademlia-query
relay-reservation
relay-circuit
autonat-probe
dcutr-hole-punch
```

The root `DialAdmissionGate` evaluates:

- destination PeerId class;
- requested origin/purpose;
- global/per-peer connection limits;
- punitive and retry backoff;
- shutdown state;
- address/path policy;
- relay-specific limits where relevant.

A denied behaviour-originated dial must not reset normal peer backoff. Diagnostics preserve origin so Phase-9 behaviour cannot become invisible dial load.

Infrastructure-only PeerIds are dialable only for permitted connectivity origins, and the origins above divide exhaustively. The list is `DialOrigin`'s variants; that enum is canonical and this section follows it rather than restating a set of its own.

**Permitted: `relay-reservation` and `autonat-probe`, and only those.** Each is an exchange *with* the infrastructure peer for the purpose it was authorized for.

**Refused: every other variant.** `direct-user-command` and `kademlia-query` carry application traffic and routing outright. `relay-circuit` and `dcutr-hole-punch` name that peer as the DESTINATION of a data-plane path, which §4's matrix refuses. And `connection-reconcile` and `discovery-reconnect` are refused because both are *reconnection loops for peers this node wants a data-plane connection to* — neither may be the mechanism that re-establishes infrastructure.

**An infrastructure connection is re-established by the purpose that authorized it, and §8 already owns that path**: a lost reservation goes `failure -> Backoff -> Candidate -> DialingControlConnection`, and that control dial carries `relay-reservation`. AutoNAT is the same shape — the next probe cycle (§5) dials under `autonat-probe`.

**The gate enforces this and the runtime does not retry against it.** A reconnection-loop dial toward an infrastructure-only peer is refused `NotAuthorizedForDataPlane` at the root gate before a socket opens, and an authorization refusal settles the PEER rather than the address: the retry claim is cleared and the entry removed, because waiting does not make an unauthorized peer authorized. The residue is one refused attempt and one diagnostic per underlying failure, not a loop. (A refused *behaviour*-originated dial is the different case: the Swarm discards that denial with no `Dialing` and no `OutgoingConnectionError`, which is why the gate must record its own refusals.)

(Only the `relay-circuit` refusal is ADR-0036's Amendment 2026-09-03. `dcutr-hole-punch` was already refused by a matrix row that predates it — that row is what the amendment was modelled on. The rest of this split states what the shipped classification has always done.)

## 12. Path-aware peer dialing

Known addresses are classified:

```text
DirectVerifiedPublic
DirectConfiguredOrObserved
DirectLan
Relayed { relay_peer }
```

For a data-plane destination, default selection is direct-first:

1. reuse healthy direct connection;
2. start bounded direct dial(s) using normal address ranking;
3. after **750 ms** direct head-start, a usable relay path may start in parallel when no direct connection has completed;
4. first authenticated connection that satisfies policy wins the pending operation;
5. losing in-flight attempts are cancelled where safely possible;
6. relay remains eligible for future failover.

The 750-ms value is an initial architecture default subject to SPIKE-004 tuning, not a wire invariant.

`PeerUnreachable` is returned only after the caller deadline/path budget is exhausted. The public transport error taxonomy does not expose NAT internals.

## 13. DCUtR eligibility and lifecycle

DCUtR applies only when:

- the remote application PeerId is DataPlaneTrusted;
- an authenticated relayed connection to that peer exists;
- no healthy preferred direct connection already exists;
- the peer negotiates DCUtR;
- per-peer/global attempt limits and cooldown allow an attempt.

Defaults:

- max concurrent hole-punch attempts: **4**;
- max attempts concurrently per peer: **1**;
- failure cooldown per peer: **5 min**;
- direct stability period before retiring redundant relay peer connection: **10 s**.

State:

```text
RelayedConnected
   |
   | eligible
   v
HolePunching
   | success                    | failure/timeout
   v                            v
DirectCandidateStable       RelayedConnected
   |
   | stability period
   v
DirectPreferred
   |
   `- retire redundant relayed peer connection when safe
```

Success yields a new direct libp2p connection. Existing streams are not modeled as migrated. After the configured stability gate, runtime emits `PeerPathChanged { previous: relayed, current: direct, reason: dcutr }` for an already-logically-connected peer; it does **not** emit a second `PeerConnected`. New direct requests/pubsub streams prefer the stable direct connection. The relay reservation itself may remain warm for inbound failover according to reservation target policy.

DCUtR-originated dials are attributed `dcutr-hole-punch` and must pass the root gate for the actual remote data-plane PeerId.

## 14. Network-change handling

The daemon may survive Wi-Fi/ethernet/VPN/hotspot changes without changing PeerId.

On listener/interface/address changes:

1. invalidate AutoNAT evidence for removed direct addresses;
2. update candidate address registry;
3. move direct state toward `Unknown` until fresh evidence arrives;
4. raise relay reservation target to private/unknown default immediately;
5. retain still-functional relay control connections/reservations if OS/network permits;
6. trigger bounded re-probe/reconciliation with jitter;
7. do not replay messages that failed during the transition.

A later verified-public result may reduce warm relay target from two to one but never below the configured public target.

## 15. Address registry and Identify wiring

Rust Identify is treated as an explicit integration source, not magical plumbing.

The address registry stores each address with:

```text
address
path_kind
discovery/source provenance
reachability evidence class
observed_at
expires_at?
relay_peer?
```

Advertisement policy:

- verified public direct: eligible for Internet Identify advertisement;
- active relay reservation: eligible until reservation expiry/closure;
- LAN/private address: eligible only under local/LAN policy, never labeled Internet verified;
- unverified observed public address: diagnostic/candidate only by default.

Identify observations feed:

- peer address book;
- relay/AutoNAT server protocol capability observations;
- Kademlia capability observation where already designed;
- diagnostics.

Identify does not grant trust or infrastructure authorization.

## 16. Interaction with Kademlia

Kademlia remains peer-routing discovery, not NAT traversal.

- Kademlia may help learn addresses of trusted data-plane peers.
- It may not publish/consume provider records for relay service.
- Infrastructure-only relay/probe peers are not inserted into v1 Kademlia routing tables under `routing_peer_policy=data-plane-trusted`.
- EndpointId/ChannelId/recovery/human metadata never enter the DHT.
- A Kademlia-returned address still needs ConnectionManager path/trust admission.

Phase 9 therefore does not weaken the Kademlia trust separation established by ADR-0009/0034.

## 17. Interaction with Model B endpoints

Reachability works entirely below endpoint routing:

```text
(PeerId B, EndpointId human)
       |
       v
ConnectionManager selects direct or relay path to PeerId B
       |
       v
/direct/2.0.0 carries EndpointId human
```

A relay does not see or authorize local EndpointRegistry leases as a transport policy principal. Endpoint-specific authorization still evaluates the authenticated remote data-plane PeerId after connection establishment.

## 18. Interaction with GossipSub

GossipSub uses whatever admitted connection path exists between data-plane peers. Infrastructure-only relay/probe control connections are excluded/blacklisted.

A relayed data-plane connection still participates as a connection to the actual trusted remote PeerId. Relay metadata visibility does not change ADR-0014: relay transport is end-to-end encrypted, while trusted GossipSub forwarding peers can read plaintext application payloads because they are actual data-plane peers.

## 19. Transport-visible status

Backend details are normalized into:

```text
ConnectivitySummary {
  direct_inbound: unknown | verified_public | not_verified,
  relay_inbound: unavailable | partial | ready,
  active_relay_reservations: u16,
  target_relay_reservations: u16,
  active_relayed_peer_paths: u16,
  hole_punch_inflight: u16,
  preferred_path_policy: direct_first,
  updated_at,
}
```

This is operational status. It does not expose raw relay/probe peer lists to Claude by default.

## 20. Health semantics

Reachability component:

- `healthy`: direct verified-public **or** relay target satisfied; outbound connectivity usable;
- `degraded`: transport still usable but direct unverified and relay target only partially met, or AutoNAT evidence stale, or relay failover reduced;
- `unavailable`: no usable outbound path/control connectivity and no active direct/relay data-plane path.

Aggregate transport may remain degraded rather than unavailable when already-connected peers continue to work.

AutoNAT-server or relay-server role health is reported separately from client reachability. A relay server at capacity is degraded for server service but does not necessarily make its own application transport unavailable.

## 21. Failure semantics

| Failure | Required behavior |
|---|---|
| no AutoNAT servers | direct state unknown; maintain relay target; degraded diagnostic |
| contradictory AutoNAT evidence | keep bounded hysteresis, expire old evidence, do not flap trust |
| all relay reservations denied/full | bounded backoff; direct connections still usable; reachability degraded |
| one of two relays fails | keep remaining path, replace failed reservation with another eligible relay |
| all relays fail while direct private | inbound reachability degraded/unavailable; no identity change/mailbox |
| DCUtR fails | keep relay path; cooldown before retry |
| DCUtR succeeds | prefer new direct path after stability period; retire redundant relay peer connection safely |
| relay circuit closes mid direct send | ordinary request-response failure/cancellation semantics; caller may retry |
| network interface changes | invalidate stale evidence/addresses, re-probe/reconcile, preserve PeerId |
| infra peer sends app protocol | deny/exclude; do not promote to data-plane trust |
| relay source is trusted but relayed remote is not | reject remote data-plane connection |

## 22. Resource limits

Reachability-specific defaults are summarized in `docs/architecture/resource-limits.md` and configuration schema. No queue, probe set, reservation set, or hole-punch set is unbounded.

Client defaults:

- AutoNAT probes inflight: 2;
- AutoNAT distinct confirmations: 2;
- relay reservations private/unknown: 2;
- relay reservations public: 1;
- max relay reservations: 4;
- active relay path per remote peer: 1;
- DCUtR inflight global: 4;
- DCUtR inflight/peer: 1;
- DCUtR cooldown: 5 min;
- total libp2p connections: 384 default / 4096 ceiling;
- connections to one remote PeerId: 3 default / 8 ceiling to allow bounded direct+relay transition.

Relay-server defaults are in section 9.

## 23. Security model

### Relay

A relay can:

- know source/destination/relay PeerIds and path use;
- observe timing, volume, circuit lifetime;
- refuse reservations/circuits;
- drop/degrade availability.

It must not be treated as able to authorize an endpoint or application peer. End-peer libp2p security authenticates the remote application PeerId. Relay traffic is expected to remain end-to-end encrypted by the libp2p secure connection.

### AutoNAT

An authorized probe server can lie about reachability or selectively fail probes. Multiple distinct observations reduce single-server influence but do not provide Byzantine consensus. Results never grant data-plane or infrastructure authorization.

### DCUtR

Hole punching reveals/uses candidate network addresses between already-authorized remote application peers. It increases metadata exposure but does not create application trust.

### Infrastructure connection class

Infrastructure-only authorization is intentionally narrower than `trust.allowed_peers`. A compromised relay/probe server therefore should not receive broadcast plaintext or direct application payloads through the local data plane.

## 24. Observability

Required bounded diagnostics:

- `direct_inbound_state` and state transitions;
- tested-address counts without raw public labels in metrics;
- AutoNAT probes started/succeeded/failed/timeouts by bounded reason;
- distinct evidence-server count;
- relay candidates/active reservations/target;
- reservation accepted/renewed/denied/closed/backoff;
- relayed peer paths established/closed;
- relay server reservation/circuit utilization and denials;
- DCUtR attempts/success/failure/cooldown;
- path chosen: direct vs relay;
- direct-upgrade stable/retired relay path events;
- dial origin including reachability behaviours;
- infrastructure-only protocol admission denials.

Raw PeerIds/multiaddrs remain local-admin/redactable diagnostics, not unbounded metric labels.

## 25. Required integration tests

At minimum:

1. two directly reachable public peers verify direct reachability and still maintain configured warm relay policy;
2. private peer obtains two reservations and is dialable through either relay;
3. one relay disappears and replacement/failover restores target without PeerId change;
4. private-to-private peers communicate through relay when hole punching fails;
5. eligible private peers upgrade relayed connection with DCUtR and subsequently prefer direct;
6. DCUtR failure leaves relay messaging intact;
7. AutoNAT evidence requires distinct authorized servers and expires correctly;
8. stale/removed listener address loses verified status;
9. infrastructure-only relay cannot receive/send GossipSub/direct/endpoint-directory/Kademlia data-plane traffic;
10. relayed inbound untrusted application PeerId is denied even through authorized relay;
11. relay-derived listen address disappears immediately when reservation closes;
12. direct/relay race obeys root dial limits/backoff and records origin;
13. network change invalidates evidence and raises reservation target without changing PeerId;
14. relay server resource limits deny excess reservations/circuits without unbounded state;
15. server and bootstrap co-location does not make bootstrap authoritative;
16. Model-B direct EndpointId routing is identical on direct and relayed connections;
17. GossipSub broadcast works over admitted relayed data-plane connection without adding relay infrastructure peer to the mesh;
18. application operation timeout/failure semantics remain transport-v2 compliant during path failure;
19. runtime infrastructure-only/data-plane class transition reconciles GossipSub/Kademlia/application state atomically or closes/reopens the connection before privilege changes;
20. a Relay v2 circuit whose far end IS an infrastructure-only peer is refused, and a DCUtR hole punch toward such a peer is refused, while a reservation with that same peer and a circuit *through* it toward a trusted destination are both admitted — the pair is the assertion, since either half alone passes for a gate that refuses everything or one that refuses nothing (ADR-0036 Amendment 2026-09-03; §4's matrix, §11's origin split).

## 26. SPIKE-004 release gate

SPIKE-004 must validate the actual rust-libp2p dependency selected for production:

- AutoNAT v2 client/server event/API behavior and server-selection control;
- Circuit Relay v2 reservation lifecycle and address generation;
- relay client transport + Noise/Yamux + direct/GossipSub over relayed connection;
- DCUtR success/failure events and connection coexistence;
- root dial-gate visibility for behaviour-originated AutoNAT/relay/DCUtR dials;
- protocol-admission enforcement for infrastructure-only peers, especially GossipSub exclusion;
- direct-vs-relay racing/cancellation semantics;
- network-change behavior;
- resource/bandwidth costs under target defaults.

Failure blocks standard-v1 release or triggers a new ADR. It no longer authorizes removing Phase 9 from the standard product.

### Phase A (2026-09-01): what this gate has and has not received

SPIKE-004 ran in two phases and only the first is closed. Phase A is loopback on one machine; the verdict and its findings are in [`SPIKES.md`](../../roadmap/SPIKES.md) and the record is [`spikes/spike-004/`](../../../spikes/spike-004/README.md). Against the nine items above (two of them split — the second because reservation obtain and address generation are answered while refresh and expiry are not, and the third because the relayed transport is answered while the data-plane protocols over it are not):

- **answered by phase A**: AutoNAT v2 client/server event and API behaviour with per-scenario server selection; Circuit Relay v2 reservation lifecycle for obtain, multi-relay hold and withdrawal-on-loss, with address generation; the relay client transport over Noise/Yamux to a completed circuit; DCUtR success events and connection coexistence; root dial-gate visibility for every behaviour-originated AutoNAT, relay and DCUtR dial.
- **not answered, and not by loopback**: reservation refresh and expiry (the crate's default reservation lasts an hour); DCUtR failure and its cooldown (a loopback punch succeeds); direct-versus-relay racing and cancellation semantics (never exercised); network-change behaviour; resource and bandwidth cost under target defaults.
- **not answered, and blocked on this stage's own work rather than on the environment**: `direct`/GossipSub over a relayed connection, and protocol-admission enforcement for infrastructure-only peers including GossipSub exclusion. The phase-A harness carries Identify plus the three connectivity behaviours and no data-plane behaviour at all, so it can say which control protocols such a peer advertises and nothing about which are withheld. That evidence needs the composed `SubstrateBehaviour`, which is §4's exclusion work itself.

**This gate is therefore not met**, and phase B — the real-NAT matrix — is required before it can be.
