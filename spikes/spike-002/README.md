# SPIKE-002 — transport wire/race and GossipSub cache behavior

**Status: PASS**, with four findings that change how Stage 6 must be written and one that changes how it must be tested.

rust-libp2p direct request-response and concurrency/validation behavior. Authoritative objective, evidence requirements, and decision gate live in [`architecture/roadmap/SPIKES.md`](../../architecture/roadmap/SPIKES.md); this file records what was actually observed.

Do not treat the experiment in [`harness/`](./harness) as production implementation. It is deliberately outside the workspace — an empty `[workspace]` table in its manifest — so it cannot be built by `cargo xtask ci`, cannot enter `Cargo.lock`, and cannot become a production dependency by a stray `path =`.

**That isolation carries more weight here than it did for SPIKE-006.** This harness enables `request-response` and `gossipsub` on the same `libp2p` dependency the substrate uses, and Cargo unifies features across one build of one workspace. As a member it would have switched both protocols on inside `interweave-transport-libp2p` — undoing CLAUDE.md §3's "absent from the feature list rather than merely unused", and doing it with nothing in the production crate having changed.

## What was pinned

```text
libp2p =0.56.0   features: tcp, noise, yamux, identify, tokio, macros,
                           ed25519, request-response, gossipsub, cbor
tokio  =1.53.1   futures =0.3.34   serde =1.0.229
```

**Every version is the one the root `Cargo.lock` resolves**, not merely a pinned one. The async graph underneath is part of what A5 and A8 measure — timeout attribution is a race between two executors, and cancellation is a stream-teardown ordering — so a spike on a different `tokio` or `futures` measures a different scheduler than the one that ships. The spike-only additions are libp2p *features*, never different versions.

The version the substrate ships, pinned exactly and with `Cargo.lock`
committed beside it. A floating `"0.56"` resolves a later patch release,
and the two findings below — duplicate-cache ordering and timeout
attribution — are precisely the kind of behaviour a patch release may
change. A result that cannot be rebuilt is not evidence, so this spike's
lock is the one exception to `spikes/**/Cargo.lock` being ignored. A spike measuring a version the product does not use measures nothing — SPIKE-006 learned that the expensive way, having first been run against a `libp2p-identity` the substrate could not have linked.

Protocol names are `/spike-002/…`, never `/interweave/…`: a spike that speaks the production protocol name is one `git mv` away from being mistaken for it.

## Run it

```sh
cd spikes/spike-002/harness && cargo run
```

Every experiment prints what it observed. The output below is a real run.

## A — request-response

### A1, A3. The shape works, and two families share one connection

Explicit destination and omitted destination both survive the round trip, and the two protocol families (`/spike-002/direct/2.0.0`, `/spike-002/endpoints/1.0.0`) are answered over **one** connection:

```text
distinct connections the two families arrived on   1
established connections on the responder           1
```

The first number is the load-bearing one. An earlier version of this experiment reported `network_info().num_peers()`, which is `1` whether one connection carried both families or each opened its own — so it would have said "one connection" in exactly the case it existed to detect. The connection IDs the requests actually arrived on are recorded now, and the established-connection counter corroborates them.

Stage 6 does not need a connection per protocol family.

### A2. An unsupported major is reported cleanly

A requester speaking `/spike-002/direct/2.0.0` to a responder that speaks only `/spike-002/direct/3.0.0` receives:

```text
OutboundFailure::UnsupportedProtocols
```

Distinct from every transport failure, and available before any application timeout. Stage 6 can map a major-version mismatch to a specific local diagnostic rather than inferring it from silence.

### A4. AcceptedV2 can be withheld without blocking the Swarm — the finding Stage 6 was waiting for

The responder took a `ResponseChannel` and did **not** answer. While that channel was held:

- a second peer's request was accepted and answered normally;
- the held response, sent later, still arrived at the original requester.

This is the clearance `FINAL-REVIEW.md` asked for: `AcceptedV2` may be withheld until bounded local endpoint-queue admission completes, and doing so does not stall the Swarm for anyone else.

### A5. Timeout attribution is a RACE, and the channel outlives its usefulness

Two runs of the same experiment produced two different answers on the requester:

| run | requester saw | responder saw |
|---|---|---|
| 1 | `Io(Eof { name: "enum", expect: Small(1) })` | `Timeout` |
| 2 | `Timeout` | `Timeout` |

