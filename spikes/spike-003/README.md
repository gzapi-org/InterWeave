# SPIKE-003 — Kademlia integration validation

**Status: PASS for Stage 10 implementation. The v1 RELEASE gate is NOT closed** — two of the brief's expected-evidence items are not established, and both need infrastructure this harness does not have.

Thirteen findings change how Stage 10 must be written, one of which says Stage 10 cannot begin by enabling the feature and three of which say the gate cannot be written the obvious way.

## What this verdict does and does not unlock

**Unlocked:** implementing the specified `KademliaDiscovery` and driver. Every design question Stage 10 has to answer before writing code was asked here, and the answers are below.

**NOT unlocked:** ADR-0034 makes SPIKE-003 a *v1 release gate* for shipping configured Kademlia entries default-enabled. Two required items are unmet, so that gate stays open:

- **Server-mode reachability evidence is not consumed.** The design requires AutoNAT-verified direct reachability or an active relay reservation before a node advertises server mode. AutoNAT and Relay are absent from the libp2p feature list; SPIKE-004 is where they arrive. A node here advertises server mode because it was configured to.
- **Single-path capture is not shown to be reduced.** K24 measures it against controls — one seed versus three, `disjoint_paths` off versus on, nine routers of which two know the target, `parallelism` 3 so a walk cannot contact them all at once. **No capture was observed at all**: the single-seed asker reached the target too, so the topology cannot distinguish the configurations, and an absence of difference is not evidence for the option. K16 says the same about path width — five requests against five.

Neither is a failure of the design; both are questions this harness cannot pose. Recording PASS without naming them would have closed a release gate on evidence that does not exist.

Authoritative objective, evidence requirements, and decision gate live in [`architecture/roadmap/SPIKES.md`](../../architecture/roadmap/SPIKES.md); this file records what was actually observed.

Do not treat the experiment in [`harness/`](./harness) as production implementation. It is deliberately outside the workspace — an empty `[workspace]` table in its manifest — so it cannot be built by `cargo xtask ci`, cannot enter the root `Cargo.lock`, and cannot become a production dependency by a stray `path =`.

**That isolation is what keeps `kad` off the production feature list.** Cargo unifies features across one build of one workspace, so as a member this harness would switch Kademlia on inside `interweave-transport-libp2p` — undoing CLAUDE.md §3's "absent from the feature list rather than merely unused", and doing it with nothing in the production crate having changed. That rule is the whole reason this spike runs *before* Stage 10 rather than during it.

## What was pinned

```text
libp2p =0.56.0   features: tcp, noise, yamux, identify, tokio, macros,
                           ed25519, kad
tokio  =1.53.1   futures =0.3.34   sha2 =0.10.9
```

Every version is the one the root `Cargo.lock` resolves; `kad` is the only spike-only *feature*. The lock is committed, because half of what is recorded below is `libp2p-kad`'s own behaviour — an implicit bootstrap on routing insertion, protocol withdrawal on a mode change, what `BucketInserts::Manual` does and does not do — and every one of those is a patch-release-visible detail that Stage 10 is built on.

The harness uses the **production dial gate and the production connection policy by path**: `interweave-transport-libp2p`, `interweave-transport-runtime`, `interweave-transport-api`, `interweave-trust-api`. The dependency runs spike → product, which is the direction CLAUDE.md §4 permits. Measuring a copy of the gate would have measured a copy.

## How the dial measurement is honest

The brief forbids inferring behaviour-originated dial volume from the absence of ordinary scheduler calls. `SwarmEvent::Dialing` cannot answer the question either: it says a dial happened, not who asked for it, and a `ToSwarm::Dial` from Kademlia is indistinguishable there from one the application made.

libp2p routes **every** dial through `NetworkBehaviour::handle_pending_outbound_connection`, synchronously inside `Swarm::dial` — which is the hook the production `OutboundAdmission` already uses. The harness's `InstrumentedGate` is that hook with counters, in the same first position in the behaviour struct. A dial arriving there with no registered admission ticket is behaviour-originated *by construction*, not by inference.

It runs in two modes, and both are measured:

- **`DenyUnadmitted`** — what production does today: refuse any dial without a root admission ticket.
- **`PolicyAdmit`** — the Stage 10 proposal: hand the dial to the root admission under `DialOrigin::KademliaQuery`, so trust, per-peer backoff, drain state and the ceilings all apply.

Measuring only the first would say the gate refuses everything; measuring only the second would say nothing about what ships now.

**`PolicyAdmit` asks the `ConnectionManager`, not `ConnectionPolicy`**, and the distinction is finding F7 below rather than an implementation detail.

It uses a **live clock** — finding F8b: `now_ms` was a field pinned at zero, so every admission and settlement carried the same timestamp, a backoff recorded at 0 with a 30-second delay expired at a moment the clock never reached, and `PeerBackoff` was permanent rather than temporary. Every experiment asserting the immediate refusal passed throughout.

It also handles the ticket the way the runtime must, which is finding **F8**: a `DialTicket` reserves a pending-dial slot **and** the connection it may become, and its `Drop` returns both. So the gate holds the ticket while the dial is in flight; on failure it hands it back through `record_failure`, and on success it converts it with `record_success` into a `ConnectionSlot` that it keeps until the connection closes. Dropping the ticket on receipt bounds nothing; dropping it when the dial *establishes* bounds `max_pending_dials` and silently exempts `max_connections`. The second is the subtler of the two and is exactly what the first version of this fix did.

## What was observed

188 assertions across 28 experiments, consecutive clean runs. The harness exits non-zero when any required observation is false, so `cargo run` cannot report success while its own output disproves the record.

**Namespace (K1).** The published golden vector reproduces exactly: `network_id: example-private-network` → `ssbtblqj7mexczivog5qfbfjvi` → `/interweave/kad/1.0.0/ssbtblqj7mexczivog5qfbfjvi`. The derivation is implemented from the specification text rather than from a shared helper, so a derivation that merely agrees with itself could not pass. The 26-character unpadded base32 tag is a valid `libp2p::StreamProtocol`, and the `^[a-z0-9][a-z0-9._-]{0,63}$` grammar accepts and refuses what the spec says it should.

**Silence when absent (K2).** A node built without the behaviour advertises no Kademlia protocol, originates no dial, and runs no query — against a server-mode control in the same run that advertises the derived protocol, so the negative is a fact about the node and not about the topology.

**Manual bucket inserts (K3).** An authenticated, identified connection puts **nobody** in the routing table. One explicit `add_address` on the same peer over the same connection does, and is reported as a routing update.

**Modes (K4).** A server advertises the derived protocol; a client does not, so a client is not a routing target. A client-mode node still runs a query to completion.

**Bootstrap (K5).** `bootstrap()` on an empty routing table returns `NoKnownPeers`, and succeeds and completes once one peer is known. A single `add_address` on a previously-empty table starts **exactly one** query the caller never requested, asserted rather than printed — the count is what finding F2 rests on.

**Behaviour dials, gated as production gates them today (K6).** A three-node walk where the asker knows only the router, and the router knows the target: the query originates a dial the application never requested, aimed at the peer the walk is walking toward, and **today's gate refuses every one of them**. No connection is established — and the refusal is what prevents it, not the topology, because the same walk under `PolicyAdmit` (K7) reaches the peer through a behaviour dial the policy admitted.

**Trust (K8).** The same walk with the target *not* in the asker's trust policy: the router returns it, the query tries to dial it, the gate refuses with `unauthorized`, and the asker never dials it. A malicious trusted router cannot cause a connection to an unauthorized peer by placing it in a response.

**Backoff and drain (K9).** A peer put into backoff through the manager's own failure path — admit a dial, record it failed, exactly as the runtime does when a dial does not come back, and nothing about Kademlia is told — has its Kademlia dials refused for `peer backoff`. A draining node is still *asked* for dials and refuses **every one** as `shutting down`; the experiment queries for a peer it is not connected to, because querying a random key let the existing routing connection satisfy the walk and "no dial happened" then read as "the drain refused it".

