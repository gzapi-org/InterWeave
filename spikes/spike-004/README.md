# SPIKE-004 — AutoNAT v2 / Relay v2 / DCUtR, phase A

AutoNAT v2, Circuit Relay v2 and DCUtR measured against the **production
root dial gate**, on the exact rust-libp2p the product pins.

Do not treat experiments placed here as production implementation.
Evidence and the final decision are recorded against
[`architecture/roadmap/SPIKES.md`](../../architecture/roadmap/SPIKES.md).

## What this phase does and does not cover

SPIKE-004's brief asks for an environment matrix — public VM, home NAT,
symmetric and carrier NAT, two independently operated relay services,
network-interface change. **This is phase A: one machine, loopback.** It
answers the half of the brief that is about protocol semantics, dial
attribution, admission-class enforcement and state machines, and it
answers none of the half that is about NAT traversal actually working.

**Phase B — required before Stage 11 CLOSURE and before v1 release —**
is the real-network matrix: hole-punch success rates against real NATs,
two independent public relays, carrier NAT, interface change, and
measured resource cost. Nothing in this record speaks to any of that,
and the stage's exit gate ("the mandatory standard-v1 NAT/relay/
hole-punch matrix passes") cannot be met from loopback.

## What was pinned

`libp2p = "=0.56.0"` — exact, with `Cargo.lock` committed beside it —
carrying the production feature list plus the three spike-only features
`autonat`, `relay`, `dcutr`. Most of what is recorded below is the
library's own behaviour: probe event shapes, what the AutoNAT server
validates, whether a dial carries an address at the pending hook. A
floating `0.56` resolves a later patch, and a result that cannot be
rebuilt is not evidence.

The harness is its own workspace (an empty `[workspace]` table). That is
what keeps `autonat`, `relay` and `dcutr` off the production feature
list: Cargo unifies features across one build of one workspace, so as a
member this harness would switch all three on inside
`interweave-transport-libp2p` invisibly — nothing in the production
crate would have changed.

## How the measurement is honest, and where it is a proposal

The harness depends on `interweave-transport-runtime`, `-api`,
`interweave-trust-api` and `interweave-transport-libp2p` **by path**.
The dependency runs spike → product, the direction CLAUDE.md §4 permits.

**The POLICY is production; the HOOK is a proposal, and the difference
matters.** `InstrumentedGate` asks `ConnectionManager::admit` through a
real `SnapshotHandle`, so every trust, class, backoff, quarantine and
ceiling decision here is the shipped one. The gate *behaviour* around it
is this harness's own, because production's
`OutboundAdmission::handle_pending_outbound_connection` hardcodes
`DialOrigin::KademliaQuery` — which is the thing under test. A spike
measuring a change cannot use the code the change replaces.

An earlier version of this section said the harness ran "the production
root gate", full stop, while no source file referenced
`interweave-transport-libp2p` at all: the dependency was declared and
unused. A review caught it.

**`R6` closes that gap.** It runs the real `OutboundAdmission`,
unmodified and by path, in front of a real `relay::client::Behaviour`,
and measures what the shipped gate answers when the relay client asks
to dial its relay. With the relay authorized as infrastructure only the
gate refuses — `kademlia dial refused: NotAuthorizedForDataPlane`, for a
dial no Kademlia made. Move that same relay into the data-plane
allowlist, change nothing else, and the identical dial is admitted and
connects. F1 is measured, not read.

Getting there took two corrections worth recording, because both are
the same mistake in different clothes — **an experiment whose subject
and control agree tells you about your fixture, not about your
subject**:

- The first version watched `SwarmEvent` and saw nothing: no dial, no
  error, in either configuration. Written up as "the relay client never
  dials", which was false. It dialled every time; the refusal is simply
  invisible from outside (F8), so the instrument had to move to where
  the decision is made.
- With the instrument moved, both configurations refused with
  `PolicySuperseded` — because `ProductionNode` dropped its
  `ConnectionManager` after taking a handle, and
  `SnapshotHandle::is_current` upgrades a weak reference to the manager
  and refuses when it is gone. Production is right to do that and has
  two tests pinning it; the harness was wrong. Holding the manager is
  what let the control separate from the subject.

`cargo run` exits non-zero if any required observation is false, so it
cannot report success while its own output disproves this file.

## Findings that constrain Stage 11

**F1 — a dial can be attributed to the behaviour that made it, and
Stage 11 does not work without it.** Production's pending hook is handed
a `ConnectionId`, an `Option<PeerId>` and nothing else; today it infers
`DialOrigin::KademliaQuery` because Kademlia is the only behaviour that
can dial. `KademliaQuery.is_data_plane()` is true, and R3.2 shows a
data-plane origin is refused for an infrastructure-only peer — so
without attribution **every relay reservation and every AutoNAT probe
would be refused against exactly the infrastructure the stack exists to
use.** R6 runs that: the shipped gate refuses a real relay client's
reservation dial toward an infrastructure-only relay with `kademlia
dial refused: NotAuthorizedForDataPlane`, and admits the same dial when
the only change is the relay's trust class (R6.5–R6.8).

