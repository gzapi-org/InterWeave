# transport-client

The human client's transport facade (plan §17 (1); the blueprint's `human-transport-client`). It is generic over the neutral `DataSessionBinding`, with the admin `Status` connection beside it, and it owns the client's half of retention:
- commit-pending, then send, then transport-terminal;
- drain, then commit-unread, then present;
- re-open with backoff when a binding's connection has ended;
- the degraded-storage reaction;
- the retry, which resends the stored bytes (ADR-0050 rule 7) under the transport `MessageId` stored with the row (ADR-0019; schema v6, `HUMAN-CHAT.md` §Compression).

It depends on `local-client-api`, `transport-api`, `human-store` and `chat-protocol`, and names nothing under `crates/transport/*`, no libp2p and no Slint.

**Current status:** active workspace member since Stage 14 batch 5, built and tested against `tests/local-client-fake`.

## The contract

The caller-facing surface was agreed with the client's role before it was built (relay seqs 10522, 10534 and 10540), and amended after #167's review (10561, 10567, 10570). Every `TransportError` maps exhaustively onto these types, and none of them claims more than the transport proved.

**Outbound status** (`OutboundStatus`):
- `Sending`: the facade retries on its own, without bound, and nothing that went out may have reached the remote. The backoff starts at 1 s, doubles, caps at 5 min, and has deterministic per-row jitter.
- `Unconfirmed`: retried the same way, but an earlier attempt MAY have reached the remote. That follows any error in `TRANSPORT.md` §Error model's "outcome unknown" class: `Timeout`, `CancellationRaced`, `PeerUnreachable`, `BackendUnavailable`, `ShuttingDown`, `ProtocolViolation` and `Internal`. "May have reached" only ever goes from false to true. The retry goes under the same transport id, which the receiver's dedup makes safe.
- `NeedsAttention { problem, may_have_reached }`: no retry until the person acts. The row stays pending and durable; there is no "failed" terminal state.
- `Accepted`: bounded remote queue admission, never "read" or "seen".
- `Published`: published locally, not delivered to any recipient in particular.
- `Cancelled { may_have_reached }`. After a restart, `may_have_reached` is derived as `attempts > 0`. An attempt is recorded before the transport call, so this errs only toward "may".

`send` commits nothing, and returns an error, in these cases:
- the envelope is too large;
- it is not one a receiver would accept;
- the configuration cannot send there (`NotConfigured`: a channel it does not join, or a direct send with no endpoint);
- the store cannot hold it.

A row that survived a restart into a configuration that cannot send it becomes `NeedsAttention(NotConfigured)`, and the session is left alone. `NotConfigured` comes from that configuration check alone. From the transport, `EndpointNotRegistered` or `ChannelNotJoined` means the session lost its lease or a join: the facade re-opens and retries.

**Send problems** (`SendProblem`), as `human-client-ui.md` §12 lists them. The transient ones are `NoNetworkPath`, `Busy`, `ServiceUnavailable` and `RouteUnavailable`.

**Session** (`SessionState`):
- `Ready`;
- `Reconnecting`;
- `Refused { SessionProblem }`, left only through `reopen`, so a refusal never loops;
- `StorageDegraded`: the lease is released and joins are suspended until a re-check finds the store healthy;
- `Closed`.

**Connectivity** (`human-client-ui.md` §7):
- `OnlineDirect`;
- `OnlineRelay`;
- `OnlinePartial`;
- `Offline`;
- `Unknown`, which is never shown as `Offline`.

It is read from the runtime's state pushed on the data session (`ServerState`, at open and on each change) and from the admin port's status, asked every 5 s; the later wins. A pushed state with no summary changes it only when the runtime is unavailable (`Offline`).

