
# Kademlia integration blueprint

Status: **architecture-complete, implementation deferred, default disabled**.

This document specifies how Kademlia integrates into `claude-p2p-channel` without changing the generic `DiscoveryProvider` or `Transport` contracts. It is an implementation blueprint, not production code.

## 1. Goal and rollout posture

Kademlia provides optional distributed **peer routing and address discovery**. It does not provide channel membership, trust, application storage, durable messaging, or application identity.

The configuration remains:

```yaml
discovery:
  providers:
    - type: kademlia
      enabled: false
```

Two independent gates must be satisfied before it can run:

1. the daemon build contains the approved Kademlia implementation; and
2. the operator explicitly sets `enabled: true`.

If gate 1 is false and gate 2 is true, profile startup fails with an explicit unsupported-provider configuration error. If the build supports Kademlia but `enabled: false`, no Kademlia provider task is started and the Swarm must not advertise or initiate the project Kademlia protocol.

## 2. Architectural boundary

```text
                         generic boundary
                             |
DiscoveryManager ------------+-----------------------------
       ^                     DiscoveryProvider events
       |
       | Discovered / Updated / Expired / health
       |
KademliaDiscovery
  - scheduler
  - query budgets / saturation
  - TTL/provenance
  - health
       |
       | neutral bounded kademlia-control-api port
       v
transport-libp2p / Swarm task
  - Identify adapter
  - Kademlia driver: libp2p::kad::Behaviour
  - ConnectionManager policy state
  - Swarm-wide DialAdmissionGate
       |
       v
Noise + Yamux/TCP libp2p connections
```

`KademliaDiscovery` implements the generic discovery contract but does **not** own the Swarm and does not dial peers directly. The existing single Swarm task remains the only owner of `libp2p::kad::Behaviour`.

The port is deliberately Kademlia-specific and internal to the Rust workspace. It lives in a tiny `kademlia-control-api` crate with backend-neutral opaque identifiers/addresses; it is not added to `transport-api`, `discovery-api`, IPC, or Claude-facing tools. `discovery-kademlia` and `transport-libp2p` both depend on this tiny crate, not on each other.

## 3. Internal driver port

Conceptual command enum:

```text
KadCommand
  AddRoutingAddress { peer_id, address }
  RemoveRoutingAddress { peer_id, address }
  RemoveRoutingPeer { peer_id }
  Bootstrap { reason }
  LookupTrustedPeer { peer_id }
  Explore { random_key, max_results }
  SetMode { client | server }
  Snapshot { request_id }
```

Conceptual driver events:

```text
KadDriverEvent
  RoutingUpdated { peer_id, addresses, evicted_peer? }
  RoutablePeerObserved { peer_id, address }
  UnroutablePeerObserved { peer_id }
  QueryProgress { query_id, kind, step, result, stats }
  ModeChanged { mode }
  InboundRecordWriteAttempt { peer_id, kind }
  SnapshotResult {
    request_id,
    mode,
    protocol_hash,
    routing_peer_count,
    nonempty_bucket_count,
    active_queries_by_class,
    pending_behaviour_dials,
    last_query_progress_at?
  }
  DriverError { class }
```

`SnapshotResult` is bounded diagnostic state only: no payloads, private keys, unbounded peer lists, or raw routing-table dumps. A request ID correlates the asynchronous response; missing response within the local control deadline is a driver-health failure.

Both directions use bounded Tokio channels. The provider may schedule work; the Swarm task serializes all libp2p behavior mutation.

## 4. Protocol/network namespace

Do not use the public IPFS Kademlia protocol identifier. A supported build constructs a custom stream protocol from:

```text
wire family:  claude-p2p-channel/kad
wire major:   1.0.0
network_id:   operator-defined, non-secret deployment namespace
```

Conceptual derived protocol:

```text
/claude-p2p-channel/kad/1.0.0/<network-hash>
```

`network_id` is restricted to lower-case ASCII matching `^[a-z0-9][a-z0-9._-]{0,63}$`. Derive `network-hash` exactly as follows:

```text
digest = SHA-256("claude-p2p-channel/kad-network/v1\0" || ASCII(network_id))
network-hash = lowercase RFC4648-base32(digest[0..16]), without '=' padding
```

The 16-byte truncation produces a 26-character base32 tag. Golden fixture:

```text
network_id: example-private-network
network-hash: cfrtdnvch5ozdvziz6esfgbs44
protocol: /claude-p2p-channel/kad/1.0.0/cfrtdnvch5ozdvziz6esfgbs44
```

Properties:

- `network_id` is **not a secret** and grants no trust;
- it prevents accidental DHT mixing between unrelated deployments that happen to share bootstrap infrastructure;
- channel names are never included in the Kademlia protocol name or query keys;
- a wire-major change requires explicit compatibility/migration planning; silently speaking unrelated protocol majors is forbidden.

## 5. Client/server mode

The first integration uses explicit mode configuration:

```text
client (default)
server (operator opt-in)
```

`auto` is intentionally excluded initially. Rust-libp2p can automatically move a node to server mode when it observes a confirmed external address, but this project prefers predictable operator intent for the first integration.

### Client mode

- may initiate Kademlia queries;
- does not advertise/serve as a Kademlia server;
- appropriate for laptops, intermittent nodes, and NAT-restricted clients;
- still participates normally in direct/GossipSub data-plane traffic if trusted.

### Server mode

- explicitly advertises and serves the project Kademlia protocol;
- intended for stable, adequately reachable nodes;
- is still not an authority, registry, membership service, or message broker;
- does not enable record/provider-record storage.

A remote deployment needs at least one reachable trusted server-mode node in the bootstrap/routing graph for clients to make progress.

## 6. Trust and connection policy

Kademlia does not weaken ADR-0011/0012.

For the **first integration**, a peer is eligible to become a Kademlia routing/query peer only when `PeerTrustPolicy` authorizes that PeerId for ordinary data-plane connectivity. Discovery can observe unauthorized candidates, but they are not manually admitted to the local Kademlia routing table.

### Behaviour-originated dial requests

The Kademlia provider itself still does not call the ordinary dial scheduler. However, an iterative `kad::Behaviour` query is a `NetworkBehaviour` and may request dials from the Swarm while walking toward a key. That execution path must not bypass ConnectionManager policy.

The backend therefore applies ADR-0011's **Swarm-wide `DialAdmissionGate`** to every outbound connection attempt, including dials requested by Kademlia. The gate consumes an atomically readable ConnectionManager policy snapshot and enforces:

- current trust authorization for the target PeerId;
- per-peer punitive/retry backoff;
- pending-dial and connected-peer limits;
- shutdown/drain state;
- address policy available at dial admission.

This rule also covers peers learned *inside* iterative query responses before the provider receives a final normalized result. A malicious trusted router cannot cause a successful connection to an unauthorized returned PeerId simply by placing it in a Kademlia response.

Kademlia dial requests are counted separately where attribution is possible (`origin=kademlia-query`) but consume the same global connection budget. A policy denial is reported to the behavior as a dial failure and must not reset ConnectionManager backoff.

Why the trust constraint exists: libp2p connections are multiplexed. Allowing an untrusted peer onto a connection solely for Kademlia would require explicit per-connection/per-protocol admission so GossipSub/direct protocols could not reuse the same connection and reopen the confidentiality problem resolved by ADR-0012/0029. That is a future architecture, not an implicit side effect.

Consequences:

- Kademlia is a **trust-bounded distributed routing overlay** in its first integration;
- asymmetric/small allowlists constrain DHT reachability;
- bootstrap still requires separate trust authorization;
- trust revocation removes the peer from Kademlia routing state and ordinary connections;
- Kademlia cannot bypass ConnectionManager backoff merely because a query needs a routing hop.

SPIKE-003 must instrument/measure behaviour-originated dial volume and prove the gate works under backoff and global-limit pressure.

## 7. Address, Identify, and capability-cache integration

Rust-libp2p does not automatically connect Identify observations to Kademlia routing. The backend must bridge them deliberately.

Admission pipeline for a routing address:

```text
candidate observation
  -> PeerId syntax/self check
  -> address normalization and global address limits
  -> PeerTrustPolicy authorization
  -> connection permitted by DialAdmissionGate
  -> authenticated Identify observation
  -> exact current Kademlia server protocol advertised
  -> KadCommand::AddRoutingAddress
  -> Behaviour::add_address
```