It fails closed and it fails **silently** — not "as an ordinary dial
failure", which an earlier version of this sentence claimed and which
would at least have been visible. See F8.

The mechanism that works: a wrapper behaviour announces
`ConnectionId -> DialOrigin` from the originating behaviour's own
`poll`, because `DialOpts` already carries the `ConnectionId` the Swarm
will use (`opts.connection_id()` — the same value libp2p-autonat's
server uses to correlate its own dial-back). R2 measures that the note
is always present when the gate looks: one relay-originated dial,
resolved as `RelayReservation`, **zero unattributed**. Disabling the
announcement fails R2.6 and R2.7.

**F2 — the AutoNAT v2 server does NOT implement AUTONAT.md §7's
dial-back restriction, and Stage 11 must implement it at the gate.**
`handle_request_internal` (`v2/server/handler/dial_request.rs`) pops the
LAST address the client supplied and, when it differs from the observed
address, requests "dial data" — an amortization cost — and then dials it
anyway. There is no literal-IP requirement, no candidate-IP-equals-
observed-source-IP check, and no special-use address-class filter
anywhere in that path. The crate priced the request; §7 requires
refusing it.

What makes this fixable rather than blocking: the dial-back is an
ordinary `ToSwarm::Dial` (`PeerCondition::Always`, `allocate_new_port`),
so it **traverses the root gate** — R4.4 records the permissive server's
dial-back admitted through it as `autonat-probe`.

**And the check belongs at the PENDING hook, not the established one.**
An earlier version of this paragraph said the opposite, and a review
caught it: `handle_established_outbound_connection` runs *after* the TCP
connection is open, so a server validating there would already have
contacted the target — which is the entirety of what an SSRF check
exists to prevent. R4.10 measures that the dial-back candidate IS
present at the pending hook (one address, `[1]`), before any socket, so
an address check there precedes contact.

Two things the gate still cannot do alone, and Stage 11 must solve:

- **The gate does not know the observed source address** of the probing
  connection, and §7's central rule is *candidate IP equals observed
  source IP*. That correlation lives in the AutoNAT server's own
  request handling, not in a dial hook — so the wrapper or replacement
  around `autonat::v2::server` has to carry it, with the gate as the
  second line rather than the only one.
- **Address-class filtering** (literal IP, no DNS, no loopback/private/
  link-local/special-use) needs no such correlation and can be enforced
  at the pending hook for every behaviour dial at once.

**F3 — `RelayCircuit` is a command-path origin, not a
behaviour-originated one.** Review finding on PR #69: the first version
of the attribution wrapper gave each behaviour one FIXED origin, so
`relay::client::Behaviour` announced everything as `RelayReservation`
and `RelayCircuit` could never reach the gate. The wrapper now takes a
classifier per dial — and R5 shows the split it was built for is not
where the variant comes from.

Dialling `/…/p2p-circuit/p2p/<dest>` is handled by the relay
**transport**. The behaviour emits no `ToSwarm::Dial`, so no poll-time
mechanism can see one, and R5.2 records the source's circuit dial
resolving as `manual` — the command path — rather than as a circuit.
The conclusion for Stage 11: **`GatedSwarm::dial` sets `RelayCircuit`
from the address it was handed**, because the caller dialling through a
relay is the party that knows. The classifier still earns its place for
the reservation-vs-circuit split if a future crate version dials
circuits from the behaviour; it is not what produces the variant today.

**F4 — "the pending hook is handed an empty address list" is not
universal.** SPIKE-003 recorded F9 from Kademlia's behaviour, where the
hook exists so each behaviour may CONTRIBUTE addresses and the list
arrives empty. R2.4 measures **one** address at the pending hook for a
relay-reservation dial, and R4.10 measures one for an AutoNAT
dial-back. So a Stage 11 gate must not assume either shape:
`addresses.first()` is sometimes there and sometimes not.

**Which does not mean deferring the decision to the established hook.**
An earlier version of this paragraph said it did, and it survived the
correction to F2 above by one sentence — the two halves of one rule,
edited apart. For an SSRF filter the established hook is too late by
construction: the socket is already open and the target already
contacted. The rule is that a check which MUST precede contact belongs
at the pending hook and must handle the address being absent (refuse, or
defer to a per-behaviour wrapper that has it); only a check that may run
after contact — an identity mismatch, a quarantine — belongs at the
established hook, where the address is always available.

