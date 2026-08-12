# AutoNAT v2 design

Status: **Required standard-v1 libp2p backend component**

## 1. Selection

Use rust-libp2p AutoNAT **v2** client. Do not use v1 as the standard-v1 reachability oracle. The upstream v2 design separates client/server roles, performs dial-back over a newly allocated port, and includes an asymmetric-cost defense intended to reduce probe-server abuse.

The server role is supported but disabled unless explicitly configured.

## 2. Ownership

`ReachabilityManager` owns policy/evidence. The Swarm owns the rust-libp2p behavior.

```text
Swarm AutoNAT-v2 event
        |
        v
ReachabilityManager
        |
        +-- evidence[address, server]
        +-- DirectInboundState
        +-- relay target input
        `-- ConnectivityChanged
```

AutoNAT must not modify trust, discovery membership, EndpointId state, or application subscriptions.

## 3. Probe-server eligibility

A server is eligible only when all are true:

- its PeerId is `DataPlaneTrusted` or `ConnectivityInfrastructureOnly`;
- it is configured statically, or (only when `use_authorized_identify_servers=true`) learned through an already-authorized Identify/control connection; this flag defaults false and static servers have selection precedence until they cannot meet the observer target;
- it advertises/negotiates the required AutoNAT-v2 server protocol on fresh evidence;
- it is not in per-server cooldown/backoff;
- global probe/resource budgets permit work.

Discovery of a peer or protocol support never authorizes it.

## 4. Evidence model

Evidence is keyed at least by `(tested_address, server_peer)` and contains:

```text
outcome
observed_at
expires_at
probe_id/correlation
bytes_sent class (diagnostic)
```

Defaults:

- required distinct successful servers: 2;
- success TTL: 15 minutes;
- refresh: 5 minutes;
- initial retry: 30 seconds, bounded exponential backoff up to 5 minutes;
- max probes in flight: 2;
- max candidate addresses per cycle: 4;
- probe timeout: 15 seconds.

`verified_public` requires fresh successful evidence from the configured number of **distinct authorized servers** for at least one advertised direct address.

Do not count repeated probes from one server as distinct observers.

`not_verified` means evidence is sufficient to say the proof threshold is not currently satisfied; `unknown` covers startup/insufficient/indeterminate evidence. Exact transition hysteresis is implemented from the state table below and frozen by Phase-9 tests.

## 5. State transitions

```text
startup/network change -> unknown
unknown + threshold fresh successes -> verified_public
verified_public + evidence expiry/failure threshold -> not_verified/unknown
not_verified + threshold fresh successes -> verified_public
```

A verified state must not survive beyond its evidence TTL without refresh. Two fresh independent failures may invalidate a previously verified address before TTL when the configured policy says the tested address is no longer reachable.

## 6. Address candidates

Only listener/address-registry candidates within configured scope are probed. Never send arbitrary remote-supplied addresses to a probe server as an unbounded SSRF-like work queue.

The address registry distinguishes:

- bound local addresses;
- operator-declared candidates;
- Identify-observed candidates;
- AutoNAT-verified direct addresses;
- active relay-derived addresses.

Only verified direct and active relay-derived addresses are advertised as standard-v1 inbound routes by default.

## 7. Server role

When `autonat.server.enabled=true`, this node is connectivity infrastructure. It does not become membership/trust authority.

Default limits:

- concurrent probes: 8;
- probes per client PeerId per minute: 2;
- global probes per minute: 60;
- probe timeout: 15 seconds.

The server accepts probe service only from peers admitted by its configured service policy; standard project deployments use `DataPlaneTrusted` or `ConnectivityInfrastructureOnly` rather than an open anonymous service.

**Dial-back target restriction is mandatory.** The probe server compares every requested candidate against the requester's observed transport source address before any dial is admitted:

- candidate must contain a literal IP address; the probe server does not resolve requester-supplied DNS names;
- candidate IP must equal the observed source IP of the authenticated probing connection; only the candidate port/transport may vary within protocol policy;
- loopback, unspecified, multicast, link-local, RFC1918 private IPv4, IPv6 ULA, and other non-global/special-use destinations are rejected under the standard Internet-service policy even if supplied by an authorized peer;
- mismatch/rejection is a probe failure and never becomes a generic dial request.

This is an SSRF/network-scanning boundary for the server role. Phase-9 conformance must attempt internal, loopback, and unrelated-public-IP targets from an otherwise authorized client and prove no dial is emitted.

Server events/requests must share global connection/dial limits. A permitted probe-created dial uses origin `autonat-probe` and passes `DialAdmissionGate`.

## 8. Security

Threats and responses:

- lying probe server -> multi-observer evidence + TTL + relay fallback;
- colluding servers -> operational independence recommendation; no claim of Byzantine proof;
- probe flood -> server rate/concurrency/timeout budgets;
- client-side address amplification -> bounded local candidate set only;
- probe-server SSRF/scanning -> observed-source-IP equality + literal/global-address filter before dial admission;
- infrastructure privilege escalation -> ADR-0036 protocol matrix;
- stale public classification -> evidence expiry/network-change invalidation.

## 9. Observability

Required diagnostics:

```text
autonat_probes_total{outcome}
autonat_probes_inflight
autonat_distinct_success_observers
autonat_verified_address_count
direct_inbound_state
last_autonat_success
last_autonat_failure_class
```

Raw probe payloads are not application data and should not be logged verbatim when unnecessary.

## 10. Conformance tests

At minimum test distinct-observer counting, TTL expiry, conflicting evidence, network change invalidation, unauthorized server exclusion, behavior-originated dial admission, global/per-peer bounds, server-role quotas, observed-source-IP dial-back restriction (including loopback/private/unrelated-public/DNS rejection), and no data-plane authority leakage.