Use `BucketInserts::Manual`. Merely connecting to a peer must not automatically put it in the DHT routing table.

Kademlia addresses are normalized as fully-qualified peer addresses where required by the selected rust-libp2p API. DNS multiaddrs remain unresolved until the normal connection/dial layer resolves them; Kademlia does not create a second DNS resolver.

### Persisted server-capability observation

Remote Kademlia server mode is not locally knowable from a bare allowlisted PeerId. It becomes observable only after an authenticated connection exposes supported protocols through Identify. To make targeted lookup implementable after restart, the existing advisory peer cache persists a bounded capability observation:

```text
protocol_family = claude-p2p-channel/kad
wire_major = 1
network_hash = current network hash
role = server
supported = true | false
observed_at
```

Rules:

- the observation is advisory and never grants trust;
- positive evidence is valid only for the exact wire major + `network_hash`;
- freshness cannot exceed the enclosing peer-cache TTL;
- a fresh Identify response supersedes cached evidence;
- if a peer no longer advertises the exact server protocol, stale positive evidence is removed/replaced;
- deleting the peer cache merely disables cold-start targeted eligibility until evidence is learned again.

This capability field belongs to `PeerCacheDiscovery` because it is historical transport observation, not Kademlia authoritative state.

## 8. Seeding

Eligible hints can enter Kademlia from:

- `StaticBootstrapDiscovery`;
- `PeerCacheDiscovery`;
- optionally mDNS for same-LAN DHT seeding.

The `seed_sources` configuration controls which provider observations are forwarded through `DiscoveryProvider::add_hint`/the composition layer into `KademliaDiscovery`.

A seed is still only a candidate. Before Kademlia routing insertion it must pass the trust/address/protocol rules above.

Avoid a feedback loop: Kademlia-derived candidates are not immediately re-added to Kademlia as new external seed hints. They are already represented inside the Kademlia driver/routing state.

## 9. Query algorithm

The provider has three query classes, sharing global concurrency/rate budgets.

### 9.1 Bootstrap

Trigger bootstrap when:

- the provider starts and at least one eligible routing peer is known;
- the routing table recovers from empty/unavailable to seeded;
- a bounded refresh interval elapses and the provider is enabled/healthy enough to query.

`Behaviour::bootstrap` performs a self lookup and bucket refresh queries. `NoKnownPeers` is a degraded/unavailable provider condition, not a daemon failure.

The implementation explicitly configures periodic bootstrap behavior rather than inheriting an upstream default unknowingly. Any automatic bootstrap triggered by the selected rust-libp2p version on routing-table insertion must be measured and counted in diagnostics/budgets during SPIKE-003.

### 9.2 Trusted-peer targeted lookup

A targeted lookup is eligible only when **all** are true:

1. target is a remote PeerId authorized by current `PeerTrustPolicy`;
2. a fresh peer-cache/Identify capability observation says the target advertised the exact current Kademlia **server** protocol/network namespace;
3. no usable current target address exists, or all normal candidate addresses are in backoff/unusable;
4. the per-target targeted-lookup cooldown has elapsed;
5. global Kademlia query budget permits work.

The provider may then issue a lookup using the PeerId bytes as the Kademlia lookup key. Results are advisory `PeerInfo` observations. The cached server-capability observation only answers "was this peer recently observed serving this DHT namespace?"; it does not prove current reachability, trust, or continued server mode.

This is not a general directory for client-mode nodes. Client nodes are not assumed discoverable by PeerId through `FIND_NODE`; other discovery providers/configured hints remain necessary.

If capability evidence is absent/expired/negative, skip targeted lookup and record a bounded reason diagnostic instead of guessing remote mode.

### 9.3 Random exploration, effective target, and saturation

Define:

```text
remote_trusted_population = count(distinct trust.allowed_peers excluding local PeerId)
effective_target = min(
  target_routing_peers,
  max_routing_peers,
  remote_trusted_population
)
```

This prevents the default target of 64 from making a two- or three-peer private overlay permanently degraded solely because the trust domain is intentionally small.

While the routing view is neither target-satisfied nor saturated, generate 32 cryptographically random bytes and call `get_n_closest_peers(random_key, max_results)` subject to global budgets.

Random exploration keys are independent per query, never hashes of ChannelId/repository/application identity, never persisted, and diagnostics-redacted by default.