**The ceilings (K19).** Three parts, because each catches what the others miss. With a pending-dial ceiling of one already filled by an ordinary dial, a Kademlia query's dials are refused as `too many pending dials`; releasing that dial returns the slot and the same query is then admitted, so the ceiling is a live count rather than a latch. The second part has **no external ticket at all** — five routed-but-unconnected routers and a fan-out query — so the ceiling can only be filled by the gate's own tickets, and 10 of 15 dials were refused. The third sets `max_connections` to one against reachable routers: a behaviour dial that *establishes* keeps its slot, the manager counts it, and the remaining dials are refused as `connection limit reached`.

Each part fails to a different mutation and passes the others', which is the reason there are three: dropping the ticket on receipt fails the second only; dropping it on `ConnectionEstablished` fails the third only.

**Records (K10).** `PUT_VALUE` and `ADD_PROVIDER` are shown to be **sent**, to **arrive** — counted as inbound requests at the receiver — and to store nothing: zero records, zero provider records. Arrival is asserted rather than assumed, because an empty store proves filtering only if the write reached it; a negotiation failure, an absent route and an unsent request all produce the same empty store.

**Exploration and convergence (K11, K17).** A ten-node line seeded one-deep closes to **9/9 entries on every node** under random exploration — asserted per node, because a weaker predicate passed a run that converged to a staircase. Twenty nodes seeded in a star at a single hub converge to **19/19 routing entries on every node** within five rounds, ~60s of wall clock on one machine, with no routing table exceeding its bound. Every behaviour dial in that run — around 220 of them — is **accounted for**: admitted plus refused equals originated, and every refusal is an explained policy outcome (`peer backoff`) rather than an unexpected class.

That is deliberately weaker than the zero-refusal assertion it replaces, and the reason is worth stating. Once the dial hook stopped acting on address denials it could not evaluate (F16), dials that had been refused began to proceed — and some genuinely fail, against a remote at its own ceiling or a peer mid-restart, after which the manager records the failure and refuses the next attempt. The policy working is not the gate malfunctioning, and a run where that happens must not be reported as a failure.

What rules out the case the zero-refusal assertion was guarding against — a run in which dials are refused wholesale — is **K17.1**, which requires every node to have reached 19/19. A network cannot converge while its dials are being refused.

**Project exploration rules (K12).** Effective target, no-progress backoff and saturation are project logic, not library behaviour. Implemented as a state machine over the signal the library actually provides: the delay doubles per no-progress round and caps at 15 minutes, saturation needs three consecutive no-progress rounds *and* a usable peer *and* no targetable observation outside the routing set, progress resets it, and a trust or seed change invalidates it. `effective_target(64, 256, 2) == 2` is what stops a three-peer overlay being permanently degraded by a default of 64.

**Capability observation (K13).** A server on *this* `network_id` is observed advertising the exact protocol; a server on a different `network_id` is not — the hash is genuinely part of the evidence. A node dropped to client mode stops advertising the server protocol, and the fresh Identify exchange **replaces** the observation rather than merging it.

**What the dial hook can and cannot decide (K21).** libp2p calls `handle_pending_outbound_connection` with an **empty** candidate list for a behaviour dial — measured, not assumed: the harness records the count it was handed, and it is zero every time. The hook is where behaviours *contribute* addresses, and the union is dialled after it returns. So trust, per-peer backoff, drain and the ceilings can be decided there and **address-scoped policy cannot**, however carefully the list is walked. The address check therefore happens at `handle_established_outbound_connection`, which is handed the address that was actually used: a peer whose address is quarantined is dialled, the connection is refused on that address, and no connection to it survives — against a control showing a different address for the same peer is still admissible.

**Dial volume by query class (K23).** The release criterion asks for volume measured by class, and libp2p does not supply it: the dial hook receives a connection id and a peer and nothing else — no query id, no originating behaviour. So attribution comes from what the *provider* declares it is running. Measured in three states: dials from work the provider never started (the implicit bootstrap of F2) are attributed to `none`; with one class in flight every dial is attributed to it and the per-class total accounts for every behaviour dial; with two in flight the attribution is the SET rather than a guess at one of them.

