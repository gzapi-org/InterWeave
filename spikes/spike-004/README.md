# SPIKE-004 — AutoNAT v2 / Relay v2 / DCUtR, phase A

AutoNAT v2, Circuit Relay v2 and DCUtR measured against the **production
root dial gate**, on the exact rust-libp2p the product pins.

Do not treat experiments placed here as production implementation.
Evidence and the final decision are recorded against
[`architecture/roadmap/SPIKES.md`](../../architecture/roadmap/SPIKES.md).

**One name below no longer exists, and is left as written on purpose.**
This record measures dated states of the production code, so it names
`DialOrigin::is_data_plane` throughout. Stage 11 step 2 renamed that
predicate `names_application_destination` on 2026-09-04, in the commit
that also moved `RelayCircuit` and `DcutrHolePunch` into it — the
rename and the fix are the same change, because the old name described
traffic while the rule decides ADR-0036's WITH/FOR question. Rewriting
the narrative to the new name would make phase A's measurements read as
though they were taken against code that did not exist yet; the dated
`RESOLVED` notes say what changed instead. **The same applies to the
harness sources**, which measure those same dated states: where a
comment there explains what phase A found, it keeps the old name.

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

`libp2p = "=0.56.0"` — exact, with `Cargo.lock` committed beside it.
The manifest's own feature array is `tcp`, `noise`, `yamux`,
`identify`, `tokio`, `macros`, `ed25519` plus the three spike-only
`autonat`, `relay`, `dcutr`; the production crate's remaining features
(`kad`, `request-response`, `gossipsub`) arrive through the path
dependency on `interweave-transport-libp2p`, which Cargo unions in. So
the RESOLVED graph is the production set plus three, and the array in
the file is not — a distinction worth stating here, directly above the
paragraph about feature unification, because a reader takes the array
at face value otherwise. Most of what is recorded below is the
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
is this harness's own, because at the time these experiments were run
production's `OutboundAdmission::handle_pending_outbound_connection`
hardcoded `DialOrigin::KademliaQuery` — which was the thing under test.
A spike measuring a change cannot use the code the change replaces.