**F5 — `ConnectionPolicy::default()` refuses everything.** It carries
`max_pending_dials: 0` and `max_connections: 0`, both enforced by the
manager. A harness or test that takes the default and then asserts a
refusal will pass for `ConnectionLimitReached` while believing it
measured trust or class. This cost two rewrites here (see below) and is
worth stating because Stage 11's own tests will construct managers.

**D1 — `DcutrHolePunch` is admitted for an infrastructure-only peer,
and ADR-0036 says it must not be.** The one finding here that is a
defect in THIS project rather than in a dependency, and the reason the
harness has a "divergence" category at all.

ADR-0036's protocol-admission matrix reads *"DCUtR with that peer as
application destination | DataPlaneTrusted: yes |
ConnectivityInfrastructureOnly: **no**"*, and `DCUTR.md` §2 says never
to initiate DCUtR merely with an infrastructure-only peer as the
destination. `DialOrigin::is_data_plane` lists only `Manual`,
`ConnectionManager`, `DiscoveryReconnect` and `KademliaQuery`, so a
hole-punch counts as control-plane traffic and `ConnectionPolicy::admit`
lets it through.

**Stage 11 must fix this, and the two obvious fixes are not the same
rule.** Adding `DcutrHolePunch` to `is_data_plane` forbids every
hole-punch toward an infrastructure-only peer — but a hole-punch
*through* infrastructure toward a trusted peer is legitimate and is what
DCUtR is for. The matrix is about the peer as *application destination*,
so the check likely belongs on the destination's class rather than on
the origin alone. R3.5 asserts today's behaviour so that changing it
fails here rather than passing silently.

This was found because an earlier version of R3 *required* the
admission — recording the violation as evidence that the split held.

**D2 — `RelayCircuit` is admitted for an infrastructure-only
DESTINATION, and a circuit is application traffic by construction.**
The same omission from `is_data_plane`, one origin over, and it is the
more consequential of the two: a relayed circuit exists to carry the
data plane. ADR-0036's enforcement clause is explicit — *"the root dial
gate evaluates both requested dial purpose and destination class. It
must not authorize a generic application dial merely because the PeerId
is an infrastructure peer."* R7.4 pins today's answer (admitted); R7.5
is the control, showing the very same destination refused
`NotAuthorizedForDataPlane` under a data-plane origin, so this is the
origin's classification and not a broken class check.

**The fix is NOT the same as D1's, and is not "add both to
`is_data_plane`".** `RelayReservation` must stay non-data-plane: a
reservation *is* the reachability purpose, and making it data-plane
would refuse every relay the stack needs — which is F1 from the other
direction. `RelayCircuit` is different because of what R5.6 measured:
the relay behaviour originates no circuit dial, so the origin can only
be set by the caller, and the peer that caller names is the
**destination**. Two origins that look adjacent in the enum name
different parties.

The positive half of ADR-0036's relayed clause holds and is measured:
the source ends up connected to the destination's own authenticated
PeerId, separately from the relay's, with Identify completing through
the circuit (R7.9–R7.12). The rule "evaluate the end PeerId" is
decidable here because the end PeerId is what arrives.

**D3 — a relayed inbound connection is charged to a bucket the attacker
names.** `contracts/CONNECTIVITY.md` §10 requires charging the
authenticated relay transport connection and relay PeerId, plus the
global caps, and says the destination *"MUST NOT create unbounded
pseudo-source buckets from circuit metadata."*

R8 measured what the destination is handed before Noise: a remote of
`/p2p/<source>` — no IP anywhere (R8.4) — with the relay's PeerId sitting
in the LOCAL address (R8.5). R9 then calls the shipped
`PreAuthAdmission`'s own `handle_pending_inbound_connection` with exactly
those shapes. `source_label` returns a multiaddr as written when it
holds no IP, so the bucket is `/p2p/<source>`: **one bucket per source
identity, over one relay connection, and identities are free to mint.**
The relay's PeerId is right there in the local address and is not read.

The control is what makes it a finding rather than an observation
(R9.2): two DIRECT inbounds from one IP, same gate, same
`max_pending_per_source: 1` — the second is refused. The ceiling works.
The relayed pair escapes it. R9.4 measures what is left: the global
`max_pending_total` still holds at 8 of 32, so this is the bucket's
granularity failing, not the absence of any bound.

**Not reachable in a shipped build today** — no relay feature is
compiled, so no relayed inbound can arrive — and live the moment Phase 4
lands. That is why it is recorded against the code rather than against
the stage, exactly as D1 is.

**And the comment above `source_label` argues the opposite**, naming the
relayed case and calling it "the fail-closed direction … it cannot merge
two peers into one bucket, only fail to merge two addresses that belong
together." That reasoning is right for the memory transport and wrong
here: §10's risk is not merging, it is proliferation. An unenforced
claim about a case nobody had run — which is the shape this repository's
own rule about comments is written against.

