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
clause for the same reason. It survives as the DIAL GATE's, not as this
client's: see §4's Amendment 2026-09-09 (ii).

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

**Closed 2026-09-17, where the note said it would be.** The adapter
wraps the client in `ScopedCandidates` (`crates/transport/libp2p/src/
candidate_scope.rs`), which sits on the candidate path and drops every
`NewExternalAddrCandidate` that `is_probeable_address` refuses before
the client sees it — a private, loopback, special-use, relayed or
non-literal address a peer claims to have observed never reaches the set
a server is asked to dial. Two things the note did not name are closed
with it: the client's own candidate map is unbounded, remote-fed and
never shrinks, so the wrapper forwards at most `MAX_TRACKED_CANDIDATES`
observed addresses at once and at most four times that over the
process's life (`MAX_OBSERVED_EVER`), beside a separate quota for the
addresses this profile binds; past the lifetime ceiling a new observed
claim is refused and counted, and the profile learns no new observed
address until it restarts -- the case a NAT'd profile loses if one
trusted peer spends the ceiling first, chosen over a map that peer
could grow forever; and the client confirms an address to the Swarm on ONE
server's success, so the wrapper swallows that confirmation and the
manager's verdict is what the Swarm advertises (§5). A peer can still
choose WHICH public address of ours is tested, within those bounds;
what it cannot do is make a server dial anything §6 forbids. Pinned by
the wrapper's tests, with a public control beside every refusal.

## 4. Evidence model

Evidence is keyed at least by `(tested_address, server_peer)` and holds
that server's latest success and latest failure with their observation
times:

```text
success_observed_at   (absent until one succeeds)
failure_observed_at   (absent until one fails; cleared by a later success)
probe_id/correlation
bytes_sent class (diagnostic)
```

**Each server speaks once, with its latest word**: a fresh failure makes
it an observer saying unreachable, otherwise a fresh success makes it one
saying reachable, never both. The success is kept under a later failure
so §5's hysteresis can tell a *reversed* observer — a fresh success now
under a fresh failure — from a *dissenter* that never said reachable and
from a *silent* one whose success aged out. An address verified by the
threshold stays verified while the servers that said reachable, still
saying it or reversed, still make the threshold, at least one still says
it, and fewer than two in total say unreachable. A dissenter counts
toward that two and toward nothing else. Three shapes that could not
express this: one slot per key (one `Unreachable` from a counting
observer dropped the address below the threshold on its own); a server
counted in both sets (at a threshold of one the address stayed verified
for a full TTL while its only observer said unreachable); and a quorum
that admitted dissenters (removing a verifying server left the verdict
standing on the word of the server calling the address unreachable). A
success that merely ages out is silence, not contradiction, and a verdict
short of its threshold lapses with it.

Defaults:

- required distinct successful servers: 2;
- evidence TTL: 15 minutes — for a success and for a failure alike, since
  the two are weighed against each other;
- refresh: 5 minutes — the cadence at which the reachability manager
  returns an address with fresh success evidence to the sweep through
  ADR-0051's `retest`; the client's own tick is not this and is not set
  from it, see the amendment below and its 2026-09-17 note;
- max candidate addresses per cycle: 4.

### Amendment 2026-09-09 (ii) — three client knobs named a policy nothing here could apply

**What changed.** `initial retry: 30 seconds, bounded exponential backoff
up to 5 minutes`, `max probes in flight: 2` and `probe timeout: 15
seconds` were removed from this client list, and the success TTL was
widened to cover failures. `max candidate addresses per cycle` stays as
the one value the client actually sets, and `refresh` stays as the
manager's cadence (as first written this sentence had the client set
both; the note of 2026-09-17 below is why it no longer does).

**Why.** After the §3 amendment above, this profile does not issue
probes: `libp2p-autonat` 0.15.0 picks the address, picks the server and
picks the moment. Its whole client surface is `Config::
with_probe_interval` and `Config::with_max_candidates`. The second is
`max candidate addresses per cycle`. The first is NOT `refresh`, and
the adapter does NOT set it from that value (the 2026-09-17 note below
says why): the crate's tick sweeps only
candidates it has never tested (`v2/client/behaviour.rs:319-321`,
pristine 0.15.0 numbering as in ADR-0051; 333-335 in the patched tree), and a
tested candidate — `Received` or `Failed` — is never swept again by any
public path (`:166`, `:232`; re-reporting an address only raises its
score, `:108-112`). So the crate offers no refresh and no second observer
for an address at all, which is the finding ADR-0051 answers: the client
is vendored with a `retest` entry point, and WHICH address is re-tested
and WHEN becomes the reachability manager's decision, keyed on this
section's evidence. The interval's default of five seconds matters only
in that light — the tick is cheap while nothing is untested, and it is
`retest` that puts something there. The other three had no mechanism:

- **in-flight probes** — the client caps its own outbound dial-requests
  in a `FuturesMap` built at `v2/client/handler/dial_request.rs:94`, with
  no setter. The bound in force is **10 per connection**;
- **probe timeout** — the same line hard-codes **10 seconds**. This
  section said 15, and nothing could make it so. §7's server-side timeout
  went the same way when step 4 read the server (§7's note of
  2026-09-18);
- **retry backoff** — a probe is a request on a connection already open,
  so what a failure should slow down is the DIAL of a server that will
  not connect. That is the root dial gate's, where
  `ConnectionManager::retry_delay_ms` is already 30 s doubling to a
  5-minute ceiling — this section's own numbers — and
  `ConnectionPolicy` scopes it to the address before the peer, so one bad
  address does not suppress a server's working route. A second copy in
  the client had no way to act: it could not tell the crate which server
  to skip. **One case the gate cannot see**: a server that connects and
  then never answers the dial-request. The crate maps that stream timeout
  to `Io`, resets the candidate to untested and re-issues on its next
  tick (`behaviour.rs:212-223`). ADR-0051's vendored client does NOT
  stop that reset — its patch touches the `AddressNotReachable` arm,
  `retest` and `reset_status_to` only, and the ADR says so itself: a
  transiently failing address "is re-probed without limit even
  upstream". What bounds a silent server is therefore a RATE, the
  crate's own 5-second tick times `max_candidates` per sweep — the
  first left at its default, so no configuration key changes the tick's
  period; `max_candidate_addresses_per_cycle` only caps how many probes
  each tick issues (note of 2026-09-17) — and no event
  reaches the manager for it. The retry
  policy this clause deletes from configuration governs what the
  manager DOES schedule — when `retest` returns an address to the sweep
  after a reported failure — and the adapter applies it under the dial
  gate's 30 s / 5 min constants when it drives `retest`. Nothing in the
  policy crate reads those constants yet; that is the adapter's, and an
  earlier version of this paragraph said the vendored client stopped the
  reset and the manager already applied the policy, neither of which
  the tree did. Review finding on PR #84.