**The bounded query scheduler (K22).** A concurrency ceiling and a rate ceiling, shared across the three query classes. `kad::Config::set_parallelism` is not this: it bounds the peers ONE query contacts at a time and says nothing about how many queries a provider may run or how often it may start them. Modelled here and asserted: the concurrency budget admits exactly its number and refuses the next *for concurrency*; a finished query returns its slot; start-and-finish is bounded by the RATE, which concurrency cannot stand in for since a prompt caller never reaches it; the window slides rather than resetting, so the budget cannot be spent twice across a boundary; and driving real `kad` queries through it, a driver asking for ten starts exactly two and the node really has two in flight.

**The routing bound under pressure (K17.5/K17.6).** `max_routing_peers` is project logic applied before manual insertion, and `kbucket_size` does not stand in for it — a table can hold `kbucket_size` entries in each of many buckets and still exceed the total. It is only testable against a population *larger* than the bound and on a *fresh* node, since a bound stops a table growing and cannot shrink one already full. Two newcomers join the converged twenty-node network from the same seed: the bounded one stops at 5, its twin reaches 20.

**A dial where every candidate fails (K25).** K18's topology keeps a good route to the target alive, which suppresses peer backoff — so its multi-address assertion never meets the ordinary case, where the first settlement advances peer backoff and every later `admit` is refused for it. With all candidates dead, a settlement loop that admits as it goes scores the first address and silently drops the rest. Every ticket is therefore minted *before* any is settled, and K25 names both dead addresses in the peer's candidates afterwards. The same dial is then run under a ceiling of one, where pre-minting cannot get a ticket for the second address: one is scored, one is counted as unsettled, and the shortfall is reported rather than silent — F15.

**Revocation propagates (K20, K28).** `set_trust` is given the live peer list, so it can say which connections a revision invalidates, and the `Revoked` it returns is acted on: the peer leaves the Kademlia routing table and its connection is closed, because the connection is multiplexed and every other protocol rides it. An earlier version passed an empty live list, ignored the result, and let the experiments disconnect by hand — under which an implementation that left revoked peers connected and routable would have passed unchanged. K28 now revokes and asserts the connection goes and the routing entry goes, with nobody disconnecting anything.

**Trust withdrawn mid-dial (K20, K28).** The gate admits against the trust of the moment it is asked and settles later, so the settlement reclassifies rather than trusting the admission's answer. With trust intact a behaviour connection is retained; revocation genuinely changes what the settlement reads (`DataPlaneTrusted` → `Unauthorized`); and after revocation nothing is retained for that peer.

Releasing the ticket is only half of it. The reservation goes back and the connection stays **live** — established, unauthorized, and outside the manager's accounting, which is the same fail-open shape as a settlement that cannot account for itself. Both now queue a close. K28 covers what it can of the connection half: a retained connection survives (the control), a revoked peer's connection does not, and no new one is admitted afterwards.

**The close on the withdrawn branch is not asserted**, for the same reason K20's reclassification is not — reaching it needs a settlement to land after revocation, and this harness cannot force that window. Removing it would fail nothing here. It is a fail-open path removed because the production settlement path does the same, not on evidence, and this record does not pretend otherwise.

**Targeted lookup (K14).** §9.2's eligibility rule is project logic, implemented over the observation the library provides and denied one conjunct at a time: an untrusted target, absent evidence, *negative* evidence, a client-mode observation, evidence from another `network_id`, evidence for another wire major, stale evidence past the TTL, a peer that already has a usable address, an unexpired cooldown, and an exhausted budget each refuse it on their own. Then the lookup itself runs for real — an asker holding **no** address for the target asks the DHT by PeerId and recovers the address a router knows.

**F17 — a client-mode peer is not returned by a walk at all.** The router holds it in its routing table — asserted — and a `get_closest_peers` result still does not contain it, because the walk verifies a peer by contacting it and a client-mode node does not answer the protocol. So §9.2's "client nodes are not assumed discoverable by PeerId through FIND_NODE" is enforced by rust-libp2p itself rather than by the provider. The corollary shapes what can be tested: anything a walk *does* return has necessarily been contacted and identified, so the "query result with no capability evidence" state barely exists, and the capability gate has to be exercised on the evidence rather than on its absence.