> **Since 2026-09-03 that hook is no longer the one described here.**
> Stage 11 step 1 (PR #71) built the mechanism this harness proposed:
> the hook resolves an announced `ConnectionId -> DialOrigin` note and
> refuses a dial it has no note for. `production.rs` measures the
> shipped gate and was rewired accordingly; `InstrumentedGate` is kept
> as the record of what was proposed and measured.

An earlier version of this section said the harness ran "the production
root gate", full stop, while no source file referenced
`interweave-transport-libp2p` at all: the dependency was declared and
unused. A review caught it.

**`R6` closes that gap.** It runs the real `OutboundAdmission`,
unmodified and by path, in front of a real `relay::client::Behaviour`,
and measures what the shipped gate answers when the relay client asks
to dial its relay. At phase A's close, with the relay authorized as
infrastructure only, the gate refused — `kademlia dial refused:
NotAuthorizedForDataPlane`, for a dial no Kademlia made. Move that same
relay into the data-plane allowlist, change nothing else, and the
identical dial was admitted and connected. F1 is measured, not read.
Stage 11 step 1 has since fixed it, and R6 was rebuilt around the gate
that shipped — see F1's own note below.

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
a `ConnectionId`, an `Option<PeerId>` and an address slice whose
contents depend on the ORIGIN — empty for a Kademlia query, one
candidate for a relay reservation (R2.9) or an AutoNAT dial-back
(R4.10), and F2's pre-socket check needs that last one. **What none of
them carries is which behaviour asked**, so at phase A's close the hook
inferred `DialOrigin::KademliaQuery` because Kademlia was the only
behaviour that could dial. `KademliaQuery.is_data_plane()` is true, and
R3.2 shows a data-plane origin is refused for an infrastructure-only
peer — so without attribution **every relay reservation and every
AutoNAT probe would be refused against exactly the infrastructure the
stack exists to use.** R6 ran that: the shipped gate refused a real
relay client's reservation dial toward an infrastructure-only relay with
`kademlia dial refused: NotAuthorizedForDataPlane`, and admitted the
same dial when the only change was the relay's trust class.

> **FIXED 2026-09-03 by Stage 11 step 1, and R6 now measures the fix.**
> The pending hook resolves an announced `ConnectionId -> DialOrigin`
> note instead of assuming one, and refuses a dial it has no note for.
> A reservation dial announced as `RelayReservation` is not data-plane,
> so it is ADMITTED (R6.5) and the node reaches the relay (R6.8). The
> class check still runs: the same dial toward the same relay announced
> under a data-plane origin is refused `KademliaQuery dial refused:
> NotAuthorizedForDataPlane` (R6.6), which is what stops R6.5 reading as
> "the gate stopped looking". **F1 is therefore history — the record of
> why attribution was required and of what happened without it — rather
> than a live description of the gate.**

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
mechanism can see one. The evidence is R5.6 — no dial resolved as
`relay-circuit` at all — together with R5.11, which shows that no dial
the relay BEHAVIOUR made was aimed at the destination. (R5.2's `manual`
count is not itself evidence: the harness announced its own dial that
way. What R5.11 rules out is the classifier having relabelled a real
behaviour-originated circuit dial as a reservation, which origin counts
alone cannot show.)
The conclusion for Stage 11: **`RelayCircuit` comes from the CALLER**,
because the caller dialling through a relay is the party that knows.
*(Correction, 2026-09-04: as written this named the wrong function.
In shipped code `attempt_dial` carries the origin the caller supplied
into admission, and `AdmittedDial::from_ticket` — reached from
`attempt_dial`, before the Swarm is touched — ENFORCES the pairing
against the address, refusing a `/p2p-circuit` address not admitted as
`RelayCircuit` and the reverse. `GatedSwarm::dial` does neither: it
destructures an already-built `AdmittedDial`, registers the id and
forwards to the inner Swarm, with no address inspection and no refusal
path. Second correction, 2026-09-04: the first one named the wrong
function too.)* The classifier still earns its place for
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

> **RESOLVED 2026-09-04, Stage 11 step 2.** The origin is named by the
> admission predicate — renamed `names_application_destination` in the
> same change — so a hole punch terminating at an infrastructure-only
> peer is refused. R3.5 asserted the admission in the defect's shape so
> that a fix would fail there rather than pass silently, and it did:
> R3.5 was one of three observations that failed on the fixing commit.
> It now asserts the refusal, and asserts it carries
> `NotAuthorizedForDataPlane` rather than merely that something was
> refused. The record below is left as written; this note is the
> correction.

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

> **CORRECTED 2026-09-03 — the two fixes are not in tension, and this
> paragraph read them as if they were.** The predicate has two
> production consumers and both key on the class the same way:
> `ConnectionPolicy::admit` (`connection_policy.rs`, the `match class`
> in `admit`) and
> `ConnectionManager::authorizes_for` (the `match class` in that
> function) each consult `is_data_plane` in ONE arm,
> `ConnectivityInfrastructureOnly`, and let `DataPlaneTrusted` through
> with no origin check at all. The class they switch on is the class of
> the peer the dial NAMES, and a hole punch toward a trusted peer
> through a relay names the trusted peer. (`authorizes_for` exists to
> AGREE with admission — its own note says fixing the classification
> there fixes it here — so the two cannot drift apart.) So adding the origin to
> `is_data_plane` cannot refuse that punch — it *is* the
> destination-class check this paragraph reaches for, expressed where
> the policy already keys on the destination. What Stage 11 still
> decides is the fix; what it no longer has to weigh is an objection
> the shipped code does not support.
>
> **What is READ rather than measured**: that a hole punch names the far
> end as its dial peer. R12.4 measured that every punch dial reaches the
> gate attributed under `DcutrHolePunch` — on both nodes, none
> unattributed — but did not assert which PeerId that dial carries, and
> on loopback both ends were trusted, so the infrastructure-only
> destination case is R3.5's synthetic policy call rather than a real
> punch. The half of this correction about the shipped policy is pinned
> by `a_trusted_destination_is_admitted_under_every_origin`; the half
> about the crate is pinned by nothing, and Stage 11 should confirm the
> dial's peer when the behaviour is enabled.
>
> The paragraph is left as written
> because a spike record is read for what it found.

This was found because an earlier version of R3 *required* the
admission — recording the violation as evidence that the split held.

**D2 — `RelayCircuit` is admitted for an infrastructure-only
DESTINATION, and a circuit is application traffic by construction.**

> **RESOLVED 2026-09-04, Stage 11 step 2**, together with D1 and by the
> same two-word change. `RelayReservation` did NOT move, for the reason
> this section argues below. R3.6 and R7.4 now assert the refusal;
> R7.4's control changed meaning with it, since both origins are
> refused now and R7.2 carries the discrimination — a stranger is
> refused `Unauthorized` rather than for the data plane. The record
> below is left as written; this note is the correction.

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

**But D2 was a document CONFLICT before it was a code defect, and Stage
11 had to resolve it in that order.** A review on PR #69 was right to
hold this to CLAUDE.md §2. ADR-0036's enforcement clause forbids the
admission; at the time this was written, two accepted rows appeared to
permit it:

- `transport/libp2p/CONNECTIVITY.md` §4's protocol matrix read
  *"Relay v2 control | eligible | eligible"*, and ADR-0036's own matrix
  said the same for reservation/circuit control;
- §11 said infrastructure-only PeerIds are dialable "only for permitted
  connectivity origins" and then named exactly two that never use the
  infrastructure set — `direct-user-command` and `kademlia-query`.
  `relay-circuit` was in the origin list and among neither.

The spike's reading was that a circuit *toward* a destination is not
"relay control *with*" that peer, and that the matrix already drew
exactly this distinction for DCUtR — *"DCUtR as destination peer |
no"* — which is why D1 had no such ambiguity and D2 did. The matrix
simply had no row for a circuit whose DESTINATION is the
infrastructure-only peer.

That reading was a recommendation, not a verdict. The architecture was
to be amended first — a row for the destination case — and the code to
follow it; changing `is_data_plane` against a contract that arguably
permitted the behaviour is precisely what §2 forbids.

> **RESOLVED 2026-09-03 — the spike's reading was adopted.** ADR-0036's
> Amendment 2026-09-03 added the row the matrix lacked, `| Relay v2
> circuit with that peer as application destination | yes | **no** |`,
> and `transport/libp2p/CONNECTIVITY.md` §4 and §11 inherited it. **The
> conflict is gone and D2 became an ordinary code fix**, made on
> 2026-09-04 — do not stop for the architecture decision, it has been
> taken, and do not re-make the code change either. The fix stays
> per-origin, as this section argues above: `RelayCircuit` moves,
> `RelayReservation` does not. The finding above is left as it was
> measured, because what a spike recorded is the thing a later reader
> comes here for.

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
lands. **The same is true of D1 and D2**, and an earlier version of this
paragraph implied otherwise: `grep` over `crates/` shows
`DialOrigin::DcutrHolePunch` and `DialOrigin::RelayCircuit` appear only
in the enum definition and in `#[cfg(test)]` blocks, so nothing in
production constructs either. All three are latent to exactly the same
degree, and all three are recorded against the code rather than against
the stage because that is where the fix belongs.

**And the comment above `source_label` argues the opposite**, naming the
relayed case and calling it "the fail-closed direction … it cannot merge
two peers into one bucket, only fail to merge two addresses that belong
together." That reasoning is right for the memory transport and wrong
here: §10's risk is not merging, it is proliferation. An unenforced
claim about a case nobody had run — which is the shape this repository's
own rule about comments is written against.

> **RESOLVED 2026-09-04, Stage 11 step 2.** `source_label` reads the
> REMOTE address for an IP first, and falls back to the relay — by
> PeerId where the local address carries one, else by the relay's IP —
> whenever that local address holds `/p2p-circuit`. So a relayed
> inbound is charged to the relay, which is what §10 asks for, and the
> source PeerId the circuit asserts no longer names a bucket. The
> `relay:` prefix on those buckets is a namespace this fix introduces:
> it keeps a relay's circuits from colliding with a direct inbound from
> the relay's own IP. R9.3 and R9.4 were flipped to assert the fix and
> failed on the commit that made it — R9.4 is the number worth
> reading, because 32 minted identities over one relay bought 8
> admissions under the global cap before and buy 1 now. The comment
> above `source_label` was rewritten with the tests it lacked.

**F6 — the infrastructure/data-plane split holds against the real
policy, in both directions.** R3 asks the production
`ConnectionManager::admit` for one peer authorized ONLY as
infrastructure. **Updated 2026-09-04**, because step 2 moved two
origins across the line and R3 measures the gate rather than describing
it: `RelayReservation` and `AutonatProbe` are admitted (R3.1) — the two
the matrix's control rows name — and the other SIX are refused, and
refused specifically as `NotAuthorizedForDataPlane`. Four of the six
are R3.2 (`KademliaQuery`, `ConnectionManager`, `Manual`,
`DiscoveryReconnect`); `RelayCircuit` is R3.6 and `DcutrHolePunch` is
R3.5, held separately because each began as a divergence pinned in the
defect's own shape and became a regression guard when step 2 flipped
it. Phase A measured four admitted and four refused; that four/four
split WAS D1 and D2. Adding the same peer to the data-plane allowlist
flips the refusals to admissions, which is ADR-0036's "data-plane trust
wins" observed rather than restated.

**Two of phase A's four admissions were the divergences, not the
finding.** `RelayReservation` and `AutonatProbe` are what the split
exists to permit, and they are the two that remain. `DcutrHolePunch`
was D1 and `RelayCircuit` was D2, and what F6 established is that the
MECHANISM works — the policy reads the destination's class and the
origin's purpose and combines them — which is why the two wrong
answers were a question of which origins sat on which side rather than
of whether the split is enforced. Step 2 moved them; the mechanism
F6 measured is what carried the fix without further change.

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
data-plane protocol, which is the invariant
`transport/libp2p/CONNECTIVITY.md` §4's protocol matrix actually
states.

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
  `OutboundAdmission` writes about *why* — `KademliaQuery dial refused:
  NotAuthorizedForDataPlane` — lives in `Error::source`, so a refusal
  logged the obvious way says nothing. R6 walks the chain deliberately;
  that it must is part of this finding.

**What binds Stage 11**: attribution (F1) removes the wrong refusals,
but a right refusal is just as silent. The stage owes an explicit
record at the gate — the pending hook already has the
`ConnectionId`, the peer and the verdict — because neither the Swarm
event stream nor the rendered error carries it.

**F9 — §8's reservation targets are expressible, and an address dies
with its relay.** `transport/libp2p/CONNECTIVITY.md` §8 keys reservation
targets to reachability state — two while Unknown or NotVerified, one when
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

So `max_circuits_per_source_peer: 4` from §8 yields five. Phase 4 must
either subtract one when configuring or amend §8 to say what the numbers
mean; copying the table across is wrong in every row that has a per-peer
form.

**MEASURED for circuits, READ for reservations.** `behaviour.rs:412`
guards `max_reservations_per_peer` with the same `>`, so
`max_reservations_per_peer: 1` should likewise yield two — but this run
does not show it. A client's relay behaviour reuses its existing
connection for a second reservation, and the relay keys reservations by
`ConnectionId` in a `HashSet`, so a second reservation on one connection
does not grow the count. Reaching it needs two separate connections from
one peer, which the fixture does not build. Stated here rather than
asserted, on the same rule that narrowed F7 and F12.

The dials had to be issued one at a time for this to be measurable: the
relay counts circuits it has ACCEPTED, so two requests in flight
together are each counted against nothing, and a first attempt saw one
circuit accepted and concluded the ceiling held.

**F12 — DCUtR has no bounds of its own, and one attempt is not one
dial.** `dcutr::Behaviour::new` takes a `PeerId` and nothing else. The
crate's own ceiling, `MAX_NUMBER_OF_UPGRADE_ATTEMPTS = 3`, is a
`pub(crate)` constant counting retries per relayed connection — neither
a concurrency cap nor a cooldown. So `transport/libp2p/CONNECTIVITY.md` §13's "at most four concurrent, one
per peer, five-minute failure cooldown" has to be enforced outside the
behaviour.

**Not by the gate alone, though.** The gate is the only place that sees
every dial, and R12.4 confirms every hole-punch dial arrives there
attributed — but it is handed no logical attempt identifier and no
outcome. R12.5 measures the split the conclusion rests on: **one hole
punch produces a dial at BOTH ends**, so no single node's gate ever sees
the attempt, only its own half. A per-peer "one hole punch" ceiling
counted at one gate is counting half of something.

**A claim that was here and is now gone, because a run disproved it.**
An earlier version of R12.5 required that EXACTLY ONE end report the
result, having seen that in several runs — and F12 rested on it: the
node that dialled would never learn how its attempt ended. Run the
harness enough times and both ends report. The requirement was a shape
three runs happened to have, which is the same error as measuring a
fixture, and it was caught by the harness failing rather than by anyone
reading it. The per-node result counts are R12.13, a note. What survives
is the weaker and true statement: whether a node is told its own
attempt's outcome is not something this harness can pin, and the outcome
reaches the DCUtR behaviour rather than the gate either way.

**Candidate multiplicity is not measured** either: each endpoint dialled
once, because loopback offers one candidate address. That the crate
dials every observed candidate is read from its source, so "a per-peer
gate would refuse its own attempt's siblings" is not carried forward as
evidence. The attempt lifecycle belongs to a
DCUtR adapter, with a token for the attempt reaching the gate, which
admits or refuses it as a unit.

R12.4 says EVERY rather than at least one: the
announced and resolved counts for the origin agree on both nodes, with
zero dials of any origin meeting a gate with no note. `> 0` would have
been satisfied by one admitted dial per node while the rest bypassed the
gate — which is precisely the regression F12's conclusion depends on not
happening, and a review on PR #69 said so.
**But the gate cannot read "one attempt" off its own dial count.** Both
ends dial for a single punch — source one, destination one — so a
per-peer rule counting dials at either gate counts half of an attempt
that spans two nodes.

**F13 — a successful punch is a second connection to a peer that is
already connected.** `contracts/CONNECTIVITY.md` §5 says a successful hole punch
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
- **Three of SPIKE-004's own expected-evidence items in `SPIKES.md` are
  neither answered nor previously disclaimed, and all three are blocked
  on components this stage has yet to build rather than on the
  environment.** *Multi-observer aggregation to
  `unknown`/`verified_public`/`not_verified`* — the crate's per-probe
  results are observed, but the aggregation over two servers is our
  reachability state machine and does not exist. *Identify-learned
  infrastructure candidates disabled by default, and never displacing
  usable static ones* — that promotion policy is ours and does not
  exist. *Relay service admission accepting only configured
  `DataPlaneTrusted`/`ConnectivityInfrastructureOnly` classes* — every
  relay server here is built with empty trust sets and accepts a
  reservation from anyone, because `relay::Behaviour` has no admission
  hook; the class check is a wrapper Phase 4 writes. **R3, R6 and R7
  measure the CLIENT's dial gate, never a relay's own service
  admission**, and nothing in this record should be read as validating
  the latter.
- **Three items of `transport/libp2p/CONNECTIVITY.md` §26's gate are untouched and were
  not previously listed here.** Direct-versus-relay racing and
  cancellation semantics are never exercised; network-change behaviour
  is not simulated; and `direct`/GossipSub over a relayed connection is
  unreachable from this harness, because `SpikeBehaviour` carries no
  data-plane behaviour (see F7). The last of those is Phase 2's own
  work rather than an environment limit. §26 now carries the same
  item-by-item split.
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
   holds its manager, and the 2026-09-04 re-measurement of this
   mutation fails R6.5, R6.6, R6.7, R6.8 and R6.11 -- see the table
   below, which is the measured list. R6.7/R6.8 was the pair recorded
   when the bug was first found, against the pre-step-1 wiring.
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

## What CI does and does not run

**Nothing here runs in CI, and the phrase "pinned so a fix fails here"
has to be read against that.** The harness is its own workspace root —
deliberately, because Cargo unifies features across one workspace and
membership would switch `autonat`, `relay` and `dcutr` on inside
`interweave-transport-libp2p`. The cost is that `cargo xtask ci`, the
`rust` job and `cargo deny` never touch this directory: nothing re-runs
`cargo run`, and nothing even proves these files still compile against
the production crates they path-depend on. A refactor of
`OutboundAdmission::new` or `PreAuthLimitsBuilder` breaks the harness
with no signal at all.

**That happened, and was repaired on 2026-09-03.**
`OutboundAdmission::new` gained a `DialAttribution` parameter in Stage
11 step 1 and `harness/src/production.rs` went on passing three
arguments, so the harness stopped compiling against `main`
(`error[E0061]`) and nothing reported it — exactly as this paragraph
predicted. The repair was not the missing argument: step 1 also made an
unattributed dial REFUSED, so the module now wires the relay client
through `Attributing` as production does, and R6 was rebuilt around the
gate step 1 shipped. A run is green again — 86 required observations, 0
failed, 3 divergences. **The gap this paragraph describes has not
closed**: nothing in CI would catch the next such break either.

So where a divergence has to fail something CI runs, the pin lives in
production:

- **D1 and D2** are pinned by `connection_policy.rs`'s
  `every_origin_is_classified_and_the_classification_is_pinned`, which
  matches `DialOrigin::ALL` exhaustively and hardcodes the expected side
  per origin. It pinned the divergence while the divergence stood, and
  step 2 flipped its two arms rather than deleting it — so **it now
  fails if either origin is taken back OUT of
  `names_application_destination`**, which is the direction that
  matters from here.
- **D3** had no counterpart, so the PR that RECORDED it added four
  tests beside `source_label` in `preauth_gate.rs` — that file had no
  test module at all before, which is why the comment above
  `source_label` could argue the relayed case was fail-closed for as
  long as it did. Step 2 flipped two of them (the D3 pin is now
  `two_relayed_sources_over_one_relay_share_one_bucket`, which asserted
  the opposite while the defect stood) and PR #74's review added three
  more, for six: the circuit branch's third case, the hook's argument
  order, and the `relay:` prefix's no-collision claim.

Everything else here is evidence, and evidence is re-run by hand.

**And it has to be deterministic to be evidence at all.** Twice now a
required observation has failed on roughly one run in ten while passing
on the others, and both times the flake was the finding rather than the
fixture: the first was R12.5 asserting that exactly one end reports a
hole punch's result, which several runs had shown and a later one
disproved. The second was never captured, and rather than keep sampling
for it the experiments that assert an OUTCOME now wait for that outcome
instead of for a fixed time budget — R5 and R7 for a circuit accepted
and established, R11 for the relay to answer each circuit request
before the next is made, R12 for a hole-punch result with both
connections open. A `pump` of *n* seconds followed by an assertion is a
statement about this machine's load; `pump_until` the thing you are
about to assert is a statement about the crate.

**The harness's dependency graph is also outside `cargo deny`.** Its
3,790-line lock — `libp2p-autonat`, `libp2p-relay`, `libp2p-dcutr` and
their transitive dependencies — is not covered by the licence or
advisory checks, which run at the repository root. Inherited from
spike-003's layout rather than new here, but the exemption is otherwise
invisible.

## Reproducing

```
cd spikes/spike-004/harness
cargo run
```

Exits 0 only when every required observation held — **86 of them**, and
every finding above is carried by one rather than by a printed number.
That is a review finding on PR #69, raised four times over: F3, F4, F6
and F7 were each asserted in this file while the harness only noted the
value behind them, so a run contradicting one still exited 0. What each
claim owes now:

Every row names a `require`, which is what can fail. Where a note is
mentioned it is labelled as one — a note is printed, never checked.

| Finding | What must hold |
| --- | --- |
| F1 attribution | R2.6–R2.8: zero unattributed, resolved AS `RelayReservation` |
| F1 why it is needed | R6.4–R6.8. As measured at phase A close: the shipped gate refused a real relay client's reservation dial as `NotAuthorizedForDataPlane`. Since step 1 fixed it, R6.5 asserts the dial is ADMITTED under its own origin, R6.6 that a data-plane origin toward the same relay is still refused, and R6.8 that the attributed node reaches the relay where a misattributed one does not |
| F8 the refusal is silent | R6.9 (no `Dialing`, no `OutgoingConnectionError`), R6.11 (the admitted dial IS reported — the positive control), R6.10 (the only trace is a listener closing successfully) |
| F2 dial-back crosses the gate | R4.6–R4.8: admitted as `AutonatProbe`, the probe completed, and an untrusting server's is REFUSED *on trust* (R4.8 checks the denial reason, since an empty allow-list also holds for an unattributed or nameless dial) |
| F2 the check is MISSING | R4.12: the candidate the server dialled back to is a LOOPBACK address, which §7 requires be rejected even from an authorized peer. This is the finding measured rather than read — R4.1 is a note, and a note cannot fail. **Scoped**: only §7's special-use rule is reached; the source-equality and unrelated-public-IP cases need a second interface and are phase B |
| F3 circuit is command-path | R5.6, with R5.8/R5.9 (the dial happened and was attributed) and R5.11 (the behaviour dialled only the relay). R5.7 and R5.12 establish that the circuit was ACCEPTED and ESTABLISHED, so this covers the path rather than only the dial that opens it |
| F2 where the check runs | R4.10: the candidate is at the pending hook, before any socket |
| F4 pending-hook address count | R2.9: exactly one, where Kademlia's is zero |
| F6 class split and precedence | R3.1/R3.2 by denial REASON, and R3.4 flips all four when the peer is in both sets |
| D1 DCUtR divergence, FIXED 2026-09-04 | R3.5 pinned the admission so a fix would fail here rather than pass silently — it did, and now asserts the refusal by denial REASON |
| D2, FIXED 2026-09-04 | R3.6 — the same admission R3.1 used to list under "the matrix permits", split out so that fixing D2 does not fail an experiment claiming the matrix allows it |
| F5 default policy refuses everything | R3.7: `ConnectionPolicy::default()` refuses a fully TRUSTED peer with `ConnectionLimitReached`, so a fixture taking the default measures nothing about class |
| D2 relayed-circuit divergence, FIXED 2026-09-04 | R7.4 pinned the admission and now asserts the refusal by denial REASON; R7.2 carries the discrimination the control used to (a stranger is refused `Unauthorized`, not for the data plane), so R7.5 is now a regression guard rather than a contrast |
| D3 relayed pre-auth bucket, FIXED 2026-09-04 | R9.3 pinned the source-named bucket and now asserts the relay-named one; R9.2 is the control (direct inbounds from one IP — the second refused); R9.4 was "the global cap is all that remains" and now asserts the PER-SOURCE cap bounds it, at 1 admission where 32 identities used to buy 8; R9.6 requires the two sources to differ only in identity — the fixture being R8's MEASURED address with one substitution, which R9.5 records as a note |
| §10's premise and its capability | R8.4 (no source IP on a relayed remote), R8.11 (the remote IS the source's PeerId — D3's whole force), R8.5 (the relay's PeerId IS available at the pending hook), R8.7 the control (a direct inbound does carry an IP), R8.8 (the two are distinguishable there) |
| ADR-0036's inbound relayed clause | R8.9/R8.10: the destination's established hook names the SOURCE's authenticated PeerId on a relayed local address, and never the relay's |
| F9 reservations and withdrawal | R10.2/R10.3 (two relays, each recording its own acceptance), R10.5 (both addresses advertised), R10.9 (the loss was observed) and R10.10 (withdrawn within a second of it), R10.7 with R10.8 as the control (the survivor stays) |
| F10 relay defaults vs §8 | R11.2/R11.3 (the two that break a deployment), R11.4 (the two that are looser), R11.1 and R11.5 record the rest |
| F11 per-peer off-by-one | R11.7 (a ceiling of one admits two) with R11.9 as the control (the third is refused) |
| F12 DCUtR bounds are an adapter's, not the gate's | R12.4 (announced == resolved for the origin on both nodes, zero unattributed — every punch dial, not merely one) and R12.5 (one punch, a dial at BOTH ends, so no single gate sees the attempt). R12.9 and R12.13 are notes recording the counts behind them |
| F13 §5's dedupe is ours | R12.7 (two `ConnectionEstablished` for one logical peer) with R12.8 (one relayed and one direct connection OPEN AT ONCE, by connection id and endpoint — not a peer present in a set; R12.10 is the note recording the pair) |
| ADR-0036's relayed end-PeerId clause | R7.12 (the path went through the relay), R7.9/R7.10 (two distinct authenticated identities), R7.11 (Identify completed with the destination through the circuit) |
| F3, again | R5.11 — no relay-behaviour dial targeted the destination, which origin counts alone cannot show |
| F7 advertised control protocols | R1.6/R1.7 (Identify arrived and names all four) with R1.8 (and NOTHING else — the list is exact, since it is what a restriction must leave intact). **Scoped**: the harness has no data-plane behaviours, so it says nothing about isolation |

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