**F6 — the infrastructure/data-plane split holds against the real
policy, in both directions.** R3 asks the production
`ConnectionManager::admit` for one peer authorized ONLY as
infrastructure: `RelayReservation`, `RelayCircuit`, `AutonatProbe` and
`DcutrHolePunch` are admitted; `KademliaQuery`, `ConnectionManager`,
`Manual` and `DiscoveryReconnect` are refused, and refused specifically
as `NotAuthorizedForDataPlane`. Adding the same peer to the data-plane
allowlist flips all four refusals to admissions, which is ADR-0036's
"data-plane trust wins" observed rather than restated.

**Two of the four admissions are the divergences, not the finding.**
`RelayReservation` and `AutonatProbe` are what the split exists to
permit. `DcutrHolePunch` is D1 and `RelayCircuit` is D2, and what F6
establishes is that the MECHANISM works — the policy reads the
destination's class and the origin's purpose and combines them — which
is why the two wrong answers are a question of which origins are on
which side rather than of whether the split is enforced.

**F7 — an infrastructure node advertises its control protocols, and
this harness cannot say anything about the data-plane ones.** R1.5 and
R1.7: a relay+AutoNAT server's Identify list is `/ipfs/id/1.0.0`,
`/ipfs/id/push/1.0.0`, `/libp2p/autonat/2/dial-request`,
`/libp2p/circuit/relay/0.2.0/hop`.

**That is the whole of it, and it is not an exposure baseline.** An
earlier version of this paragraph called it one. `SpikeBehaviour`
carries Identify and the three connectivity behaviours and nothing else
— no GossipSub, no direct, no endpoint directory, no Kademlia — so this
run cannot observe whether an infrastructure-only peer is offered a
data-plane protocol, which is the invariant §14 actually states.

**So the protocol-isolation correction is NOT unlocked by this
verdict.** It needs a node carrying both the data-plane behaviours and a
real infrastructure-only connection, which is phase-B-shaped work on
`SubstrateBehaviour` rather than on this harness. What phase A
establishes is narrower and still useful: the control protocols an
infrastructure peer advertises to US, which is the list a restriction
must leave intact.

**F8 — a gate refusal of a behaviour-originated dial is INVISIBLE, and
Stage 11 must make it observable.** The Swarm's handling of a
behaviour-emitted dial is `if let Ok(()) = self.dial(opts)`
(libp2p-swarm 0.47.1, `lib.rs:1098`). `Swarm::dial` builds
`DialError::Denied`, notifies the behaviour via `FromSwarm::DialFailure`
and returns it — and that `Err` is **discarded**. So a dial our own
policy refuses produces no `SwarmEvent::Dialing` and no
`SwarmEvent::OutgoingConnectionError`.

Precisely: the *originating behaviour* is told, via
`FromSwarm::DialFailure`, and reacts in its own terms. Nothing outside
that behaviour is told anything. So the refusal reaches an observer only
translated into whatever the behaviour does next — a Kademlia query that
fails, a relay listener that closes — with the policy that caused it
nowhere in the report.

R6.9 measures the absence and R6.11 is its positive control: the same
node, with the relay data-plane trusted, reports exactly one `Dialing`.
So the silence is the refusal, not a fixture that cannot report dials.

In R6 that translation is the relay client giving up on its listener —
reported as `ListenerClosed { reason: Ok(()) }`, a **successful** close
(R6.10). An operator watching a node that never obtains a reservation
sees a normal shutdown of a listener they did not ask to shut down.

This also corrects a sentence SPIKE-003 wrote and this plan repeated:
that a refused behaviour dial "surfaces as an ordinary dial failure".
For Kademlia it surfaces as a failed *query*, which is close enough to
be misleading; as an `OutgoingConnectionError` it does not surface at
all. The distinction matters because the remedy differs — a dial error
an operator can read names an address, and this names nothing.

Two smaller edges of the same finding:

- `DialError::Denied`'s `Display` is the bare string `"Dial error"` —
  the `print_error_chain` the other variants use is not reached for it.
- `ConnectionDenied`'s `Display` is `"connection denied"`. Everything
  `OutboundAdmission` writes about *why* — `kademlia dial refused:
  NotAuthorizedForDataPlane` — lives in `Error::source`, so a refusal
  logged the obvious way says nothing. R6 walks the chain deliberately;
  that it must is part of this finding.

**What binds Stage 11**: attribution (F1) removes the wrong refusals,
but a right refusal is just as silent. The stage owes an explicit
record at the gate — the pending hook already has the
`ConnectionId`, the peer and the verdict — because neither the Swarm
event stream nor the rendered error carries it.