**Capability-aware admission (K26).** §7's pipeline ends in "exact current Kademlia server protocol advertised" and does not exempt the source of a candidate — a query result is *advisory*, saying a peer exists at an address, not that it serves this DHT. The distinction is only visible with a peer that is trusted, reachable, identified and NOT a server: a client-mode node, which §9.2 says must not be misrepresented as discoverable. It is connected and identified alongside a server, both are candidates by every other measure, and only the server enters the routing table. The gate itself is then exercised on the evidence: the same query-returned candidate, in the same run, admitted against a protocol it does not advertise is refused, and admitted against the one it does advertise goes in.

**Snapshot (K15).** Every `SnapshotResult` field the driver port specifies is computable from the real API, and every one is a scalar or a fixed-width tag: no routing dump, no peer list, no payload. `pending_behaviour_dials` is the gate's **live** gauge and is asserted against its cumulative total, because reporting the total would show settled dials as in flight — a materially wrong diagnostic rather than an imprecise one. The **asynchronous** half is exercised over a real bounded channel and a real deadline, through an actual consumer rather than by comparing numbers: a correlated answer arrives inside the deadline; a missing one is a bounded timeout rather than a hang; a consumer waiting for its request id *refuses another request's answer and keeps waiting*, then returns the matching one that follows — the pair that distinguishes correlating from both accepting-by-arrival-order and refusing-everything; and the channel refuses when full.

**Disjoint paths (K16).** With `parallelism = 3` and disjoint paths enabled, the explicit query contacts five routers rather than one — counted for that query alone, since taking the maximum across all queries would have measured the implicit bootstrap of F2 as readily as the query under test. Against an otherwise-identical `disjoint_paths = false` control the count is **also five**: at six nodes on loopback the option changes nothing measurable, because the whole network fits inside one round of `parallelism = 3` twice over and there is no second path to make disjoint. That is a fact about the topology rather than about the option, and it is recorded as a result rather than dressed up as one.

**Stale routing response (K18).** A router holding a real trusted peer at a dead address hands it over; the asker dials it and the dial fails. Fed to the production policy the way the runtime would, that address-scoped failure does **not** advance peer backoff, and the known-good route to the same peer stays dialable.

## Findings that constrain Stage 10

**F1 — Stage 10 cannot begin by enabling the feature.** The production `OutboundAdmission` refuses every dial carrying no root admission ticket, and every Kademlia query dial carries none. Turning `kad` on without extending the gate produces a subsystem whose every query dies at the first hop it does not already have a connection for — silently, since a refused behaviour dial surfaces as an ordinary dial failure. The gate must learn to admit a behaviour-originated dial *through* `PolicySnapshot::admit` under `DialOrigin::KademliaQuery`, which is what `InstrumentedGate`'s `PolicyAdmit` mode prototypes. That mode is a **proposal measured in the harness, not production code**.

**F2 — A routing insertion starts one query nobody asked for.** `add_address` on a previously-empty routing table produced exactly one `OutboundQueryProgressed` for a query the caller never started, on every run. The design already anticipated this ("any automatic bootstrap triggered by the selected rust-libp2p version on routing-table insertion must be measured and counted"); it is real, it is one query per transition, and it **dials**. Two consequences: the provider's query budget must account for it, and any code that installs policy *after* seeding will install it after the dial it meant to govern. Both experiments that measure a gated dial had to seed and query in that order to observe anything, which is the same trap the implementation will hit.

**F3 — Under `BucketInserts::Manual`, a seed node routes nobody, and a star does not converge.** The first run of the twenty-node experiment measured total non-convergence: every spoke had one routing entry and the hub had zero, because inbound connections insert nobody and the hub therefore answered every query with an empty list. The admission pipeline must treat an **inbound** connection's Identify observation as a candidate — peer, its reported listen addresses, and the exact advertised server protocol — not only the peers a query returns. `kademlia-integration.md` §7 describes the pipeline but reads as an outbound story; the inbound direction is what a bootstrap node lives on.

