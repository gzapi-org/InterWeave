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

The map holds exactly its budget and the excess is refused. `DIRECT.md` lists `overloaded` as a **distinct** coarse reason from `no_route`, and the two mean different things to whoever receives them: one says the route is fine and to retry later, the other says there is nowhere to deliver. The first version of this harness collapsed overflow into `no_route` while its own counter printed `Overloaded` — the spike would have reported a mapping it was not performing. Both the counter and the wire now say `overloaded`.

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