**What an implementer now does differently.** Set `with_max_candidates`
from configuration and leave `with_probe_interval` at the crate's
default (note of 2026-09-17; as first written this said "set the
crate's two knobs"). Do not build a per-probe timeout or a per-server
backoff table in the AutoNAT client; a server that will not connect is
slowed by the dial gate, and one that will not answer only by the
crate's fixed tick, since the crate re-issues that probe itself and
tells the manager nothing. What the manager DOES schedule is which address is
re-tested and when — refresh, the second observer, and retry after a
failure — because `retest` is a lever that exists, which a per-pair
in-flight table was not. The three removed keys are gone from
`config.schema.yaml` too, since a key nothing can honour is a promise the
file should not make.

**Note 2026-09-17 — the crate's tick is left at its default; `refresh`
is the manager's cadence, not the tick's.** As first written, this
amendment had the adapter pass `refresh_interval` to
`Config::with_probe_interval`. A review of PR #84 found that the two
promises above cannot both hold under that binding: the vendored poll
issues an untested candidate only when the tick fires
(`v2/client/behaviour.rs`: `next_tick` is armed with `probe_interval`
and `issue_dial_requests_for_untested_candidates` runs only there), so
with a 5-minute tick a `retest` after a failure waits up to 5 minutes
rather than the 30 s this section promises, and a brand-new candidate —
startup, a network change, a new listener — sits untested for up to 5
minutes, during which `direct_inbound` is `unknown` and the relay target
is the unverified one. The owner chose on 2026-09-17: **the adapter
leaves `with_probe_interval` at the crate's 5-second default**, which
costs nothing while no candidate is untested, and `refresh_interval`
governs only WHEN the manager calls `retest` on a verified address. So
the tick is a sweep latency of at most five seconds on top of whatever
the manager schedules; retry, refresh and the first probe are all the
manager's numbers. The one knob the adapter does set from configuration
is `with_max_candidates`. `config.schema.yaml`'s comment on the key and
`profile-config`'s field doc say the same thing.

`verified_public` requires fresh successful evidence from the configured number of **distinct authorized servers** for at least one advertised direct address.

Do not count repeated probes from one server as distinct observers.

`not_verified` means evidence is sufficient to say the proof threshold is not currently satisfied; `unknown` covers startup/insufficient/indeterminate evidence. Exact transition hysteresis is implemented from the state table below and frozen by Phase-9 tests.

## 5. State transitions

```text
startup/network change -> unknown
unknown + threshold fresh successes, fewer than two fresh failures -> verified_public
verified_public + evidence expiry/failure threshold -> not_verified/unknown
not_verified + threshold fresh successes, fewer than two fresh failures -> verified_public
```

A verified state must not survive beyond its evidence TTL without refresh. Two fresh independent failures may invalidate a previously verified address before TTL when the configured policy says the tested address is no longer reachable.

### Amendment 2026-09-10 — the threshold transitions are not reached while two fresh failures contradict them

**What changed.** The two `+ threshold fresh successes -> verified_public` rows above now carry the same condition the invalidation sentence does: fewer than two distinct servers' fresh evidence saying unreachable. Previously the table read as unconditional.

**Why.** The two halves of this section disagreed about one state, and a reviewer found it: with four authorized servers and a threshold of two, failures from two of them followed by successes from the other two satisfies the table's transition while also satisfying the invalidation clause. Read one way the address verifies; read the other it does not, and a rule that verifies on entry and invalidates on the next evaluation would flap between them on unchanged evidence.

`CONNECTIVITY.md`'s failure model settles it rather than this section choosing: *"contradictory AutoNAT evidence | keep bounded hysteresis, expire old evidence, do not flap trust"*. So contradicted evidence does not verify, in either direction of travel, and the contradiction clears by expiry rather than by being outvoted. The cost is bounded and self-healing — an address two servers affirm can sit `not_verified` until the older failures age out, at most one evidence TTL.

**What an implementer does differently.** Apply the two-failure ceiling to entering `verified_public` as well as to leaving it. `ReachabilityManager::derive` already did; this section is what was ambiguous.

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
- global probes per minute: 60.

**Note (2026-09-18, step 4).** An earlier version of this list carried `probe timeout: 15 seconds`, and the profile block a `timeout` key for it. Neither reached a mechanism: the pinned server bounds a dial request at 10 s and a dial-back stream at 10 s in code (`v2/server/handler/dial_request.rs`, `dial_back.rs`), with no setter, and the dial-back's connection attempt is bounded by the Swarm's connection timeout, which this runtime sets to the handshake timeout. A wrapper can neither lengthen the crate's bound nor abort a pending dial, so the key was removed as the client's `timeout` was (§4, Amendment 2026-09-09 (ii)). The three budgets above are the wrapper's own: concurrency is dial-backs in flight (from the request's command to the dial's outcome, or a 60 s horizon if none comes — twice the largest handshake timeout, so a pending dial is never released early), and the two rates are probe STARTS in a sliding minute — a start is charged only when every budget admits it, and a refused start spends nothing. A request over budget is refused before the crate ISSUES its dial — after the crate has parsed the request and, for a candidate that differs from the connection's observed address (the usual case), completed the 30–100 KB dial-data exchange, since no earlier hook exists outside the crate — and the client hears an internal error, which its crate treats as transient and re-asks at its next sweep; a target refusal (below) fails the dial the crate issued, and the client hears a probe failure. The two reach the client as two classes on purpose. **So the budgets bound dial-backs, not a flood's request-handling cost**: that is bounded by the crate's ten requests per connection (10 s each) and the connection ceilings, and an authorized client can spend it. A dial-back the outbound gate refuses — at its pending hook (peer backoff, a ceiling, drain: never dialled) or at its established hook (a quarantined address: dialled, connected, refused before a handler existed) — is counted as `refused_by_gate` and never as served; the client hears the same probe failure a target refusal gives, which the wrapper cannot change — and for the quarantined case that is a wrong verdict about an address just reached, mitigated by the client's multi-observer evidence.

The server accepts probe service only from peers admitted by its configured service policy; standard project deployments use `DataPlaneTrusted` or `ConnectivityInfrastructureOnly` rather than an open anonymous service. **In this substrate that policy is the class gate's infrastructure service**: the dial-request protocol is offered to both authorized classes and to nobody else, and with the server configured an infrastructure-only peer's inbound is retained so it can ask — the inbound arm asks `authorizes_for(class, AutonatProbe)` for every inbound then (CLAUDE.md §1, route 3 widened).

**Dial-back target restriction is mandatory.** The probe server compares every requested candidate against the requester's observed transport source address before any dial is admitted:

- candidate must contain a literal IP address; the probe server does not resolve requester-supplied DNS names;
- candidate IP must equal the observed source IP of the authenticated probing connection; only the candidate port/transport may vary within protocol policy;
- loopback, unspecified, multicast, link-local, RFC1918 private IPv4, IPv6 ULA, and other non-global/special-use destinations are rejected under the standard Internet-service policy even if supplied by an authorized peer;
- mismatch/rejection is a probe failure and never becomes a generic dial request.

This is an SSRF/network-scanning boundary for the server role. Phase-9 conformance must attempt internal, loopback, and unrelated-public-IP targets from an otherwise authorized client and prove no dial is emitted.

**Where it runs (2026-09-18, step 4).** The pinned server implements none of it (SPIKE-004 F2): `ProbeServer` (`crates/transport/libp2p/src/probe_server.rs`) does, at its pending outbound hook — before any socket — for every dial the crate issues, pairing each with the request it answers so the observed source is the request connection's own. A refusal fails the crate's dial (`E_DIAL_ERROR` to the client) and is reported by name; the address-class half is the same `is_probeable_address` the client's candidates pass (§6), so both ends refuse one list. The hook runs AFTER the outbound gate's, which has admitted the dial and deposited a ticket; the gate takes that ticket back on the synchronous failure and counts the release, so a refused dial-back leaks no pending-dial slot. On loopback the substrate can show a target REFUSED, not a dial-back MADE (§6 refuses every loopback candidate); the crate-level harness makes one with the bare vendored server, and a dial-back through the substrate is SPIKE-004 phase B's.

Server events/requests must share global connection/dial limits. A permitted probe-created dial uses origin `autonat-probe` and passes `DialAdmissionGate` — it is announced by `Attributing` from the server's own `poll`, CLAUDE.md §1's route 1.

## 8. Security

Threats and responses:

- lying probe server -> multi-observer evidence + TTL + relay fallback;
- colluding servers -> operational independence recommendation; no claim of Byzantine proof;
- probe flood -> server rate/concurrency budgets on dial-backs, the crate's ten-requests-per-connection cap and the connection ceilings on request handling (§7's note says which bounds what);
- client-side address amplification -> bounded local candidate set only;
- probe-server SSRF/scanning -> observed-source-IP equality + literal/global-address filter before dial admission;
- infrastructure privilege escalation -> ADR-0036 protocol matrix;
- stale public classification -> evidence expiry/network-change invalidation.