Both sides run the same `request_timeout`, so whichever fires first decides what the other is told: if the responder's inbound timeout closes the substream first, the requester reads an **I/O error rather than a timeout**.

> **Stage 6 must not branch on `OutboundFailure::Timeout` to mean "the peer did not answer in time."** A timeout on the far side surfaces locally as `Io`. Either set the local request timeout meaningfully below the peer's, or treat `Io` and `Timeout` as one class at the direct-v2 boundary.

Second half of the same finding: after the timeout the responder still **holds** a `ResponseChannel`, and `channel.is_open()` is `false`. A late `send_response` has nowhere to go. A daemon that withholds `AcceptedV2` across an await (A4) must expect the channel to have died meanwhile, and must not treat "we produced a response" as "the peer heard it".

### A6. The concurrency claim survives the real scheduler

`DIRECT.md`: *"Matching concurrent duplicates attach as waiters and receive the same eventual response."* Both halves of that sentence matter, and the first version of this experiment tested only the first — see the note at the end of this section.

Twenty-four copies of one message — same `message_id`, same body, same destination selector — sent back to back and admitted through the **production** `ReservationMap`. The owner parks its response channel and its endpoint-queue admission completes **asynchronously**, so every other copy arrives while the outcome is still unknown:

```text
owner outcome: AcceptedV2 { resolved_endpoint: "chat" }
  copies sent                                    24
  responses received                             24
  local enqueues (owners)                        1
  waiters attached to the owner                  23
  every response is the owner's outcome          true
  reservations still held                        0

owner outcome: Rejected { reason: NoRoute }
  copies sent                                    24
  responses received                             24
  local enqueues (owners)                        1
  waiters attached to the owner                  23
  every response is the owner's outcome          true
  reservations still held                        0
```

**One enqueue, and twenty-three channels held open until it resolved.** The rejection half is not decoration: a waiter that manufactured its own `AcceptedV2` would be indistinguishable from a correct implementation on the happy path, and would fabricate twenty-three deliveries on this one.

> **The first version of this experiment did not prove this.** It admitted all twenty-four copies in one synchronous pass over one mutable state, and each waiter immediately produced its own `AcceptedV2` before the next Swarm event was polled; the owner's reservation was never completed or released. That demonstrated only that a retained map entry returns `Waiter` — a fact about a `BTreeMap` — and nothing about waiters sharing an asynchronously produced result. It was caught in review, and it is recorded here because the corrected experiment is the one the PASS rests on.

### A7. The reservation map stays bounded under overflow

Sixteen distinct in-flight keys from one peer against a per-peer budget of four:

```text
admitted (owners)                              4
refused as overloaded                          12
peers told `overloaded` on the wire            12
reservations held                              4
```

The map holds exactly its budget and the excess is refused. **Every refusal here is the PER-PEER check** — the per-peer budget is smaller than the global one, so the global bound is never touched. That is A10's job, below, and the distinction is not academic: broken global accounting produces this experiment's 4/12 exactly. `DIRECT.md` lists `overloaded` as a **distinct** coarse reason from `no_route`, and the two mean different things to whoever receives them: one says the route is fine and to retry later, the other says there is nowhere to deliver. The first version of this harness collapsed overflow into `no_route` while its own counter printed `Overloaded` — the spike would have reported a mapping it was not performing. Both the counter and the wire now say `overloaded`.

### A8. A cancellation race, not merely a slow one

`SPIKES.md` requires cancellation races alongside the same-key retransmission claim, and A6 never cancels anything — every response channel stays alive until an outcome is sent. This does: the connection carrying the **owner's** request is killed mid-admission, while waiters attach on a **separate** connection to the same identity.

That construction needs two physically distinct connections presenting one `source_peer` — two `Swarm`s built from one shared keypair, since `DedupKey` is scoped to the peer and a cancellation race worth testing has to leave some retransmissions alive while others die.

```text
owner admitted, on its own connection          ConnectionId(20)
server learned the owner's connection died     ConnectionClosed
surviving waiters that still received an answer 4
reservations held after the race settled       0
a NEW request for the same key is admitted afterward true
```

The owner's connection dying does not orphan the surviving waiters — they still receive the outcome once admission completes, on the connection that is still up — and the reservation is released rather than held forever waiting for a channel that no longer exists. A production implementation that released the reservation only when the *owner's* channel confirmed delivery would hang the waiters and leak the slot; this is the case that would have caught it.