**F4 — A query result cannot distinguish a lying router from a peer that is down.** `GetClosestPeers` does not report a peer whose only address fails to connect, so "the router handed over a poisoned address" and "the peer moved" look identical in the result set. The diagnostic must come from the dial outcome, which is where the address is named — and it is why K18 asserts on the dial error rather than the query result.

**F5 — Capability evidence is withdrawn, not merely aged out.** A mode change removes the advertised protocol from the very next Identify exchange. The cache must therefore *replace* an observation on fresh evidence rather than union it, and negative evidence must be able to overwrite positive evidence before the TTL expires.

**F6 — `network_id` separation holds at the protocol level.** Two nodes on the same crate and version, differing only in `network_id`, advertise different protocols and do not mix. Positive capability evidence keyed to the exact wire major and network hash is implementable as specified.

**F7 — `ConnectionPolicy::admit` is not the root admission, and the difference is invisible until it matters.** The policy answers trust, per-peer backoff, address quarantine and drain state. The **pending-dial and connection ceilings are enforced one layer up**, in the manager, which is also what mints the `DialTicket` that reserves them. A Stage 10 gate that consults the policy directly will therefore refuse an untrusted or backed-off peer perfectly — every trust test passes — while `max_pending_dials` and `max_connections` influence no Kademlia dial at all. This spike's first version did exactly that, and its limits experiment passed.

**F8b — a gate needs a real clock, and a frozen one fails open in a way every test agrees with.** `now_ms` pinned at zero stamps every admission and every settlement at the same instant. A backoff recorded at 0 with the manager's 30-second base delay expires at 30_000, which a frozen clock never reaches — so `PeerBackoff` becomes permanent, the peer is never retried, and every assertion about the *immediate* refusal keeps passing. The clock is elapsed real time, and `K9.6` asserts it advances, because that is the only observation a frozen clock fails.

**F8 — a `DialTicket` reserves two things, and settling it wrongly exempts one of them silently.** The ticket holds a pending-dial slot and the connection that dial may become; `Drop` returns both. A gate that drops it on receipt bounds nothing. A gate that drops it when the dial **establishes** — which reads like the obvious cleanup — bounds `max_pending_dials` correctly and exempts `max_connections` entirely, because the connection reservation goes back at the moment the connection starts existing. `record_success` is what converts the ticket into a `ConnectionSlot` that keeps it, and the slot is released with `record_connection_closed`. Both wrong versions were written here before the right one, and each passed every ceiling assertion the other failed.

**F9 — address-scoped policy cannot be enforced at the dial hook, because the address does not exist yet.** For a behaviour-originated dial `handle_pending_outbound_connection` receives **no** candidate addresses: it is the hook where behaviours contribute them. A gate that checks `addresses.first()` is not merely incomplete, it is reading an empty list and admitting every quarantined route while appearing to check. The address is available at `handle_established_outbound_connection` — after TCP connect, before the handler exists — which is later than production's check and the only place a behaviour dial has one at all. Stage 10 must decide whether that lateness is acceptable, and the cost is one TCP connect to a suppressed address.

**F10 — the two halves of the system key the same route differently.** A behaviour dial's address arrives as `/ip4/…/tcp/…/p2p/<peer>`, because a query result carries the peer component. The address book and the quarantine map are keyed by the bare transport address, which is what `AdmittedDial` binds. Passing the suffixed form to the policy looks up an address it has never seen, so **every quarantine silently misses** and the dial is admitted on a route the policy had suppressed. Normalising — stripping the `/p2p` component — is what makes F9's remedy work at all.

**F11 — an address probe must discard the capacity answers.** The check at the established hook asks the same `admit`, which decides policy *and* takes a reservation; the dial being checked already holds one. At a tight ceiling the probe is therefore refused for capacity that this very dial is occupying, and refusing on that denies every behaviour connection. Capacity was decided when the ticket was minted; the late hook is for the address and for authority that can have changed since. This was measured rather than predicted — it broke the connection-ceiling experiment.

