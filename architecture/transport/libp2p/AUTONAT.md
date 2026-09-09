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
- it advertises/negotiates the required AutoNAT-v2 server protocol on fresh evidence;
- **this profile DIALLED it and holds that outbound connection**;
- global probe/resource budgets permit work.

Discovery of a peer or protocol support never authorizes it.

### Amendment 2026-09-09 — eligibility is a property of the CONNECTION, not of a selection order

**What changed.** Two clauses were removed from the list above: that a
server must be "configured statically, or (only when
`use_authorized_identify_servers=true`) learned through an
already-authorized Identify/control connection", and that "static
servers have selection precedence until they cannot meet the observer
target". Per-server cooldown/backoff was removed as an eligibility
clause for the same reason and survives as client-side policy (§4).

**Why.** `libp2p-autonat` 0.15.0's v2 client chooses its probe server by
`random_autonat_server()` — a uniformly random pick among the
CONNECTIONS this profile DIALLED whose remote advertised the
dial-request protocol (`v2/client/behaviour.rs:340`). Dialled, not
merely connected: the client installs the dial-request handler only in
`handle_established_outbound_connection`, and `supports_autonat` is set
only from that handler's `PeerHasServerSupport`, so a peer that dialled
US is never a probe server. The pick is also over connections rather
than peers, so a peer holding two outbound connections is drawn twice
as often. Its whole public surface is
`Config::with_max_candidates`, `Config::with_probe_interval`,
`Behaviour::new` and `validate_addr`: there is no hook to rank servers,
to prefer one source over another, or to veto a pick. Nor can the
outbound gate do it, because a probe is a request over an ALREADY-OPEN
connection and the client emits no dial at all (see CLAUDE.md §1 on
step 3 reaching routes 2 and 3). A rule the crate cannot express and the
gate never sees is a rule nothing enforces, and this repository has
shipped that shape before — a comment that reads as settled while
nothing fails when it stops being true.

**What replaces it.** The eligible set is exactly the set of peers this
profile has DIALLED that advertise the protocol, and every one of those
is already
`DataPlaneTrusted` or `ConnectivityInfrastructureOnly` because no other
class is retained. Static configuration keeps a weaker and enforceable
meaning: a statically configured server is one this profile
**guarantees to DIAL**, under `DialOrigin::AutonatProbe` -- dial and not
merely connect to, since the client offers its dial-request protocol
only on connections this profile opened. `use_authorized_identify_servers` likewise
governs CONNECTION rather than selection — with it false, this profile
opens no AutoNAT connection it was not configured for, so an
Identify-learned server can only ever be a peer already connected for
another reason.

**What this gives up, stated rather than implied.** A
`DataPlaneTrusted` peer this profile DIALLED for data-plane reasons that
also advertises the AutoNAT server protocol IS an eligible probe server
under the amended rule, and was not under the old one. It learns which
of our addresses we are testing. It cannot forge a verdict —
`verified_public` needs the configured number of DISTINCT servers, and
the evidence key is `(address, server)` — but it is one of them. The
narrower set is bought by not DIALLING such peers, which is a trust
decision rather than an AutoNAT one; declining their inbound
connections does not buy it, because an inbound connection was never
eligible in the first place.

**Still open, and not settled by this amendment**: §6's candidate scope
has the same shape and a sharper edge. `libp2p-identify` pushes
`ToSwarm::NewExternalAddrCandidate` for every address a peer claims to
have observed (`libp2p-identify-0.47.0/src/behaviour.rs:370`), and the
AutoNAT client probes from exactly that set. So a connected peer can put
an address of its choosing into the set this profile asks a server to
dial — bounded by `max_candidates` and by the servers being authorized,
but it is the "arbitrary remote-supplied addresses" §6 forbids by name.
Whatever closes it sits where candidates are reported, not in the
behaviour. Recorded here so the next reader finds it before writing a
comment that says §6 holds.

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