### A11. Many waiters on one key — the bound that was missing

A6 proved waiters share the owner's outcome. A7 and A10 proved the reservation map is bounded across many keys and many peers. None of them asked how many waiters **one key** may accumulate, and the first measurement was blunt:

```text
same-key copies sent                40
times the MAP answered Waiter       39
times the MAP answered Overloaded    0     <- never refused
```

`ReservationMap::acquire` matched an existing key and returned `Waiter` **before consulting either budget**. Every waiter costs the caller a held `ResponseChannel` until the owner's admission resolves — A4 established that holding one across an await is legitimate, and A6 that every waiter must be held — so the cost is real and per request. A peer retransmitting a matching request while the owner awaited endpoint admission could grow that state without limit and never be told `overloaded`.

ADR-0019's "never creates a parallel enqueue path" was upheld the whole time: one enqueue, many waiters. What was absent was any bound on how much state one key may accumulate. **This is the finding a spike exists to produce** — the pattern Stage 6 derives from A6 would have inherited it.

Fixed in the production `ReservationMap` rather than worked around in the harness: waiters are charged against the same per-peer and global budgets owners are, and releasing a key returns all of it — owner and waiters were admitted as one outcome and are answered as one outcome, so returning only the owner's share would leak the rest and turn the ceiling into a lifetime quota. The experiment now runs with **no cap of its own**:

```text
responses received                            40
per-peer budget                                8
owners                                         1
waiters attached                               7
refused as overloaded BY THE MAP              32
accepted ON THE WIRE                           8
overloaded ON THE WIRE                        32
unexpected responses                           0
highest number of channels held at once        8
```

### A8 asked for the race; it did not check that the race happened

`killed` was set the instant `close_connection` returned, and that call only *requests* teardown. The admission timer then fired regardless — so if the connection was still alive, the waiters were answered in the ordinary way, the reservation released in the ordinary way, and every check passed. The experiment reported success for the one scenario it had not exercised. The server's own `InboundFailure`, which is the actual evidence, was only printed.

Admission completion is now gated on that observation; the outer deadline still bounds it, and expiring there fails the run rather than passing it. Live output carries `server learned the owner's connection died: ConnectionClosed`.

| mutation | result |
|---|---|
| the failure is never recorded, standing in for teardown delayed past the timer | four checks **false**, exit 1 |

### A6 printed its cleanup instead of requiring it

Every A6 check is satisfied once the parked channels have received the shared outcome — which happens whether or not the owner path then releases its reservation. `reservations.len()` was a `note`. A release that stopped happening left the experiment passing while contradicting its own recorded `reservations still held 0`, and for the *rejected* outcome it also contradicts the thing that makes rejection survivable: that a later retry becomes an owner rather than attaching to a corpse. Both are now required, for both outcomes.

| mutation | result |
|---|---|
| the owner never releases | four checks **false** (two per outcome), exit 1 |

That is the sixth instance of printed-not-asserted in this spike. The count is the finding: each round fixed the instances review named, and the next round found more. Enumerating the property is what ended it for event-loop deadlines; the same enumeration is what found A7 unasked.

### The same gap in A7 and A10, found by sweeping instead of waiting

Review found A10: its verdict checked `owners`, `overloaded` and the distinct sources — all decided at REQUEST time — so a run where responses were lost, timed out, or came back with an unexpected reason left `answered < PEERS` at the deadline and still passed. The experiment is about what the budget does to callers, and a caller that never heard is not a caller that was refused.

Sweeping every client-response experiment for the same property found **A7** as well, which nobody reported: it classified the refusals on the wire but never required a total, so the accepted responses could all have gone missing while `overloaded_on_the_wire == KEYS - PER_PEER` stayed true.

The sweep was noisy and the noise is the point — a first pass flagged A8 and A1 too, and reading them showed both already assert their counts and inspect their responses. A scan is what puts the right functions in front of you; it is not the judgement.

| mutation | result |
|---|---|
| A10 loses two client responses | `responses received 6` → `every request was answered` **false** |
| A10's server refuses with `NoRoute` | three checks **false**, exit 1 |
| A7 loses three client responses | two checks **false**, exit 1 |

