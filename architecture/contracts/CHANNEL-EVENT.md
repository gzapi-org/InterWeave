# Claude Channel event contract

This document specifies bridge output, not the generic network transport. It is updated for transport v2 endpoint addressing.

## Delivery

The bridge delivers an inbound event in ONE of two modes, chosen by configuration at start — `--delivery push|pull`, required, one mode per process (ADR-0002 A 2026-10-07):

- **push** (the Claude Code Channel contract): the bridge emits

  ```text
  notifications/claude/channel
  ```

  with Channel `content` and string-valued `meta` in the form supported by the target Claude Code Channel reference, and advertises the `claude/channel` capability;

- **pull** (any MCP host): the bridge advertises the `receive` tool and no `claude/channel`; `receive(max)` returns one structured result `{events: [{kind, content, meta}], remaining, paused}` — `kind` is `direct | broadcast` (session notices are bridge state and `status`, never delivered, in both modes), `content` and `meta` are exactly what the push would carry, with no channel markup; events come in the order the bridge took them from the session; `max` defaults to the session's granted `event_queue` and is clamped to it; `remaining` is the queue's depth after the take; `paused` says the bridge has stopped draining because the queue is full. The pull queue is the bridge's, bounded at the granted `event_queue`, and DROPS NOTHING it took: full, the bridge pauses draining. What follows is the contract's existing fate for any client that stops draining (`LOCAL-IPC.md` §Push events), now reachable by design, and is stated so the host can avoid it: the pipeline behind the bridge — the IPC client's buffer, the socket, the server's event lane, the session queue, `LOCAL-IPC.md`'s capacity — keeps accepting, and a direct message is refused to its sender as overloaded only once the session queue itself is full (`TRANSPORT.md` §Backpressure); when the pause outlasts the keepalive's miss threshold the daemon closes the session as wedged, and what the pipeline held is lost with it — bounded by that capacity, never replayed, a message acknowledged to its sender that does not reach the host; the bridge reports the ended session in `status` and reconnects (`LIFECYCLE.md`), keeping its own queue. The bridge's queue is in memory: it is lost, bounded at `event_queue` and never replayed, when the bridge process exits, as the reply tokens are (§Reply token). So `paused: true` means the bridge has stopped taking, not that the daemon is refusing yet; the liveness clock starts only once the IPC client's own buffer has filled behind the paused bridge (it answers pings until then), so a quiet session may stay paused indefinitely; a host that calls `receive` before that clock runs out loses no direct message acknowledged to its sender, while broadcasts past the daemon's bound are dropped and counted there; `status` shows `paused_since` beside the depth. While paused, a tool that needs the session — `send`, `reply`, `broadcast`, `join`, `leave` — is refused at once, without calling it, with a bridge-local error naming the state ("the pull queue is full: call receive first"), never `Overloaded` and never a stall: a call awaited behind the paused drain would hold the bridge's one loop and the host's next `receive` with it; a call already in flight when the queue fills is cancelled (`LOCAL-IPC.md`'s `cancel`) and answered the same error, so no call ever waits behind the drain — and because the cancel is advisory the call may already have reached the daemon, so the error says the outcome is unknown: a `send` or `broadcast` the host repeats after `receive` may go twice — they are not re-issued, since the daemon does not deduplicate a new message — while a cancelled `join`, re-join or `leave` is kept PENDING and re-issued by the bridge once the pause lifts, before it drains again, because the daemon's join and leave are idempotent (one reference per channel per session); the re-issue's answer fixes the state — joined, left, or the daemon's refusal in `status.rejoin_refused` as today — a re-issue cancelled again stays pending, `status.pull_queue.pending` lists `{channel, op}` apart from `rejoin_refused` (which keeps its meaning: the daemon refused), and a `reply` to a broadcast on a channel whose join is pending never reaches the route: while paused it meets the pull-queue refusal first, and while disconnected the answer is decided by the bridge's CURRENT MEMBERSHIP alone: a channel the bridge holds answers the session's absence (`BackendUnavailable`); a channel it does not hold — left, a re-join refused, a re-join cancelled at a reconnect (which leaves the channel until its re-issue says it is joined, `LIFECYCLE.md` step 6), a join never landed — answers `ChannelNotJoined`; a cancelled HOST join never changes membership, so it decides nothing here — until the reconnect, at which the bridge resolves every pending entry before it reads another host line; the host's own answer stays "cancelled in flight, outcome unknown", so it is told and need not retry; the bridge reports the ended session in `status` only when a take next leaves it room to read, since paused it reads nothing — true, but late; `identity`, `status` and `receive` answer as ever, and `receive` lifts the pause. The reply token of a pulled event is minted at the take, under the epoch of the lease the event arrived under, so its TTL runs from the host's reading and every held DIRECT message's token is stale after a reconnect (§Reply token: a direct token needs the same lease epoch; a broadcast token maps to a channel and survives the re-join of `LIFECYCLE.md` step 6). The queue exists because the bridge must drain its IPC session whether or not the host calls (`LOCAL-IPC.md` §Push events).

The rules below — content, metadata, the bridge endpoint, the reply token, sanitisation — are the same in both modes. A pull host has no tag and therefore no host-set `source` attribute (§Metadata); it reads provenance from `meta` as the tag's consumer does, and the bridge's instructions (`INSTRUCTIONS.md`) are the same in both modes. How a plain host frames a tool result as external input is the host's; #221's scripted client records what it observed, and the content/meta separation, which §Sanitization rests on, holds in the structured result as in the tag.