#### No-progress backoff

An exploration round makes **progress** only if it yields at least one new trust-admitted routing peer or a new usable address for an eligible routing peer. Otherwise it increments `consecutive_no_progress_rounds` and doubles the next exploration delay from the configured `exploration_interval`, capped at **15 minutes**. Progress resets the delay to the base interval.

Initial saturation rule: after **3 consecutive successful no-progress exploration rounds**, the provider may mark the routing view `saturated` when:

- at least one usable routing peer exists;
- there is no fresh targetable server-capability observation outside the current routing set that is immediately eligible for targeted lookup; and
- recent query health is otherwise good.

Saturation is invalidated by trust-policy revision, a new/updated external seed or capability observation, routing-peer loss, network namespace/mode change, or provider restart. A saturated view still performs the much lower-frequency bootstrap/refresh work; it does not claim that every allowlisted peer was discovered.

Health treats either `routing_peers >= effective_target` **or** valid saturation as sufficient routing-population health. Query failures, no peers, or server-reachability problems may still degrade it independently.

## 10. Result normalization

A successful closest-peer result currently yields `PeerInfo { peer_id, addrs }` in rust-libp2p. Normalize each valid peer/address set into the existing discovery model:

```text
CandidatePeer {
  peer_id,
  addresses,
  source: "kademlia",
  observed_at,
  expires_at: observed_at + candidate_ttl,
}
```

Rules:

- discard self PeerId;
- cap addresses at the global per-peer limit;
- reject malformed/inconsistent `/p2p/<PeerId>` suffixes;
- merge with existing provider provenance in DiscoveryManager;
- do not attach trust, channel, role, or application metadata;
- refresh expiry on later Kademlia observations;
- expiry removes only Kademlia provenance, not observations from other providers.

## 11. Routing table policy

Initial implementation policy:

- `BucketInserts::Manual`;
- K-bucket size default `20`, never configured above libp2p's standard `K_VALUE` in the initial profile;
- separate project-level `max_routing_peers` bound enforced before manual insertion;
- routing entry requires at least one valid address and current trust authorization;
- trust revocation removes the peer immediately;
- address expiry/removal is propagated to the driver;
- routing-table state is ephemeral and never serialized as authoritative state.

The peer cache remains the only advisory persistence mechanism for previously successful peer reachability and bounded authenticated protocol observations; Kademlia routing state itself remains ephemeral.

### Planned rust-libp2p configuration mapping

Against the 2026-08 research API shape, the driver should map project config approximately as follows (exact method names must be revalidated before coding):

```text
kad::Config::new(custom_protocol)
  set_kbucket_inserts(BucketInserts::Manual)
  set_kbucket_size(kbucket_size)
  set_query_timeout(query_timeout)
  set_parallelism(parallelism)
  disjoint_query_paths(true)
  set_periodic_bootstrap_interval(None)
  set_caching(Caching::Disabled)
  set_record_filtering(StoreInserts::FilterBoth)
  set_publication_interval(None)
  set_replication_interval(None)
  set_provider_publication_interval(None)

Behaviour::set_mode(Some(Mode::Client | Mode::Server))
```

The provider scheduler owns the configured periodic bootstrap refresh instead of inheriting the library's default periodic interval. Rust-libp2p documents that routing-table insertion may itself trigger bootstrap behavior; SPIKE-003 must verify the exact selected-version behavior and count any such implicit bootstrap work in diagnostics/rate analysis.

## 12. Records/provider records are disabled

The project uses Kademlia as peer routing only.

The future driver configures incoming record filtering and never accepts record/provider-record writes into durable application state. The provider never calls:

```text
get_record
put_record
put_record_to
start_providing
get_providers
```

Write-back caching for record lookup is disabled because record lookup itself is outside scope. Inbound `PUT_VALUE` / `ADD_PROVIDER` attempts are dropped and counted. Read requests for application records must not expose project-specific data because the local store contains no project records.

A future requirement for DHT records requires a new ADR and a separate schema/security review.

## 13. Initial configuration defaults and validation

These are **implementation defaults, not protocol guarantees**. They remain bounded/configurable:

| Setting | Initial default | Rationale |
|---|---:|---|
| `enabled` | `false` | opt-in rollout |
| `mode` | `client` | no accidental DHT server |
| `routing_peer_policy` | `data-plane-trusted` | preserve established trust boundary |
| `kbucket_size` | `20` | initial K value/profile bound |
| `max_routing_peers` | `256` | project-level memory/topology bound |
| `candidate_ttl` | `30m` | advisory freshness window |
| `query_timeout` | `30s` | WAN-tolerant but bounded |
| `parallelism` | `3` | conservative query fan-out |
| `disjoint_query_paths` | `true` | resilience against adversarial routing |
| `max_concurrent_queries` | `2` | bounded network/work amplification |
| `max_queries_per_minute` | `6` | global query-rate ceiling |
| `exploration_interval` | `60s` | base background rate; no-progress backoff increases it |
| `exploration_jitter_percent` | `20` | avoid synchronized fleets |
| `max_results_per_query` | `20` | bounded closest-peer result count |
| `target_routing_peers` | `64` | desired routing population before effective-target cap |
| `targeted_lookup_cooldown` | `5m` | bound repeated missing-peer lookup |
| `bootstrap_min_interval` | `5m` | avoid bootstrap storms |
| `bootstrap_refresh_interval` | `15m` | periodic routing refresh |

Hard cross-field rules when Kademlia is enabled:

1. `target_routing_peers <= max_routing_peers`;
2. `bootstrap_refresh_interval >= bootstrap_min_interval`;
3. `max_results_per_query <= kbucket_size`;
4. every name in `seed_sources` resolves to a provider entry that is present **and `enabled: true`** in the same profile.

Violation is a configuration validation/startup error, not a warning or silent clamp. When Kademlia itself is disabled, the reserved config may remain present without requiring its seed providers to be enabled because no Kademlia work will execute.

SPIKE-003 must measure the defaults before support promotion. Phase 1 config tests freeze these cross-field rules.

## 14. Health model and server reachability evidence

Kademlia uses the generic provider states.

### Routing-population health

Compute the `effective_target` from section 9.3. The population dimension is sufficient when either:

- `routing_peer_count >= effective_target` and `effective_target > 0`; or
- the provider is in the valid `saturated` state defined above.

A small trust overlay can therefore be fully healthy without pretending it contains 64 routers.

### Server-mode reachability evidence in v1

AutoNAT is deferred, so server reachability is **not cryptographically or actively verified** in this phase. The provider classifies evidence only:

- **declared external evidence:** operator config contains at least one non-wildcard, non-loopback listen/advertised address that is plausibly externally routable (public IP or DNS form); this is operator intent, not proof;
- **peer-observed evidence:** one or more authenticated trusted peers report a non-loopback/non-private `Identify.observed_addr` for this node; this is stronger observational evidence but still not an inbound probe;
- **none:** no such evidence.

Server mode with `none` is `degraded` with reason `server_reachability_unverified`. One of the evidence classes removes that specific degradation only if normal routing/query health is otherwise sufficient. The UI/docs must not label either class "AutoNAT verified" or "publicly reachable".

SPIKE-004 compares these evidence classes with actual NAT/relay reachability and decides whether later AutoNAT changes the health model.

### healthy

- driver running;
- routing population target-satisfied or saturated;
- bootstrap/closest-peer query succeeded within refresh expectations;
- timeout/error rate below threshold;
- if configured server mode, at least declared-external or peer-observed reachability evidence exists.

### degraded

Examples: warming toward effective target; successful but not-yet-saturated under-target exploration; intermittent query failure; server mode with no reachability evidence.

### unavailable

Examples: `effective_target == 0` / no eligible route peer after startup grace; repeated bounded query failure; driver unavailable; invalid protocol configuration.

Provider failure never kills unrelated discovery providers or established transport connections.

## 15. Backpressure and scheduling

- Provider command/event channels are bounded.
- Query creation requires both a global Kademlia query permit and provider rate-budget token.
- Driver events are coalesced where safe (e.g. repeated routing snapshots) rather than queued unboundedly.
- Candidate events still pass through global DiscoveryManager candidate/address bounds.
- A slow provider consumer must not block the Swarm task; overflow transitions Kademlia health to degraded and emits diagnostics.

## 16. Observability

Expose locally, with bounded/redacted labels:

```text
kademlia_enabled
kademlia_mode
kademlia_protocol_hash
kademlia_routing_peers
kademlia_nonempty_buckets
kademlia_bootstrap_total
kademlia_bootstrap_failures_total
kademlia_last_bootstrap_success
kademlia_queries_started_total{class}
kademlia_queries_completed_total{class}
kademlia_query_failures_total{class,reason}
kademlia_query_timeouts_total{class}
kademlia_candidates_emitted_total
kademlia_candidates_expired_total
kademlia_routing_insert_denied_total{reason}
kademlia_record_write_attempts_total{kind}
kademlia_driver_channel_overflow_total
kademlia_effective_routing_target
kademlia_saturation_state
kademlia_behaviour_dial_requests_total
kademlia_behaviour_dial_denied_total{reason}
kademlia_behaviour_dial_connected_total
kademlia_targeted_lookup_skipped_total{reason}
```

Do not log random lookup keys at normal levels. Do not log payloads or private keys.

## 17. Failure behavior

| Failure | Behavior |
|---|---|
| `enabled: true` but build lacks implementation | hard configuration/startup failure |
| `enabled: false` | no provider task/protocol/query activity |
| no eligible seed/routing peer | Kademlia unavailable/degraded; other providers continue |
| protocol namespace mismatch | peer never becomes usable Kademlia route; diagnostic only |
| bootstrap timeout | provider degraded; bounded retry/backoff |
| query timeout | query fails; other queries/providers continue |
| Kademlia behaviour requests dial to unauthorized/backed-off/over-limit peer | root dial gate denies; query sees dial failure; ConnectionManager policy unchanged |
| routing table below configured target but small trust overlay saturated | health may be healthy using effective-target/saturation rule; exploration backs off |
| targeted peer has no fresh server-capability evidence | skip targeted lookup; other discovery continues |
| routing table emptied by trust revocation | remove peers, cancel/suppress exploration until reseeded |
| server mode but unreachable | provider degraded; node can still use other transport capabilities |
| inbound record/provider write | discard/not-store; counter + debug diagnostic |
| driver channel overload | drop/coalesce noncritical diagnostics, mark provider degraded; never block Swarm |

## 18. Security analysis

### DHT poisoning / malicious routing responses

All Kademlia results are advisory. Manual routing insertion, data-plane trust gating, candidate caps, address validation, disjoint query paths, and bootstrap diversity reduce impact. Residual risk remains when trusted DHT peers are compromised.

### Sybil / eclipse

The first integration's trust-gated routing set materially limits arbitrary Sybils but does not prevent an operator from trusting many attacker-controlled PeerIds. Disjoint paths and independent bootstrap seeds reduce single-path capture. No claim of Byzantine resistance is made.

### Bootstrap capture

Use multiple bootstrap candidates where practical. Bootstrap nodes have no special trust semantics beyond their independently configured PeerId trust and no authority over membership/messages.

### Privacy

Kademlia exposes transport PeerIds, addresses, protocol participation, and query traffic to routing peers. Random query keys avoid leaking channel names. `network_id` may be guessable and is not treated as confidential.

### Record abuse

Value/provider record APIs are not used; inbound writes are filtered/dropped. This avoids turning the daemon into an unplanned distributed data store.

## 19. Configuration reload

Changing Kademlia configuration is classified:

- `enabled: false -> true`: provider start only on a build that supports it; perform full validation first;
- `true -> false`: stop new queries, cancel/finish bounded in-flight work, remove Kademlia routing state/protocol participation, expire Kademlia provenance, leave other discovery providers untouched;
- `network_id` or wire-major change: restart Kademlia provider/behavior; do not migrate routing state across namespaces;
- mode change: bounded provider restart or explicit `set_mode`, depending on implementation evidence;
- budgets/TTLs: hot-reloadable if the implementation can apply atomically; otherwise provider-scoped restart.

No Kademlia reload changes `PeerTrustPolicy`.

## 20. Rust package/module placement

Planned shape:

```text
crates/
  discovery-api/
  kademlia-control-api/   # INTERNAL, tiny, neutral; no libp2p types
  discovery-kademlia/
    provider
    scheduler
    budgets
    normalize
    health
  transport-libp2p/
    swarm_task
    dial_admission_gate
    kademlia_driver       # owns kad::Behaviour inside Swarm
    identify_adapter
    connection_manager
```

