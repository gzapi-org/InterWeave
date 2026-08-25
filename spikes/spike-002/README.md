# SPIKE-002 — transport wire/race and GossipSub cache behavior

**Status: PASS**, with four findings that change how Stage 6 must be written and one that changes how it must be tested.

rust-libp2p direct request-response and concurrency/validation behavior. Authoritative objective, evidence requirements, and decision gate live in [`architecture/roadmap/SPIKES.md`](../../architecture/roadmap/SPIKES.md); this file records what was actually observed.

Do not treat the experiment in [`harness/`](./harness) as production implementation. It is deliberately outside the workspace — an empty `[workspace]` table in its manifest — so it cannot be built by `cargo xtask ci`, cannot enter `Cargo.lock`, and cannot become a production dependency by a stray `path =`.

**That isolation carries more weight here than it did for SPIKE-006.** This harness enables `request-response` and `gossipsub` on the same `libp2p` dependency the substrate uses, and Cargo unifies features across one build of one workspace. As a member it would have switched both protocols on inside `interweave-transport-libp2p` — undoing CLAUDE.md §3's "absent from the feature list rather than merely unused", and doing it with nothing in the production crate having changed.

## What was pinned

```text
libp2p =0.56.0   features: tcp, noise, yamux, identify, tokio, macros,
                           ed25519, request-response, gossipsub, cbor
```

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
per-peer budget                                8
owners                                         1
waiters attached                               7
refused as overloaded BY THE MAP              32
highest number of channels held at once        8
```

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

### A9. The `no_route` privacy class

`SPIKES.md` lists "no_route privacy class" among the cases this spike must exercise, and until now nothing did: `NoRoute` appeared only as a predetermined owner outcome in A6 and as the conflict arm in A7. A regression exposing endpoint-unknown and policy-denied as distinguishable answers would have left every recorded number unchanged.

`DIRECT.md`: `no_route` "deliberately collapses endpoint unknown, endpoint disabled, no active lease, missing default endpoint, and endpoint-specific policy denial. All such branches use the same wire code/response shape and shared response encoder."

Five requests, each selecting a genuinely different branch of the responder's routing decision:

```text
internal route failures exercised              5
responses received                             5
every response decodes to one identical value  true
and every encoding is byte-identical           true
```

The second check is the stronger one and the one the specification actually makes. Two values can compare equal in Rust and still serialize differently — a field added later with a skip condition would do it — so the five refusals are also encoded through **the same CBOR library the codec itself uses** and compared as bytes. Making a single branch answer `overloaded` instead turns both lines false.

Why it matters: distinguishable refusals are an endpoint oracle. A peer authorized for nothing learns which endpoints exist, which are disabled, and which refused it by policy, purely by probing.

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

**A second gap, found by review after the first fix: these counts were never checked against the message that arrived.** `pump` polls every node for a fixed duration regardless of what has already been published, and the first version of these counters treated any event delivered to a given index during a given window as the message that window was measuring. The control publication above is delivered asynchronously; had it been delayed into the `after_forgery` window instead of its own, it would have been counted as the forged message arriving, and the verdict could have read PASS without the contested payload ever having been involved. The counts now filter on the exact contested payload (`arrived`, in the harness), which cannot tell the forged and genuine deliveries apart from each other — they share one payload and one mesh id by construction — but rules out everything else, which is what the gap was actually about.

**That permissive receiver is the control this experiment turned out to need, and it failed the first time it was run.** Without it, "the forged message was not delivered" is equally explained by the forgery never reaching anyone — the experiment would close the spike without the invalid message ever touching a receive path. When it was first added it reported **zero**, because the star topology put it downstream of the strict receiver, which rejects the forgery and therefore never forwards it: a control that could only fail for the same reason as the thing it was controlling. Wiring it directly to the forger is what makes the numbers above evidence.

**Answer: yes, authenticity precedes the cache.** The forged message was rejected and left nothing behind; the genuine message carrying the same mesh id was delivered afterwards. No pre-cache authenticity gate needs to be prototyped, and `PUBSUB.md`'s conditional clause — "if a future rust-libp2p version changes that ordering" — remains a future concern rather than a present one.

**The deviation this experiment required, stated plainly.** The public API does not let a caller choose a message's sequence number, so a *source + sequence* collision between a forged message and a genuine one cannot be arranged through it. B2 therefore derives the mesh id from the payload, which forces exactly the collision the ordering question is about and changes nothing else in the receive path. B1 uses the real source+sequence rule. A future rust-libp2p that exposes sequence numbers would allow B2 to be run without the substitution; until then, this is the closest honest approximation, and the substitution is confined to the id function.

### An incidental finding about the library

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