**F12 — a `DialTicket` binds its address at admission, which a behaviour dial cannot supply.** F9 says the address does not exist at the dial hook; the consequence is that the ticket carries an empty placeholder, and `record_success` / `record_failure` feed `ticket.address()` to the address policy and the address book. So **every Kademlia route settles against one empty entry**: the address that worked never becomes known-good, the address that failed is never scored, and the address book never learns either. The harness works around it by re-minting the settlement ticket against the address actually used — recovered from the established hook, or from `DialError::Transport` when the dial never established, which is the case the established hook cannot reach. That is a workaround, not a design: Stage 10 needs either a re-bindable ticket or a settlement API that takes the address.

**F13 — the dial hook cannot attribute a dial to a query.** libp2p hands `handle_pending_outbound_connection` a connection id and a peer; for a behaviour dial there is no query id and no originating behaviour. Per-class dial volume therefore cannot be read off the dial and must come from the provider declaring what it is running — which is exact while one class is in flight and a set when several are. Stage 10 can narrow that by serialising classes or widening the driver port; it cannot recover it from libp2p.

**F15 — an address failure cannot be recorded without passing the policy that failure just changed.** `record_failure` takes a `DialTicket`, and a ticket comes only from `admit`. Settling a fully-failed multi-address dial therefore has no correct ordering inside the current API: settle as you go and the first failure's peer backoff refuses the rest; mint every ticket first and settlement depends on one spare pending-dial and connection slot per address, which a tight ceiling does not have. The harness pre-mints, which is the better of the two, and **counts** what it could not settle. Stage 10 needs an address-scoped failure API that requires no admission.

**F16 — the dial hook must not act on address-scoped denials it cannot evaluate, and cannot fully avoid them either.** Its probe carries the empty placeholder (F9), so `AddressQuarantined` is a verdict about an address that does not exist and `PolicyStateFull` reports that the address *table* has no room — which under pressure refuses every Kademlia dial, including ones whose real address is already known-good. The probe defers both to the established hook, where the address is real.

**The reservation cannot be deferred, and that is the harder half.** `admit` decides policy *and* takes the reservation in one call, so a dial whose placeholder request is refused cannot obtain a ticket — and admitting without one leaves the ceilings bounding nothing, which is F8 and F11 again. The only available answer is to refuse. So under address-table pressure behaviour dials stop entirely, fail-closed: the safe direction, a real availability cost, and not something deferring the probe removes. Deferring the probe alone changes nothing at all, which is what made the first version of this fix ineffective; K27 asserts both halves for that reason. Stage 10 needs an admission that can reserve capacity without deciding an address it was not given.

At the established hook the distinction inverts: `PolicyStateFull` there is *not* a capacity denial to be discarded alongside the ceilings — the request carries the real address, so it says the table cannot hold an entry for the route this connection actually used, which is the fail-closed address bound and precisely what was deferred to that point.

## Stated limits

These are the things this spike did **not** establish, recorded so no future reader mistakes its silence for a result.