## 9. Observability

Required diagnostics:

```text
autonat_probes_total{outcome}   (… | refused_unknown_server | refused_untracked_address
                                 | unclassified)
autonat_retests_total{reason}   (refresh | second_observer | retry)
                                 retry covers a reported failure, a probe that
                                 reached no outcome within the adapter's silence
                                 bound (ADR-0051: the caller's own timeout), and
                                 the re-test of every candidate after a network
                                 change (§5)
autonat_distinct_success_observers
autonat_verified_address_count
autonat_server_probes_total{outcome}   (served_ok | served_failed | served_unrecorded
                                 | refused_concurrent_probes | refused_client_rate
                                 | refused_global_rate | refused_no_address
                                 | refused_not_literal_ip | refused_source_mismatch
                                 | refused_source_unknown | refused_not_global
                                 | refused_unexpected_dial | refused_by_gate)
direct_inbound_state
last_autonat_success
last_autonat_failure_class
```

The server's row is `ProbeServer`'s counters, and every refusal is also an event (`AutonatProbeRefused`) for the reason the client's are. `served_ok` and `served_failed` are read from what the wrapper saw of the DIAL — established, or failed — not from the crate's own report, whose `result` is about the exchange and is `Ok` after any response was sent, a refusal's included; `served_unrecorded` is a report the wrapper holds no dial record for, counted apart so it is never read as a success.

The two `refused_*` outcomes are `ReachabilityManager::record_outcome`'s
`RefusedReport` variants — a report from a server this profile never
offered, and one about an address it does not track — counted by the
adapter so that a refusal is never read as "recorded, no change"
(SPIKE-004's finding that an invisible refusal is a subsystem dying
silently; review finding on PR #84). `unclassified` is a probe event
whose error the adapter could not map to an outcome, so no vote was
recorded. It exists because the adapter's first classifier (PR #89)
matched an error text the pinned crate's public event never carries,
and so mapped EVERY real failure to "no outcome" while its tests, fed
strings of the expected shape, passed; the first outcome produced over
the wire (2026-09-18) found it. The classifier now matches the texts the
public `Error` displays, pinned against the vendored source, and the
load-bearing test feeds it an event a real server produced
(`crates/transport/libp2p/tests/autonat_outcome_wire.rs`).

Raw probe payloads are not application data and should not be logged verbatim when unnecessary.

## 10. Conformance tests

At minimum test distinct-observer counting, TTL expiry, conflicting evidence, network change invalidation, unauthorized server exclusion, behavior-originated dial admission, global/per-peer bounds, server-role quotas, observed-source-IP dial-back restriction (including loopback/private/unrelated-public/DNS rejection), and no data-plane authority leakage.