**Route indicator** (`human-client-ui.md` §7): a path change to a peer the session has a route to (`PeerPathChanged`) is `ClientEvent::PeerPath { peer, path }`, the newest path per peer. It is never a reconnect, a disconnection or a message. A session event drops every queued path, since a route is a session's and the model clears its paths at that event. A peer's disconnection drops its queued path, which describes a connection that has gone; the runtime withdraws its own pending notice at a disconnection (`LOCAL-CLIENT.md` §2), so a path taken after one is a change since the reconnect and comes out behind the disconnection (`queue.rs` tests).

**Inbound** (`drain`):
- A message is committed as unread before it is returned.
- A duplicate of a held row is not returned.
- A malformed envelope is discarded and counted per reason, and never stored (`HUMAN-CHAT.md` §Consumers).
- Inbound is ordered by local `received_at`, never by the peer-asserted `sent_at_ms`.
- Whenever the facade closes a session (a lease loss, or a lost connection, which can be a remote shutdown with this session healthy), what the session holds at that moment is committed first, in one read, then handed over. Up to one session queue's worth waits for hand-over. Past that, a message stays unread in the store and is counted (`held_overflow`), and the facade emits `ClientEvent::UnreadInStore { not_handed_over }`: re-list `unread_inbound` to show it (agreed amendment A5, relay seqs 10582, 10585). Three rules make it safe to act on:
  - the event is pushed only after the commits it counts are durable, so a re-list it triggers contains those rows;
  - `Received.row` is the store's own row id, the one `unread_inbound` returns, so a re-list merges with what `drain` gave by row id;
  - `not_handed_over` is cumulative and monotone over the facade's life, and is never reset by a re-open.

**Trust** (`human-client-ui.md` §8, `LOCAL-CLIENT.md` §7 item 11): `trust()` reads the allowlist and this profile's own identity; `set_trust(peer, allowed)` allows or revokes a peer and returns the allowlist read back afterwards. Each call opens an administrative connection of its own holding `admin.trust` and nothing else, and closes it with the call: the standing status connection never holds trust authority, and none is held between a person's actions (`tests/facade.rs`: `a_trust_call_opens_a_connection_holding_admin_trust_alone`). A read's failure is a `TrustProblem` class; `Refused` is the daemon's refusal of this profile's own identity or of a new peer past the allowlist's ceiling. A change's failure (`TrustSetFailure`) also says whether the change was made: `NotMade` when the connection or the set failed before anything left, `Unconfirmed` when the set failed after it may have reached the daemon (`TRANSPORT.md`'s outcome-unknown class), `MadeNotReadBack` when only the read-back failed (`tests/facade.rs`: `a_trust_failure_says_whether_the_change_was_made`). The raw code is not kept. The allowlist is a runtime overlay, lost when the daemon restarts (ADR-0028). A revocation reaches every open session as the peer's `PeerDisconnected`.

**Driving:** poll-driven, and nothing is spawned. There are two clocks:
- `tick(now)` and every `now` are the caller's MONOTONIC clock, and drive schedules only.
- Persisted and wire times (`created_at`, `received_at`, an attempt's time, a broadcast's `sent_at_ms`) come from the WALL clock given to the constructor, in Unix ms.

Events come out of one queue coalesced per row, session, connectivity, peer disconnection and peer path, latest wins. The latest value per key is kept until taken, so a row's terminal status is never lost, a peer's path being the one key dropped (the route indicator, above); intermediate session states between two polls collapse into the last one.

## What it does not do

- **Read, keep and unkeep** stay on the store (`store_mut`).
- **Hiding a late duplicate is ui-model's job.** A duplicate that arrives after the first copy was read, and outside the transport's dedup window, is returned again; hiding it is ui-model's, within a session. After a restart it shows again as unread. Closing that means retaining bounded, content-free read (origin, application id) pairs, which `RETENTION.md` §5 allows. Plan §18 carries it to Stage 15.
- **A retry is deduplicated only inside the receiver's window.** A retry is deduplicated by the receiver only while the re-opened session holds the same endpoint lease, and only inside ADR-0019's window.