- **No adversary, and disjoint paths is not shown to do anything.** K16 measures query path **width** against a disabled control and finds no difference at six nodes; K24 measures single-path **capture** against controls at nine routers with `parallelism` 3, and finds no capture to reduce — a single-seed asker reached the target as reliably as a three-seed one. So the option is shown to be configurable and harmless, and nothing more. The weakest adversary K24 models is a router that truthfully does not know the target; nothing here models one that lies, and no claim about Byzantine resistance is made or implied.
- **No hostile *protocol* peer.** K8, K10 and K18 model a peer that returns unauthorized peers, writes records, and hands over dead addresses. None models a peer that violates the Kademlia wire format itself.
- **One machine, loopback only.** No NAT, no latency, no loss, no interface change. The twenty-node convergence figure is a convergence *shape*, not a deployment number.
- **Server-mode reachability evidence is not consumed.** The design requires AutoNAT-verified direct reachability or an active relay reservation as strong evidence before a node advertises server mode. AutoNAT and Relay are absent from the feature list — SPIKE-004 is where they arrive — so this spike **cannot** validate that rule, and Stage 10 must not treat it as validated. A node here advertises server mode because it was configured to.
- **The exploration rules are validated as logic, not as deployment behaviour.** K12 exercises the state machine over synthetic rounds. Whether three no-progress rounds is the right threshold on a real network is not a question a five-node loopback topology can answer.
- **`PolicyAdmit` is a prototype.** It demonstrates that the production admission *can* decide a behaviour dial, that its answers reach the library correctly, and that both ceilings bind when the ticket is settled properly. It is not the production gate, has not been reviewed as one, and Stage 10 owns writing it.
- **Per-class dial attribution is exact only for one class at a time.** F13 is a limit on any implementation, not on this harness: with two classes in flight the honest answer is the set, and K23 asserts the set rather than picking one.
- **The trust-withdrawn race window is not driven.** K20 and K28 establish that the settlement reads a classification revocation really changes, that the trusted path retains and its connection survives, that nothing is retained for a revoked peer, and that no path leads back to a live connection for one. Neither drives the admit-then-revoke-then-establish window itself: that gap is milliseconds on loopback and this harness cannot open it on demand. So **two** things in that branch are unasserted — the reclassification check and the close that follows it — and removing either fails no observation here. Both are stated rather than hidden; Stage 10 owns a test that can hold a dial open.
- **The query scheduler is modelled, not wired into a driver.** K22 exercises the budgets, and K11 and K17 now drive the ten- and twenty-node **convergence** through it — a permit before every exploration query, with as many started as were permitted — so the convergence figures are convergence within the budgets rather than with them bypassed. The remaining experiments still call the behaviour directly, which is right: they are about the gate and the policy, not the scheduler. What is not established is any particular driver consulting it.
- **The Snapshot channel is modelled, not driven.** K15 exercises correlation, the deadline, miscorrelation and the bound over a real Tokio channel, but there is no `KadCommand` enum and no driver task here: the Swarm is polled directly. What is established is that the specified semantics are implementable and that each failure mode is detectable — not that any particular driver implements them.

## Reproducing

```
cd spikes/spike-003/harness
cargo run
```

Around six minutes, mostly waiting on real socket timeouts and query settling. Exit code 0 means every required observation held; non-zero prints which did not.

Passing a single experiment id — `cargo run -- K14` — runs only that one, for iterating on a failure without paying the whole set.

Three mutations confirm the assertions are load-bearing rather than agreeing with the code for free:

- making the gate admit every behaviour dial unconditionally fails K8.3, K8.4, K9.2 and K9.3 — the trust and backoff refusals;
- setting `StoreInserts::Unfiltered` and `BucketInserts::OnConnected` fails K3.2, K5.1, K10.3, K11.2 and the K18 block;
- making the gate **drop** its `DialTicket` instead of holding it fails K19.7 — and nothing else, because every other ceiling assertion fills the ceiling with an ordinary dial's ticket;
- making the gate drop the ticket when a dial **establishes**, instead of converting it with `record_success`, fails K19.9 — and nothing else, including K19.7;
- freezing the gate's clock at zero fails K9.6, and only K9.6 — which is the point of having it, since a frozen clock leaves every backoff-refusal assertion green;
- removing the address check at the established hook fails K21.6 and K21.7;
- settling against the placeholder instead of the address used fails K18.6;
- ignoring the project routing bound fails K17.5, with its control unaffected;
- making the scheduler's completion release any slot rather than its own fails K22.3, K22.4 and K22.5;
- scoring only the first of a multi-address dial's failed addresses fails K18.7;
- attributing every dial to `none` regardless of what is running fails K23.2;
- invoking the behaviour before acquiring a permit fails K22.13 at ten calls for two permits;
- settling only the first of a fully-failed dial's addresses fails K25.1 and K25.2;
- a Snapshot consumer that accepts the first arrival by order fails K15.10 and K15.11;
- admitting a candidate without the exact server protocol fails K26.8;
- treating the placeholder as a real address, so the probe never defers, fails K27.3 and K27.4.

One measurement is reported rather than asserted, because both outcomes are legitimate: K25.4 accepts either a complete settlement or a counted shortfall, and refuses only the third case — addresses vanishing while the ledger reads zero.

One mutation deliberately fails nothing: removing the settlement's reclassification. That is K20's stated limit, confirmed rather than papered over.
