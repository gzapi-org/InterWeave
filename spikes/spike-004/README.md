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

## How the measurement is honest

The harness depends on `interweave-transport-libp2p`,
`-runtime`, `-api` and `interweave-trust-api` **by path**, and asks
`ConnectionManager::admit` through a real `SnapshotHandle`. Measuring a
copy of the gate would measure a copy. The dependency runs spike →
product, the direction CLAUDE.md §4 permits.

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
use.** Fails closed, silently, as an ordinary dial failure.

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

**F6 — the infrastructure/data-plane split holds against the real
policy, in both directions.** R3 asks the production
`ConnectionManager::admit` for one peer authorized ONLY as
infrastructure: `RelayReservation`, `RelayCircuit`, `AutonatProbe` and
`DcutrHolePunch` are admitted; `KademliaQuery`, `ConnectionManager`,
`Manual` and `DiscoveryReconnect` are refused, and refused specifically
as `NotAuthorizedForDataPlane`. (`DcutrHolePunch` is the exception, and
it is D1 above rather than part of this finding.) Adding the same peer to the data-plane
allowlist flips all four refusals to admissions, which is ADR-0036's
"data-plane trust wins" observed rather than restated.

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

## Stated limits

- **Loopback only.** No NAT of any kind. Every DCUtR observation here is
  about bounds and event shape, never about hole punching working.
- **No adversary.** No wire-protocol-violating peer, no relay that lies.
- **Server roles are single-instance.** ADR-0035 requires two
  independently operated relay/probe services; one process cannot show
  what redundancy buys.
- **No resource-cost measurement.** Relay bandwidth, connection and
  probe budgets are phase B.
- Reservation lifecycle, DCUtR bounds, relayed pre-auth accounting and
  relayed end-PeerId trust are **not yet covered** by this phase-A run;
  they are reachable on loopback and are the next experiments to add.
- **F3 is about the opening dial only.** No circuit completed, so
  whatever the relay behaviour may do after a relay ACCEPTS one is
  unobserved. R5.12 says so at the assertion itself.
- **A relayed data path does not complete here.** R5 obtains a
  reservation and the circuit is then refused `NO_RESERVATION` by a
  relay that had just accepted one. This is recorded as an open
  observation, not a finding: the run does not distinguish a fixture
  race from crate behaviour, and nothing in this record depends on it.

## Seven fixture bugs this run found in itself

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

## Reproducing

```
cd spikes/spike-004/harness
cargo run
```

Exits 0 only when every required observation held — **32 of them**, and
every finding above is carried by one rather than by a printed number.
That is a review finding on PR #69, raised four times over: F3, F4, F6
and F7 were each asserted in this file while the harness only noted the
value behind them, so a run contradicting one still exited 0. What each
claim owes now:

| Finding | What must hold |
| --- | --- |
| F1 attribution | R2.6–R2.8: zero unattributed, resolved AS `RelayReservation` |
| F2 dial-back crosses the gate | R4.6–R4.8: admitted as `AutonatProbe`, the probe completed, and an untrusting server's is REFUSED |
| F3 circuit is command-path | R5.6, with R5.8/R5.9 (the dial happened and was attributed) and R5.11 (the behaviour dialled only the relay). **Scoped**: R5.12 records that no circuit completed here, so this is evidence about the dial that OPENS a circuit, not the lifecycle |
| F2 where the check runs | R4.10: the candidate is at the pending hook, before any socket |
| F4 pending-hook address count | R2.9: exactly one, where Kademlia's is zero |
| F6 class split and precedence | R3.1/R3.2 by denial REASON, and R3.4 flips all four when the peer is in both sets |
| D1 DCUtR divergence | R3.5 pins today's behaviour, so a fix fails here rather than passing silently |
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