**F9 — §8's reservation targets are expressible, and an address dies
with its relay.** `CONNECTIVITY.md` §8 keys reservation targets to
reachability state — two while Unknown or NotVerified, one when
VerifiedPublic, four at most — and requires the reservation-derived
address to be advertised while live and withdrawn immediately on loss.
The scheduling is ours; what the crate must supply is the ability to
hold more than one reservation at a time and to give an address up. R10
measures both: a client holds reservations on two relays at once (each
relay separately recording its own acceptance, so it is two relays and
not one renewing), advertises a circuit address derived from each, and
when one relay is dropped outright loses exactly that address while the
other survives. The surviving address is the control — without it the
same observation would pass for a client that abandoned relaying
altogether.

**Withdrawal is measured from the loss, not from a timer.** The first
version pumped ten seconds and then sampled once, which shows only that
the address is gone by the end and cannot see a stale window in which
peers keep dialling a dead relay — a review on PR #69 raised it. R10.9
now waits for the client to observe the connection closing, and R10.10
requires the address to be gone within one second of that: two orders of
magnitude below the reservation lifetime and far below any dial timeout,
so an address surviving it is a stale window rather than a scheduling
artefact.

**Not measured: renewal.** The crate's default reservation lasts an
hour, so nothing in a run of this length can see a refresh, and expiry
is asserted from nothing here.

**F10 — the relay server's defaults are not `RELAY.md` §8's, and one of
§8's ceilings has no knob at all.** R11 reads
`relay::Config::default()` on the pinned crate:

| `RELAY.md` §8 | libp2p-relay 0.21.1 default |
| --- | --- |
| `max_reservations` 64 | 128 — **looser** |
| `max_reservations_per_peer` 1 | 4 — **looser** |
| `reservation_duration` 1h | 1h |
| `max_circuits` 128 | 16 |
| `max_circuits_per_source_peer` 4 | `max_circuits_per_peer` 4 (same rule, different name) |
| `max_circuit_duration` 1h | **120s** |
| `max_circuit_bytes` 64 MiB | **128 KiB** |
| `max_pending_control` 64 | *no such field* |

Two of these break a deployment rather than merely differing from it:
128 KiB per circuit is three 48 KiB application payloads, and 120s is a
conversation. Two are looser than the specification, which is the
direction that matters for a budget. And `max_pending_control` cannot be
expressed by configuring this behaviour at all — Phase 4 must decide
between a wrapper and an amendment rather than discovering the absence
while writing the config struct.

**F11 — every per-peer ceiling admits one more than it says.** The crate
refuses when `num_circuits_of_peer(src) > max_circuits_per_peer`, a `>`
where a `>=` is meant, and the same shape guards
`max_reservations_per_peer`. R11 measures it rather than reading it:
with the ceiling set to **one**, a single source opens **two** circuits
to two destinations and the relay accepts both. The third is refused
`ResourceLimitExceeded` — which is the control, and the difference
between an off-by-one and a missing check.

So `max_reservations_per_peer: 1` from §8 yields two reservations per
peer, and `max_circuits_per_source_peer: 4` yields five. Phase 4 must
either subtract one when configuring or amend §8 to say what the numbers
mean; copying the table across is wrong in every row that has a
per-peer form.

The dials had to be issued one at a time for this to be measurable: the
relay counts circuits it has ACCEPTED, so two requests in flight
together are each counted against nothing, and a first attempt saw one
circuit accepted and concluded the ceiling held.

**F12 — DCUtR has no bounds of its own, and one attempt is not one
dial.** `dcutr::Behaviour::new` takes a `PeerId` and nothing else. The
crate's own ceiling, `MAX_NUMBER_OF_UPGRADE_ATTEMPTS = 3`, is a
`pub(crate)` constant counting retries per relayed connection — neither
a concurrency cap nor a cooldown. So §13's "at most four concurrent, one
per peer, five-minute failure cooldown" has to be enforced outside the
behaviour.

**Not by the gate alone, though.** The gate is the only place that sees
every dial, and R12.4 confirms every hole-punch dial arrives there
attributed — but it is handed no logical attempt identifier and no
outcome. R12.5 measures the asymmetry the conclusion rests on: both ends
dial for one punch and EXACTLY ONE reports the result, so the other end
never learns how its own attempt ended. A gate enforcing "one per peer"
on dials would refuse its own attempt's sibling candidates; one
enforcing the cooldown on dials would never learn that an attempt
failed. The attempt lifecycle belongs to a
DCUtR adapter, with a token for the attempt reaching the gate, which
admits or refuses it as a unit.

R12.4 says EVERY rather than at least one: the
announced and resolved counts for the origin agree on both nodes, with
zero dials of any origin meeting a gate with no note. `> 0` would have
been satisfied by one admitted dial per node while the rest bypassed the
gate — which is precisely the regression F12's conclusion depends on not
happening, and a review on PR #69 said so.
**But the gate cannot read "one attempt" off its own dial count.** Both
ends dial for a single punch — source one, destination one — and only
one of them reports a result, because the peer that reports is whichever
dial won. A per-peer rule counting dials would therefore be counting
something else.