R6's mutations, run as a batch, each asserting the patch applied before
trusting the result. **Re-measured 2026-09-04**, after Stage 11 step 1
inverted R6.5 from a refusal to an admission and added the
`misattributed` node — the previous table was derived against the
pre-step-1 wiring and two of its rows had silently become empty claims.
Baseline for this batch: 86 required, 0 failed, exit 0, confirmed stable
over two consecutive clean runs.

| Mutation | Fails |
| --- | --- |
| subject's relay moved to the data-plane allowlist | **nothing** — exit 0 |
| control's relay moved to the infrastructure set | **nothing** — exit 0 |
| `misattributed` announces `RelayReservation` instead of a data-plane origin | R6.6, R6.8, R6.9, R6.10, R6.11 |
| `ProductionNode` drops its `ConnectionManager` (bug 9, reintroduced) | R6.5, R6.6, R6.7, R6.8, R6.11 |
| the circuit listen removed, so nothing dials | R6.4, R6.5, R6.6, R6.7, R6.8, R6.10, R6.11 |

**The first two rows are the result worth reading, and they are
negative.** Since step 1 the subject's dial is admitted, so making the
subject into the control changes no verdict; and the control's dial is
admitted whichever set its relay is in, because `RelayReservation` does
not name an application destination and `admit` reads
`names_application_destination` only in the
`ConnectivityInfrastructureOnly` arm. Neither mutation can fail
anything. The discriminating variable moved to `misattributed` — row
three is the one that now carries what row one used to.