A first attempt at the second of those mutated **A7** instead of A10, because `replace(..., 1)` takes the first occurrence in the file and A7 comes earlier — the same slip as an earlier round in this spike. It then `survived`, because the output was filtered to A10's section and A7's failure was outside the filter. Both halves are recorded here: mutate by function boundary, and read the WHOLE run.

**The verdict used to accept 33 of 40 requests.** It read `owners == 1 && high_water <= PER_PEER && overloaded == COPIES - PER_PEER`, which classifies one owner plus thirty-two refusals — and leaves the seven **waiters**, the bound this experiment exists to measure, merely printed. The polling loop can also exit on its deadline with `answered < COPIES`, so reaching the verdict never implied every request came back. A run where seven requests never reached the server satisfied the expression and closed the waiter-bound experiment as a success. Review found that; it is the same defect as A6's and B1's, one round later.

Every request is now accounted for, and the **wire** is checked against the map — counting only the map's own decisions would accept a run where the refusals never reached the client, or reached it wearing a different reason:

| mutation | result |
|---|---|
| loop exits seven responses early | `responses received 33` → `every request was answered` **false**. The *old* expression is still satisfied under this mutation, which is the finding |
| the map refuses correctly but the wire sends `NoRoute` | `unexpected responses 32` → two checks **false** |