**F13 — a successful punch is a second connection to a peer that is
already connected.** `CONNECTIVITY.md` §78 says a successful hole punch
for an already-connected relayed peer *"does not emit a second
`PeerConnected`"*. R12.7 shows why that is a rule rather than an
inheritance: the Swarm reports two `ConnectionEstablished` events naming
the same peer, one relayed and one direct, and nothing below the runtime
deduplicates them. The relayed connection is not torn down by the
upgrade either — R12.8 asserts that by CONNECTION, one relayed and one
direct open at once, because a set of PeerIds cannot tell a surviving
fallback from a replaced one and a review on PR #69 said so. §13's "keep
the relayed path as fallback" therefore has something to keep, but the
logical-peer view is Phase 6's to build.

## Stated limits

- **Loopback only.** No NAT of any kind. Every DCUtR observation here is
  about bounds and event shape, never about hole punching working.
- **No adversary.** No wire-protocol-violating peer, no relay that lies.
- **Server roles are single-instance.** ADR-0035 requires two
  independently operated relay/probe services; one process cannot show
  what redundancy buys.
- **No resource-cost measurement.** Relay bandwidth, connection and
  probe budgets are phase B.
- **Reservation REFRESH and expiry are not covered.** The crate's
  default reservation lasts an hour, so a run of this length cannot see
  a renewal. Obtain and withdrawal are measured (F9); the middle of the
  lifecycle is not.
- **DCUtR FAILURE is not observed.** On loopback a punch succeeds, so
  the cooldown, the retry ceiling and the fallback-on-failure rule are
  untested here; F12 is about where the bounds must live, not about
  them working. Failure behaviour is phase B.
- **Inbound admission is not gated here, only recorded.** R8's hooks
  observe; nothing refuses. The production gate is outbound-only and
  Stage 11 must build the inbound side, so R8's numbers say what a
  decision COULD read, never that one was made.

## Ten fixture bugs this run found in itself

Recorded because each one passed before it was caught, and each is the
same shape a Stage 11 test could take.

1. **R3 measured nothing.** With `ConnectionPolicy::default()` every
   origin was refused as `ConnectionLimitReached`; all four "is refused"
   assertions passed and all four "is admitted" failed. Asking
   `is_err()` cannot tell a class refusal from a zero ceiling. It now
   asserts the denial IS `NotAuthorizedForDataPlane`.
2. **R2 measured a manual dial.** The client was connected to the relay
   before it asked to reserve, so the relay behaviour never needed to
   dial and the only dial in the run was the harness's own. `> 0` passed
   on it. It now names the origin it expects and forces a genuine
   behaviour-originated dial by reserving on an unconnected relay.
3. **R4's evidence came from a lucky run.** Review finding on PR #69.
   R4.3–R4.5 were NOTES, so a run in which no probe reached the
   permissive server exited 0 while this file claimed a dial-back had
   crossed the gate — and the very next run did exactly that: the
   AutoNAT client picks among the servers it is connected to, it chose
   the strict one, and the permissive server made zero dials. Each
   scenario now uses one server so it is deterministic, the claims are
   REQUIRED rather than noted, and R4.8 is the control: an untrusting
   server's dial-back is still made by the crate and refused by its own
   gate, so R4.6 cannot pass for a gate that admits everything.
4. **This file asserted what the harness only printed.** Four findings
   — F3, F4, F6, F7 — were stated as established while the run could
   not fail on any of them. F6's precedence rule was the sharpest: the
   README said adding the peer to the data-plane allowlist flips all
   four refusals, which was true of a mutation run by hand and of
   nothing the harness did, so a regression in that path would have
   left every assertion passing. R2.4's note even printed "expected all
   zero" while F4 claimed one.
5. **R3 required the ADR violation.** The first version asserted that
   `DcutrHolePunch` *should* be admitted for an infrastructure-only
   peer, so D1 — a real defect in this project's policy — was recorded
   as evidence that the class split worked. A spike that asserts the
   current behaviour is correct cannot find a bug in it.
6. **The cleanup test could not fail either.** R2.10 was written with
   a dial to a dead address, which the Swarm ACCEPTS — the gate saw it,
   the note was consumed normally, and deleting the cleanup left the
   observation green. Only a dial the Swarm refuses synchronously, on a
   false `PeerCondition`, skips the hook; R2.11 establishes that
   refusal before R2.10 counts. The second vacuous test in this PR
   found by running the mutation rather than by reading it.
