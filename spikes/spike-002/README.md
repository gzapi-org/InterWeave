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

Explicit destination and omitted destination both survive the round trip, and the two protocol families (`/spike-002/direct/2.0.0`, `/spike-002/endpoints/1.0.0`) are answered over **one** connection — `network_info().num_peers() == 1` on the responder while both were in flight. Stage 6 does not need a connection per protocol family.

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

## B — GossipSub

### B1. Two publishers, one application-envelope message id

Both publishers sent an identical application envelope carrying the same `message_id`. The receiver's message-ID function is the frozen shape — authenticated source PeerId plus 64-bit wire sequence number, never the envelope:

```text
messages delivered to the receiver             2
distinct mesh ids among them                   2
both publishers reached the application        true
```

Neither message suppressed the other. This is the property `PUBSUB.md` freezes: mesh duplicate identity is transport metadata, so an application ID collision — accidental or deliberate — cannot make one publisher silence another.

### B2. Authenticity precedes the duplicate cache

The question `PUBSUB.md` requires an implementation to answer against the exact target version: can an **invalid** signed-source claim create a lasting duplicate-cache entry that suppresses a later valid message with the same mesh id?

Setup: a forger publishing with `MessageAuthenticity::Author(victim_peer)` — claiming the victim's PeerId with no signature — a victim signing as itself, and a receiver in `ValidationMode::Strict`.

```text
control publish accepted                       true
control delivered to the receiver              1     <- the mesh is live
receiver's connected peers                     2
forged publish accepted by its own node        true  <- it did leave the forger
forged message delivered to the receiver       0     <- and was rejected there
genuine publish accepted by its own node       true
genuine message delivered afterwards           1     <- NOT suppressed
```

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