A6 was corrected in the same change. It asserted "every response is the owner's outcome", which stops being true the moment any bound binds — a request the budget refused was never attached and was never promised that outcome. It now runs with a budget large enough not to bind (the bound is A11's subject) and asserts that every **attached** request got the owner's outcome.

### A10. The global reservation budget, reached by many peers

`DIRECT.md` states two separate limits ("128 global / 8 per source PeerId by default"), and A7 only ever reaches one of them. Eight distinct source peers, a generous per-peer budget of eight, and a global budget of three, so nothing but the global bound can be what refuses:

```text
distinct source peers                          8
global budget                                  3
per-peer budget (generous, cannot be the refuser) 8
admitted (owners)                              3
refused as overloaded                          5
distinct peers actually charged a reservation  3
```

Each admitted reservation is charged to a different `source_peer` — the connected peer, not a label the request chose — so the three that got in are three genuinely different accounting keys rather than three requests wearing one identity.

**Deleting the global check makes this report 8 admitted and 0 refused, while A7 still reports its documented 4 and 12.** That is the whole reason this experiment exists: the two limits fail independently, and a spike reaching only one of them has evidence about only one of them.

### A9. The `no_route` privacy class — driven through the production predicates

`SPIKES.md` lists "no_route privacy class" among the required cases. **Two earlier versions of this experiment were tautologies, and both were caught in review.** The first never ran the case. The second converted a label the request carried into an enum and handed it to a function that *discarded its argument* and returned `no_route` — so it measured whether a function that ignores its input returns the same value for every input, and reported the answer as a verdict. That is recorded here because a spike whose evidence was manufactured twice owes the reader the history.

What the property actually rests on is `EndpointRegistry::resolve_inbound` — whose five refusals are selected by five *independent predicates over real registry state* (configured? enabled? policy admits the sender? leased? default present?) — and `ResolveFailure::to_wire`, the production collapse. The responder now holds five registries in five genuinely different states, passes each request's destination through untouched, and lets the production code decide:

```text
unknown    -> local   EndpointUnknown
disabled   -> local   EndpointDisabled
unleased   -> local   EndpointOffline
nodefault  -> local   NoDefaultConfigured
denied     -> local   EndpointPolicyDenied
distinct LOCAL failures the production predicates produced  5
every response decodes to one identical value               true
and that value is no_route                                  true
and every encoding is byte-identical                        true
```

Two halves, and the first is the one the tautology lacked: **five distinct local failures** proves five independent predicates each fired, so a regression in any one — or two collapsing upstream of the encoder — is visible. The wire vocabulary is now the production `DirectRejectReason` (the spike's private copy is gone), encoded through the codec's own CBOR library and compared as bytes.

Proved load-bearing by breaking **production**, not the harness: making `to_wire` leak one variant fails the byte and value checks; making `resolve_inbound` collapse two predicates drops the distinct count to 4 and fails the verdict. The previous A9 would have stayed green under both.

## B — GossipSub

### B0. The ID function under test is the frozen one

`PUBSUB.md` freezes `GossipSubMessageIdV1` as a domain-separated, length-prefixed SHA-256 — not merely "source and sequence". An earlier version of this harness returned raw `source || u64be(sequence)`, which separates two publishers and would have passed B1 while saying nothing about the calculation Stage 7 ships.

The harness now computes the frozen function and checks it against the repository's own golden vectors before using it:

```text
sequence 0 matches the frozen vector                    true
sequence 1 matches the frozen vector                    true
sequence 18446744073709551615 matches the frozen vector true
```

Those are `fixtures/gossipsub/gossipsub-message-id-v1.json`, the same values quoted in `PUBSUB.md`. A spike that reimplements a frozen calculation and does not check it is a spike measuring its own reimplementation.

### B1. Two publishers, one application-envelope message id

Both publishers sent an identical application envelope carrying the same `message_id`. The receiver's message-ID function is `GossipSubMessageIdV1` itself, verified above — authenticated source PeerId plus 64-bit wire sequence number, never the envelope:

```text
messages delivered to the receiver             2
distinct mesh ids among them                   2
both publishers reached the application        true
```

Neither message suppressed the other. This is the property `PUBSUB.md` freezes: mesh duplicate identity is transport metadata, so an application ID collision — accidental or deliberate — cannot make one publisher silence another.

### B2. Authenticity precedes the duplicate cache

The question `PUBSUB.md` requires an implementation to answer against the exact target version: can an **invalid** signed-source claim create a lasting duplicate-cache entry that suppresses a later valid message with the same mesh id?

Setup: a forger publishing with `MessageAuthenticity::Author(victim_peer)` — claiming the victim's PeerId with no signature — a victim signing as itself, and a receiver in `ValidationMode::Strict`.

A fourth node makes the answer mean something: a **permissive** receiver wired directly to the forger, so its path to the forged message does not pass through the node under test.

```text
control delivered to the strict receiver            true   <- the mesh is live
forged publish accepted by its own node             true
forged message delivered to the PERMISSIVE receiver true   <- it ARRIVED at a receive path
forged message delivered to the STRICT receiver     false  <- and was rejected there
genuine message delivered to the strict receiver    true   <- NOT suppressed
```

**A second gap: the counts were never checked against *which* message arrived.** `pump` polls for a fixed duration, so a forgery delayed past its own window would land in the genuine window and be counted as the genuine delivery — and forged and genuine share payload, mesh id *and* source by construction, so filtering by payload cannot separate them. What neither can fake is the per-publisher **sequence number**: the forger's is random, the victim's is its own counter.

Attribution is now by sequence, across all windows combined. The forged sequence is read at the permissive receiver. The genuine one turned out **not** to be independently observable — the permissive receiver sits behind the forger, whose own duplicate cache already holds this mesh id from the forgery it published, so it never forwards the genuine message; a first version of this attribution tried to read it there and found `None`. It does not need to be observed: exactly two publications of this payload exist, so a strict delivery whose sequence is not the forged one *is* the genuine one, by elimination.

```text
forged sequence (seen at permissive)                     Some(7472259401723353819)
strict deliveries of the contested payload, ALL windows  1
their sequence numbers                                   [Some(1787644342344501503)]
strict delivered exactly one, and it is not the forgery  true
```

Proved by making the strict receiver permissive: the forgery is then delivered, poisons the cache, the genuine message is suppressed, and the strict node's single delivery carries the **forged** sequence. The old window check counted that as "genuine delivered" and passed; this fails it — which is exactly the false-PASS the reviewer described.

**That permissive receiver is the control this experiment turned out to need, and it failed the first time it was run.** Without it, "the forged message was not delivered" is equally explained by the forgery never reaching anyone — the experiment would close the spike without the invalid message ever touching a receive path. When it was first added it reported **zero**, because the star topology put it downstream of the strict receiver, which rejects the forgery and therefore never forwards it: a control that could only fail for the same reason as the thing it was controlling. Wiring it directly to the forger is what makes the numbers above evidence.

**Answer: yes, authenticity precedes the cache.** The forged message was rejected and left nothing behind; the genuine message carrying the same mesh id was delivered afterwards. No pre-cache authenticity gate needs to be prototyped, and `PUBSUB.md`'s conditional clause — "if a future rust-libp2p version changes that ordering" — remains a future concern rather than a present one.

**The deviation this experiment retains, and why it is no longer the compromise it was.** B2 derives the mesh id from the payload, because the *public* API does not let a caller choose a sequence number and a source+sequence collision cannot be arranged through it. That was written when the public API was the only way in, and it stopped being true inside this same harness: the raw injector added for B3 chooses sequence numbers directly, so the collision B2 approximates is now performed exactly, under the frozen rule, one section below.

B2 is kept as it stands rather than rewritten — it covers the *unsigned* publish path, which B3 does not — but it is redundant coverage, not the closest available approximation. B1 uses the real source+sequence rule; B3 uses it under collision. No future rust-libp2p release is required for anything here.

### B3. An invalid **signed** claim, colliding under the frozen mesh ID

B2 makes two substitutions: `MessageAuthenticity::Author` publishes **unsigned**, and the collision is forced with a payload-derived ID because the public API will not let a caller choose a sequence number. `PUBSUB.md` asks for neither — it asks whether an invalid **signed** source/sequence claim can poison the cache keyed by `GossipSubMessageIdV1`, which *is* source + sequence.

The injector in `src/inject.rs` writes `/meshsub/1.1.0` frames directly, and it can choose the sequence number — **which removed the reason the substitution existed**. Review noticed that before I did, after I had written the injector that made the caveat obsolete and left it standing. So B3 uses the frozen rule and collides on source and sequence exactly.

Three injections against one receiver, all through the same writer:

```text
mesh id rule                                    GossipSubMessageIdV1 (source + sequence)
control (correctly signed, seq 1): delivered    1
invalid signature (seq 2): delivered            0
genuine (seq 2, SAME mesh id as the invalid)    1
VERDICT: an invalid signed claim cannot poison GossipSubMessageIdV1   true
```

Injection (2) carries a signature that is present, well-formed, and computed over *different bytes than it carries*, so it cannot verify. Injection (3) shares its `(source, sequence)` — and therefore its `GossipSubMessageIdV1` — so if (2) had reached the duplicate cache before its signature was checked, (3) would be suppressed and never arrive.

**The control is not decoration.** Hand-rolled protobuf has its own failure mode: an encoding the receiver cannot parse is *also* rejected, and the experiment would report success for a reason having nothing to do with signatures.

**Absence of delivery only means refusal once presence on the wire is established.** Each injection builds a *fresh* injector and a fresh connection, and the write result went into `let _ =` with the handler reporting `ToBehaviour = ()`. So an injector that failed to connect, failed to negotiate `/meshsub/1.1.0`, or failed mid-write left `delivered_invalid == 0` looking exactly like a signature rejection — and the genuine injection then made every remaining check pass. The control cannot vouch for it, because it is a different injector on a different connection.

The handler now reports `Wrote::Frame` or `Wrote::Failed`, the behaviour surfaces it, and B3 requires the invalid and genuine frames to have reached the wire before reading anything into their delivery counts.

| mutation | result |
|---|---|
| the invalid injector never dials, so its frame is never written | `INVALID frame written: None` → three checks **false**, exit 1. The delivery counts are *unchanged* (1 / 0 / 1), so the old checks passed — that is the finding |

**Each injection carries its own payload, and that is the difference between measuring and guessing.** All three used to carry identical bytes, so a delivery was credited to whichever four-second interval it arrived in. An invalid publication delayed past its own interval and delivered during the genuine one would therefore increment the *genuine* counter — and because the genuine message may itself have been suppressed by the invalid one's cache entry, the checks would then read `invalid == 0, genuine == 1` and **PASS on precisely the outcome this experiment exists to rule out**. Review found that; it is a false pass, not a flake.

Distinct payloads cost nothing here because `GossipSubMessageIdV1` is source + sequence: the payload is not in the mesh id, so the (2)/(3) collision is exactly as tight as before. A final drain after the last injection catches a delivery still in flight when its own window closed, which the old shape would have read as suppression.

The rule is now a named function, `attribute`, with three unit tests beside it — including `an_invalid_delivery_is_never_credited_to_the_genuine_injection`, which is the false pass stated as an assertion. Replacing the function body with a constant fails all three.

> **These unit tests are not CI-enforced.** The harness declares its own `[workspace]`, so it is outside the root workspace and no job builds it; `cargo test` inside `harness/` is part of *running the spike*, not part of the merge gate. Said here rather than left to look enforced.
>
> The timing skew itself is **not** reproduced. On loopback every delivery lands inside its own window, and forcing a cross-window arrival by shrinking the first interval to 50ms did not mis-attribute even with the old rule restored. So the evidence for this fix is the tested attribution rule, not a demonstrated failure — weaker than a mutation of the live experiment, and labelled as such.

| mutation | result |
|---|---|
| sign the "invalid" message correctly | it is delivered, caches under `(source, seq 2)`, and **the genuine message is suppressed** — 3 checks fail |
| swap two protobuf field tags | control fails, and the verdict fails with it |
| receiver → `ValidationMode::Permissive` | no change: permissive still validates a signature that is *present*, so it cannot reach the collision logic — recorded as inconclusive, not evidence |

The first is the strongest evidence in this spike: it shows the collision genuinely detects a poisoned cache under the **frozen** ID function, rather than passing because nothing ever collides.

### A false verdict now fails the run

Every experiment used to `note` its result and return, so `main` reached `done` and exited **0** even when its own output disproved the recorded PASS. The `cargo run` this README tells you to reproduce with would have reported success, and a script checking the status would have been told the spike passed.

Observations the conclusions rest on now go through `check`, which tallies false answers, and `main` returns a non-zero `ExitCode` when any failed:

```text
done -- 3 REQUIRED observation(s) failed; the recorded PASS does not hold.
$ echo $?
1
```

**The first attempt converted a hand-picked list and review caught the rest** — A4's timeout branch printed `false` and returned without counting, so a failed prerequisite still ended in `done` and exit 0. Converting by sweeping every observation instead of every observation I remembered found two more classes:

- **A1 could not fail at all.** Its `while answered < 2` loop had no deadline, so a response that never arrives hangs the run forever — the one outcome no exit code can express. It now has a bounded deadline and reports the shortfall.
- **A5's label contradicted its own value.** It printed `and that channel is still answerable  false` — in a run recorded as PASS. The value was right and the *label* was inverted: the finding is that the channel is **not** answerable, which is what the comment beside it and this README both say. `note` prints without judging, so a line reading `false` under a positive claim sat there unremarked. It is now asserted with the correct polarity, so a rust-libp2p that started keeping the channel open would fail the run.

### The third round of the same finding

Review found the exit-code gap twice more after it was "fixed". The first fix converted a hand-picked list; the second converted every observation that was *already a boolean* — and still left whole experiments asserting nothing at all. What the third round caught:

| experiment | what could fail while the run exited 0 |
|---|---|
| **A2** | negotiation could time out, or fail with an error *other than* `UnsupportedProtocols`, and finding 3 would be recorded as reproduced |
| **A3** | the two protocol families could arrive on **two** connections — the exact case the experiment exists to detect — and the count was merely printed |
| **A7** | the per-peer bound could admit the wrong number, refuse the wrong number, or answer with a reason other than `overloaded`; the experiment contained no `check` whatsoever |
| **A8** | `acquire(...).is_ok()` is `Ok` for a **`Waiter`** as well as an `Owner`, so a leaked reservation made the check pass *because of* the leak it exists to rule out |

A8 is the one worth keeping in mind. The check was not weak by oversight — it was weak in the one direction that mattered, and it passed for the precise reason it should have failed.

Mutations, each restored afterwards:

| mutation | result |
|---|---|
| A2: remove the version mismatch so no `UnsupportedProtocols` occurs | `Io(Eof)` observed instead, check false, exit 1 |
| A7: give the map `PER_PEER + 1` | 4 checks false, exit 1 |
| A8: skip `release` on the cancellation path | `left no reservation behind` false, and `becomes an OWNER` **false** — under the old `is_ok()` this read `true` |
| A3 *(weaker: assertion flipped to `== 2`, not a forced second connection)* | fails against the live count of 1, so the check reads the measurement |

A3's is a coupling check rather than a true mutation: forcing a genuine second connection between the two sends hangs the harness, so the stronger evidence is not available and is not claimed.

### The fourth round, and what finally found the rest

Three more arrived after the third fix, and one of them — A3's unbounded response loop — was the *same defect as A1's*, in the commit whose message claimed a sweep. Fixing instances one at a time kept producing a next instance, so this round enumerated the two properties mechanically instead:

- **every event loop**, checked for a deadline arm. Two had none: `a3_two_families` and `direct.rs`'s `listen` helper.
- **every experiment**, checked for whether its *central* claim is asserted rather than printed.

### The fifth round, and the word that was doing the lying

The sweep above said "every event loop" and had covered `direct.rs`. `mesh.rs` has its own `listen` helper, with the same unbounded wait, and review found it — a fifth instance of the defect whose fourth instance had prompted a sweep specifically to end the series.

The enumeration was the right idea executed on the wrong set. "Every event loop" is a claim about the *harness*; the pass that produced it walked one file. That is not a smaller version of the same work, it is a different claim wearing its words — and because the sentence was written in the same commit as the fix, it read as a report of what had happened rather than an assertion anyone would check.

Redone by brace-matching every `loop`/`while` in all four sources and reading the body of each, the list is:

| site | verdict |
|---|---|
| `direct.rs` — 14 loops | all bounded |
| `mesh.rs:227` (`pump`), `mesh.rs:663` | bounded |
| `mesh.rs:213` (`listen`) | **unbounded — the finding** |
| `inject.rs:61` (`varint`) | not an event loop: pure computation over `n >>= 7`, terminates in ≤10 iterations, waits on nothing |

A first attempt at that scan read a fixed 22-line window after each loop header and reported `mesh.rs:213` as *bounded* — the window had run past the end of the five-line loop and into `pump`, which does have a deadline. A scan whose window is wrong finds nothing and says so confidently, which is the same failure as the sweep it was checking.

The bound is mutation-checked: removing `listen_on` so `NewListenAddr` never arrives panics with `no listen address within 20s` and exits 101, where before it hung with no exit code at all.

That second pass found one review had not: **B1** printed `distinct mesh ids among them` and required only that both publishers arrived. Two deliveries sharing **one** mesh id satisfied the experiment that exists to rule exactly that out.

| experiment | what was printed instead of required |
|---|---|
| **A3** | no deadline at all — a request that never arrives hangs the run |
| **A5** | the `Timeout`/`Io` attribution, the *first half* of the finding; the two checks covered only the retained channel |
| **A6** | `enqueues == 1`, the entire point of the experiment |
| **B1** | `distinct == 2`, the entire point of the experiment |

A6 is the one to keep. Its check — "every attached request got the owner's outcome" — stays **true** when `acquire` hands every copy an `Owner`, because all 24 get configured with the same outcome and all 24 receive it:

```text
local enqueues (owners)                        24
waiters attached to the owner                  0
every ATTACHED request got the owner's outcome true    <-- still true
exactly ONE local enqueue                      false
```

The old assertion was not weak in general. It was weak in the one direction the experiment existed to measure.

| mutation | result |
|---|---|
| A3 deadline → 1 ms | both families false, exit 1 |
| A5: the responder answers, so no timeout occurs | attribution checks false, exit 1 |
| A6: `acquire` returns `Owner` for every copy | 6 checks false — while the old one stayed true |
| B1: publishers share a payload-derived id | one suppressed the other, `distinct == 1`, exit 1 |

### An incidental finding about the library### An incidental finding about the library

`gossipsub::Behaviour::new` **refuses** to build a node that publishes unsigned while requiring signatures on receipt, and says so:

```text
Messages will be published unsigned and incoming unsigned messages will be
rejected. Consider adjusting the validation or privacy settings in the config
```

Worth recording because it says something about the design: unsigned publishing is a whole-node posture, not a per-message choice. The forger in B2 therefore runs `ValidationMode::Permissive` for itself; the receiver — the node under test — stays `Strict`.

## Decision

**Unlocked, as SPIKES.md scopes it:** implementation codec/task/channel details for Stage 6, without reopening endpoint routing, the direct-vs-GossipSub decision, or mesh-ID semantics.

**Not changed:** every architectural decision this spike could have disturbed held. `GossipSubMessageIdV1`'s source+sequence rule, the withheld-`AcceptedV2` pattern, the bounded reservation map, and the coarse `no_route` class all survive contact with the real library.

**Carried into Stage 6 as work:**

1. Do not branch on `OutboundFailure::Timeout` alone (A5).
2. Expect a dead `ResponseChannel` after a withheld response (A5).
3. `UnsupportedProtocols` is the major-version signal (A2).
4. One connection serves both protocol families (A1/A3).