## Content

- UTF-8 transport payload: forwarded as text essentially unchanged.
- non-UTF-8 payload: base64url string representation and `meta.payload_encoding=base64url`.
- the bridge does not parse JSON/application protocols to infer meaning.

**Content-encoding is decoded first, and that is not application parsing.** When the media type carries a content-encoding parameter — currently only `;ce=br`, the brotli form of HumanChatV2 (ADR-0050) — the bridge decodes it before applying the three rules above, under the mandatory streaming cap and mid-stream abort of `clients/human/HUMAN-CHAT.md`. The recovered bytes are then classified normally — except that recovered bytes that are not UTF-8 are dropped, never base64url (below): a HumanChatV2 envelope is UTF-8 and therefore forwards as text unchanged, with `meta.payload_encoding=utf8`.

The distinction is between representation and meaning. Decoding says what the bytes *are*; parsing would say what they *mean*, and the bridge still does neither for the envelope — it does not read `text`, `reply_to`, or any other field to infer anything. Without this step a compressed envelope would satisfy the non-UTF-8 rule, reach the model as opaque base64url, and make the decoded-size cap below unenforceable, since it is calibrated against decoded bytes.

`meta.content_type` reports the media type **with the content-encoding parameter removed**, because the encoding described how the payload travelled, not what it is. A payload the bridge could not decode within the cap is dropped with a bridge-local error and is never forwarded as partial content. So is a `;ce=br` payload whose stream decodes within the cap to bytes that are not UTF-8: HumanChatV2 is a UTF-8 JSON envelope, so those bytes are a payload that failed its decode path and is malformed (`HUMAN-CHAT.md` §Media type), and the bridge drops it with a bridge-local error rather than forwarding the recovered bytes as base64url — one decode path, and the bytes of a malformed envelope tell the model nothing (A 2026-10-05, #194).

## Metadata

Proposed stable keys:

| Key | Meaning |
|---|---|
| `delivery_mode` | `broadcast` or `direct` |
| `source_peer` | authenticated transport PeerId string |
| `source_endpoint` | direct only: remote peer-asserted EndpointId route |
| `destination_endpoint` | direct only: this bridge's resolved local EndpointId |
| `message_id` | normalized 128-bit transport message ID |
| `received_at` | RFC3339 UTC timestamp |
| `channel` | logical ChannelId; only for broadcast |
| `reply_token` | opaque, short-lived local bridge routing token |
| `payload_encoding` | `utf8` or `base64url` |
| `content_type` | optional safe media type |

At the bridge boundary, transport `Payload.media_type` maps one-for-one to Claude-facing `meta.content_type`.

No key is named `source` (A 2026-10-03, on SPIKE-001 fact 11): the host sets the tag's `source` attribute itself, from the server name, and a `meta` key of that name is not merged with it but rendered as a second `source` attribute on the same tag. The constant `p2p` this table carried under that name said nothing a consumer could act on — which bridge spoke is the host's attribute, and whose message it is is `source_peer` and `source_endpoint`. Every key here matches the host's `^[a-zA-Z_][a-zA-Z0-9_]*$` (a key outside it is dropped by the host and logged, fact 12); a new key is added under that grammar or not at all. The host renders the keys in the order the bridge sent them (fact 10); the order the bridge chooses is CLAUDE-CODE-CHANNEL.md's, not this contract's.

`source_peer` proves only a transport cryptographic identity. `source_endpoint` is a routing label asserted by that authenticated peer. Neither may be described as an employee, human, agent role, host role, or application authorization principal unless a higher-level protocol separately establishes that binding.

## Bridge endpoint identity

Each Claude bridge that needs direct messaging connects to IPC v2 under one configured EndpointId, commonly `claude` or another operator-selected route.

The bridge does not choose a source endpoint per message. Its IPC lease defines the source route for all direct sends/replies during that connection.

If the endpoint lease cannot be obtained (for example another live bridge already owns `claude`), direct operations are unavailable and `status` reports the conflict. The bridge must not silently claim another endpoint.

## Reply token

A reply token is local, opaque, unguessable, short-lived, and never a libp2p handle.

It maps:

- direct inbound -> `{remote_peer=source_peer, remote_endpoint=source_endpoint, local_endpoint=destination_endpoint, local_lease_epoch}`;
- broadcast inbound -> `{channel, mode=broadcast}`.

Default TTL: 30 minutes, bounded maximum entries: 2048 per bridge process. Tokens disappear on bridge restart.

For direct reply:

- bridge must still own the same `local_endpoint` lease epoch;
- destination is the original remote `source_endpoint`;
- current profile and endpoint outbound trust/policy still apply;
- token never falls back to remote default endpoint or a different local endpoint.

A broadcast reply token does **not** confer or recreate a subscription. If the bridge has left the mapped channel, `reply` fails `ChannelNotJoined`.

## Sanitization

All metadata values are bounded strings. The bridge rejects/normalizes control characters and never constructs channel markup by concatenating unescaped peer-controlled strings. Payload stays in `content`; routing metadata stays in `meta`. This rule is load-bearing, not defensive (A 2026-10-03, on SPIKE-001 fact 14, Claude Code 2.1.285): the host XML-escapes `meta` values but does NOT escape the body beyond a closing `</channel>`, so a body is exactly what the bridge hands over — a forged opening tag in a peer's payload reaches the model as text inside the real tag, which is the containment the provenance framing relies on, and anything the bridge itself composed into the body would be indistinguishable from it.