`kademlia-control-api` contains only the narrow command/event port and neutral bounded DTOs. It may depend on neutral transport identifier/address types but not on `libp2p`. Both concrete sides depend on it:

```text
discovery-kademlia ---> kademlia-control-api <--- transport-libp2p
        |                                         |
        +--> discovery-api                        +--> rust-libp2p
```

This corrects the dependency ambiguity that would occur if the handle type lived in `transport-libp2p`: the provider stays libp2p-free and the backend does not depend on the provider crate. The composition root obtains the backend implementation of the port and injects it into the optional provider.

The port remains internal and Kademlia-specific; it does not become another generic `DiscoveryProvider`-like abstraction.

## 21. Test plan

### Unit

- config defaults/ranges and conditional `network_id` requirement;
- `enabled: false` creates no provider activity;
- unsupported build + `enabled: true` fails;
- deterministic protocol-name derivation fixtures;
- random exploration keys never depend on ChannelId/application input;
- query budget, concurrency, jitter, targeted cooldown, no-progress backoff, effective-target/saturation state;
- manual routing insertion policy and trust rejection;
- root dial-admission behavior for Kademlia-originated dials under backoff/global limits;
- peer-cache positive/negative server-capability observation freshness/supersession;
- Kademlia cross-field config and enabled seed-source validation;
- bounded `SnapshotResult` correlation/shape;
- `PeerInfo` normalization/address caps/self filtering;
- Kademlia provenance TTL/expiry;
- inbound record/provider write attempts are not stored.

### Provider conformance

Run the standard `DISCOVERY-CONFORMANCE.md` suite, including deterministic shutdown and failure isolation.

### Local integration

- 3 trusted nodes: one server seed + two clients bootstrap and discover routing candidates;
- 10-20 node private DHT: random exploration converges without channel/provider records;
- targeted lookup locates an allowlisted peer with fresh cached exact-server-protocol evidence whose address is not locally cached;
- a client-mode peer is not falsely promised to be discoverable through Kademlia peer routing alone;
- client-mode node is not advertised as a Kademlia server;
- server-mode node serves `FIND_NODE` but stores no application/provider records;
- trust revocation removes a routing peer and prevents further query use;
- asymmetric trust does not create untrusted Kademlia connections;
- mDNS/static/cache seeds merge without duplicate CandidatePeers;
- Kademlia failure leaves GossipSub/direct traffic on existing peers intact;
- 2-3 peer allowlist reaches effective-target/saturated healthy state without 60-second perpetual exploration;
- Kademlia query engine attempts to dial a returned unauthorized or backed-off peer -> root dial gate denies establishment;
- server-mode `none` vs declared-external vs peer-observed reachability evidence produces the documented health reason without claiming AutoNAT verification.

### Adversarial/integration

- malicious trusted seed returns many hostile/stale addresses -> caps and trust still apply;
- multiple trusted malicious routers attempt eclipse -> disjoint paths/diverse seeds measured;
- query storms -> global budget enforced;
- record/provider write flood -> filtered/dropped without unbounded store growth;
- protocol namespace mismatch -> clean incompatibility, no accidental public-DHT join;
- Kademlia driver event flood -> Swarm remains responsive.

### Interoperability

Before implementation freeze, run a rust-libp2p-only test using the exact targeted crate version and the custom protocol namespace. Cross-language interoperability is optional unless a product requirement appears.

## 22. Enablement criteria

Kademlia remains `enabled: false` by default even after implementation exists. Promotion from architecture-only to supported optional capability requires:

1. SPIKE-003 validates current rust-libp2p hooks and the Identify/manual-insert behavior;
2. provider conformance suite passes;
3. 20-node local convergence test passes within documented query bounds;
4. poisoning/eclipse simulation produces bounded resource use and documented residual risk;
5. no record/provider-record persistence occurs;
6. disabled-mode test proves no Kademlia network activity;
7. behaviour-originated dial measurement proves ConnectionManager policy/backoff/global limits apply to Kademlia query dials;
8. targeted lookup capability-cache semantics and small-overlay saturation tests pass;
9. Phase 1 cross-field/seed-source config fixtures are frozen and passing;
10. final implementation ADR/review confirms no change to discovery/trust/connection boundaries.