7. **R4 trusted a peer that did not exist.** `Node::new` mints its own
   keypair; the fixture generated a separate one to name in the servers'
   allowlists, so the dial-back was refused as `Unauthorized` — which
   read as a finding about the crate and was a bug in the fixture.
8. **R6 watched the wrong place and wrote up the silence as a
   finding.** The first version measured `SwarmEvent` and concluded
   "the relay client never dials", because a refused behaviour dial
   emits nothing (F8). It dialled on every run. Only its control saved
   it: subject and control were identical, so the result had to be
   recorded as a fact about the fixture rather than about the gate. An
   experiment whose control agrees with its subject has measured
   neither.
9. **R6 then refused everything, for a reason unrelated to trust.**
   `ProductionNode` took a `SnapshotHandle` and dropped the
   `ConnectionManager`; `is_current` upgrades a weak reference to the
   manager and refuses when it is gone, so every dial was
   `PolicySuperseded` and subject and control agreed *again* — this
   time with the instrument in the right place. Production's behaviour
   here is deliberate and pinned by
   `a_handle_that_outlives_its_manager_admits_nothing`. The node now
   holds its manager, and R6.7/R6.8 fail if it stops.
10. **The relay had no external address, so no circuit could ever
    complete.** A relay server builds a reservation's address list from
    its own `ExternalAddresses` (libp2p-relay 0.21.1
    `behaviour.rs:449`), and a loopback node that never calls
    `add_external_address` has none. The client accepted a reservation
    it could not use, closed its listener with
    `NoAddressesInReservation`, and the relay dropped the reservation
    with the connection — so a later CONNECT was answered
    `NO_RESERVATION` against a reservation the relay had genuinely
    accepted. Written up as an unexplained crate behaviour and
    deliberately not claimed as a finding, which is the only reason it
    did not become one. R5.7 and R5.12 now require the circuit to
    complete, and fail when the external address is removed.

## Reproducing

```
cd spikes/spike-004/harness
cargo run
```

Exits 0 only when every required observation held — **79 of them**, and
every finding above is carried by one rather than by a printed number.
That is a review finding on PR #69, raised four times over: F3, F4, F6
and F7 were each asserted in this file while the harness only noted the
value behind them, so a run contradicting one still exited 0. What each
claim owes now:

| Finding | What must hold |
| --- | --- |
| F1 attribution | R2.6–R2.8: zero unattributed, resolved AS `RelayReservation` |
| F1 why it is needed | R6.4–R6.8: the SHIPPED gate refuses a real relay client's reservation dial as `NotAuthorizedForDataPlane`, and admits it when the relay's trust class is the only thing changed |
| F8 the refusal is silent | R6.9 (no `Dialing`, no `OutgoingConnectionError`), R6.11 (the admitted dial IS reported — the positive control), R6.10 (the only trace is a listener closing successfully) |
| F2 dial-back crosses the gate | R4.6–R4.8: admitted as `AutonatProbe`, the probe completed, and an untrusting server's is REFUSED |
| F3 circuit is command-path | R5.6, with R5.8/R5.9 (the dial happened and was attributed) and R5.11 (the behaviour dialled only the relay). R5.7 and R5.12 establish that the circuit was ACCEPTED and ESTABLISHED, so this covers the path rather than only the dial that opens it |
| F2 where the check runs | R4.10: the candidate is at the pending hook, before any socket |
| F4 pending-hook address count | R2.9: exactly one, where Kademlia's is zero |
| F6 class split and precedence | R3.1/R3.2 by denial REASON, and R3.4 flips all four when the peer is in both sets |
| D1 DCUtR divergence | R3.5 pins today's behaviour, so a fix fails here rather than passing silently |
| D2 relayed-circuit divergence | R7.4 pins today's behaviour, R7.5 is the control (same destination, data-plane origin, refused), R7.2 shows the class check runs on the destination at all |
| D3 relayed pre-auth bucket | R9.3 pins today's behaviour, R9.2 is the control (direct inbounds from one IP — the second refused), R9.4 shows the global cap is what remains |
| §10's premise and its capability | R8.4 (no source IP on a relayed remote), R8.5 (the relay's PeerId IS available at the pending hook), R8.7 the control (a direct inbound does carry an IP), R8.8 (the two are distinguishable there) |
| ADR-0036's inbound relayed clause | R8.9/R8.10: the destination's established hook names the SOURCE's authenticated PeerId on a relayed local address, and never the relay's |
| F9 reservations and withdrawal | R10.2/R10.3 (two relays, each recording its own acceptance), R10.5 (both addresses advertised), R10.9 (the loss was observed) and R10.10 (withdrawn within a second of it), R10.7 with R10.8 as the control (the survivor stays) |
| F10 relay defaults vs §8 | R11.2/R11.3 (the two that break a deployment), R11.4 (the two that are looser), R11.1 and R11.5 record the rest |
| F11 per-peer off-by-one | R11.7 (a ceiling of one admits two) with R11.9 as the control (the third is refused) |
| F12 DCUtR bounds are an adapter's, not the gate's | R12.4 with R12.9 (announced == resolved for the origin on both nodes, zero unattributed — every punch dial, not merely one) and R12.5 (both ends dial and EXACTLY ONE reports the result, so a node's dial count tells it neither how many attempts are open nor whether one failed) |
| F13 §78's dedupe is ours | R12.7 (two `ConnectionEstablished` for one logical peer) with R12.8 and R12.10 (one relayed and one direct connection OPEN AT ONCE, by connection id and endpoint — not a peer present in a set) |
| ADR-0036's relayed end-PeerId clause | R7.12 (the path went through the relay), R7.9/R7.10 (two distinct authenticated identities), R7.11 (Identify completed with the destination through the circuit) |
| F3, again | R5.11 — no relay-behaviour dial targeted the destination, which origin counts alone cannot show |
| F7 advertised control protocols | R1.6/R1.7: Identify arrived and names both. **Scoped**: the harness has no data-plane behaviours, so it says nothing about isolation |

The claims that carry the mechanism are mutation-checked:

- deleting the `announce` call in `Attributing::poll` fails R2.6 and
  R2.7 (`unattributed: 1`);
- deleting the `DialFailure` cleanup fails R2.10 with `after 1`;
- R4.6/R4.7/R4.8 fail on a run where the probe reaches the wrong server,
  which is how the lucky-run bug above was caught;
- adding the infrastructure peer to the data-plane allowlist fails all
  four R3.2 refusals with `got None` — which is ADR-0036's precedence
  rule from the other side, and is now also asserted in its own right by
  R3.4 rather than existing only as a mutation someone ran by hand.

R6's four, run as a batch, each asserting the patch applied before
trusting the result:

| Mutation | Fails |
| --- | --- |
| subject's relay moved to the data-plane allowlist | R6.5, R6.8, R6.9, R6.10, R6.11 |
| control's relay moved to the infrastructure set | R6.7, R6.8, R6.11 |
| `ProductionNode` drops its `ConnectionManager` (bug 9, reintroduced) | R6.7, R6.8, R6.11 |
| the circuit listen removed, so nothing dials | R6.4, R6.5, R6.7, R6.8, R6.10, R6.11 |

The FIRST of those is the one worth reading: it left R6.6 passing,
because with no refusal at all `refusals().iter().all(..)` is vacuously
true. R6.6 now requires the list to be non-empty as well, so it stands
without leaning on R6.5.

R7's three:

| Mutation | Fails |
| --- | --- |
| the infrastructure-only destination added to the data-plane allowlist too | R7.5 |
| the source stops trusting the destination | R7.6, R7.8, R7.9, R7.10, R7.11, R7.12 |
| the circuit dial announced as `Manual` rather than `RelayCircuit` | R7.8 |

The SECOND is the one that matters: R7 is about an OUTBOUND circuit
dial, so removing the destination from the source's data-plane
allowlist refuses that dial outright and takes every relayed
observation with it. The relay's own authorization is untouched and
buys the source nothing.

R8's three and R9's three:

| Mutation | Fails |
| --- | --- |
| the destination never listens on the circuit | R8.3, R8.4, R8.5, R8.8, R8.9, R8.10 |
| the direct control node never dials | *nothing* — see below |
| the destination stops trusting the source | *nothing* — see below |
| the relayed remote address carries an IP after all | R9.3, R9.4 |
| per-source ceiling raised to two | R9.2 |
| global ceiling lowered to four | R9.4 |

**Two of R8's mutations changed nothing, and both are worth stating.**
The separate direct-control node is redundant: DCUtR upgrades the
relayed path on loopback, so a direct inbound from the *same* peer pair
arrives anyway — a better control than the one written, since only the
path differs. And the DESTINATION refusing to trust the SOURCE changes
nothing because **nothing gates inbound here**: the harness gate records
and the production gate is outbound-only. That is not in tension with
R7's mutation, which removes trust in the other direction and refuses an
outbound dial. R8 measures what an inbound decision could read, never a
decision being made.

R10's two and R11's one:

| Mutation | Fails |
| --- | --- |
| the lost relay is not actually dropped | R10.7 |
| the client never listens on the second relay's circuit | R10.2, R10.3, R10.5 |
| per-source circuit ceiling left at the crate default | R11.7, R11.9 |
| DCUtR disabled at the source | R12.4, R12.5, R12.7, R12.8 |
| DCUtR dials are never announced | R5.5, R12.4, R12.5, R12.7 |
| the lost relay is not actually dropped (again, against the tightened claims) | R10.7, R10.9, R10.10 |