**Row two said `no R6 row (R4.7 only)` until 2026-09-04, and that was a
flake read as a measurement.** R4.7 could not have been the mutation's
doing: `main` runs R4 before R6, and R6's control does not exist while
R4 is being measured. It was R4's own missing pump predicate, which
made it fail about one run in three; the root cause is fixed and the
reasoning is written up at its site in `experiments.rs`. Re-measured
after that fix, with the baseline above and the patch asserted to have
applied, the mutation fails nothing and the harness exits 0 — so row
two now says what row one says, for the same reason.

That is this record's own bug 8 arriving a second time by a different
route: an experiment whose control agrees with its subject has measured
neither. The first time it was written that way; this time a code change
made it so, and the table went on asserting the old result. A mutation
table is evidence with a shelf life — it expires when the experiment is
rewired, and nothing fails when it does.

Historical note on the old first row. It was the row worth reading
then, and the mechanism was vacuous truth: moving the subject's relay
into the data-plane allowlist got its dial ADMITTED, so there were no
refusals at all and `refusals().iter().all(..)` held over an empty
list. R6.6 now requires the list to be non-empty as well, so it stands
without leaning on R6.5.

A second change to R6.6 in the same period is easy to mistake for that
one, and it is not another explanation of it. R6.6's needle used to be
`kademlia`, because `OutboundAdmission` rendered every denial as
`kademlia dial refused: {denial:?}`; step 1 replaced that rendering
with `{origin:?}`, which is why the needle is `KademliaQuery` today.
That governs what R6.6 MATCHES when there is a refusal to match, and
says nothing about why the mutation left it green — an empty list
passes any needle.

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

R10's, R11's and R12's:

| Mutation | Fails |
| --- | --- |
| the lost relay is not actually dropped | R10.7 |
| the client never listens on the second relay's circuit | R10.2, R10.3, R10.5 |
| per-source circuit ceiling left at the crate default | R11.7, R11.9 |
| DCUtR disabled at the source | R12.4, R12.5, R12.7, R12.8 |
| the permissive AutoNAT server trusts nobody | R4.6, R4.7 |
| R3.7 built with a non-default policy | R3.7 |
| DCUtR dials are never announced | R5.5, R12.4, R12.5, R12.7 |
| the lost relay is not actually dropped (again, against the tightened claims) | R10.7, R10.9, R10.10 |
