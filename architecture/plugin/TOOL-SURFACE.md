# Claude-facing tool surface

Names are conceptual; final packaging may namespace them to avoid collisions.

| Tool | Input | Meaning |
|---|---|---|
| `broadcast` | `channel`, `content`, optional `content_type` | publish realtime to a logical channel; caller must already be joined |
| `send` | `peer`, optional `endpoint`, `content`, optional `content_type` | direct send to a trusted PeerId and optional remote EndpointId; omitted endpoint requests remote default route |
| `reply` | `reply_token`, `content`, optional `content_type` | follow the exact route of a prior inbound event subject to current trust/subscription/endpoint-lease state |
| `join` | `channel` | acquire local subscription |
| `leave` | `channel` | release local subscription |
| `identity` | none | show local profile PeerId and this bridge's local EndpointId |
| `status` | none | high-level bridge/daemon/discovery/network health, endpoint lease, this bridge's joined channels, and in pull mode the pull queue's depth, whether the drain is paused and since when |
| `receive` | optional `max` | **pull mode only** (ADR-0023 A 2026-10-07): take what the bridge holds for this session — direct messages and broadcasts, in the order taken from the session, never waiting — one result `{events: [{kind, content, meta}], remaining, paused}`; `max` defaults to the granted `event_queue` and is clamped to it (`CHANNEL-EVENT.md` §Delivery); absent in push mode, where delivery is the Channel notification |

`content_type` is the Claude-facing name only. The bridge maps it to/from generic transport `Payload.media_type`.

## Bridge endpoint

Every direct-capable Claude bridge is configured with one local EndpointId and claims it during IPC v2 handshake. Common values such as `claude` are conventions only.

The bridge never accepts a `source_endpoint` tool argument. Its active IPC endpoint lease is the source of every direct send/reply.

If the configured endpoint is already leased by another process, direct operations fail clearly; the bridge must not silently choose a different route.

## What is not a Claude tool

- approve/trust/revoke peer;
- create/enable/rename/rebind local endpoints;
- mutate endpoint ACLs/advertisement/default route;
- rotate key;
- edit private configuration;
- add bootstrap infrastructure;
- dump multiaddresses/Swarm internals;
- force Kademlia queries;
- inspect private keys;
- stop/shutdown the shared transport daemon;
- execute arbitrary network protocol operations.

Those are local administrative/diagnostic actions available only on the admin socket. The Channel IPC client uses the data socket and cannot be granted `admin.endpoints` or `admin.shutdown`.

## Direct send semantics

Examples:

```text
send(peer=P, endpoint="human", content="hello")
```

targets exactly `P/human`.

```text
send(peer=P, content="hello")
```

asks `P` to resolve its configured `default_direct_endpoint`. It does not request broadcast/fan-out.

Result wording includes the endpoint that actually accepted the message when transport v2 returns it. `RemoteEndpointUnavailable` is intentionally coarse and does not claim whether the endpoint was unknown, offline, disabled, default-missing, or endpoint-policy denied.

## Reply semantics

For direct inbound messages, `reply_token` captures:

```text
remote_peer
remote_source_endpoint
local_destination_endpoint
local_endpoint_lease_epoch
```

Reply sends from the bridge's same currently leased local endpoint to the original remote source endpoint. If the bridge lost/reacquired the endpoint and the lease epoch changed, the stale token fails rather than switching routes.

Current outbound endpoint/profile trust policy still applies. A revoked peer fails `UnauthorizedPeer` before dialing.

For broadcast inbound, reply publishes to the same channel. The bridge must still hold its join reference; otherwise `ChannelNotJoined`.

Claude can choose explicit `send(peer, endpoint?, ...)` if it wants a private response to a broadcast source, subject to trust and endpoint routing.

## Status visibility

`status` includes:

```text
local_peer_id
local_endpoint
endpoint_lease_state
endpoint_lease_epoch
joined_channels
profile_desired_channels
rejoin_refused
transport_health
```

`joined_channels` and `profile_desired_channels` remain distinct. A profile-desired backend subscription does not authorize bridge broadcast or make it an inbound consumer.

Where each comes from (A 2026-10-06, Stage 16 step 3): `local_peer_id` is the session's `local_peer` (`contracts/LOCAL-CLIENT.md` §2), learned at open. `profile_desired_channels` is read from the profile's non-secret configuration document through `profile-config` (`ProfileConfig::load` over the same `ProfilePaths` that give the data socket; the socket paths themselves need no document read) and is reported **as configured**: the profile document's statement, not the daemon's live subscription state, which nothing on the data plane reports. When that load fails, `status` reports the field as unknown with the load error's class, never as an empty list. `joined_channels` are the joins the bridge made through its join tool and nothing else: the bridge joins no channel by configuration, a reconnect re-takes exactly the joins it held, and a profile-desired subscription confers no bridge membership. `rejoin_refused` lists `{channel, error}` for each re-join the daemon refused at a reconnect — `error` the join's error class, the vocabulary §Tool results uses — each row held until the next join or leave of that channel; `status` is the only place it is reported, never a channel event and no MCP logging notification (`plugin/LIFECYCLE.md` step 6).

## Tool results

Wording must be exact:

- receive (pull mode): events in the order taken from the session; `paused: true` means the bridge has stopped draining because its queue is full (the liveness clock starts only once the IPC client's own buffer fills behind it) — never "the daemon is refusing" (it refuses only once its own queue is full) and never "messages were lost here" (the bridge drops nothing it took; what a wedge close loses is the daemon's, `CHANNEL-EVENT.md` §Delivery); an empty queue returns `events: []` at once — the tool never waits;
- pull queue full (pull mode): `send`, `reply`, `broadcast`, `join` and `leave` answer at once with the bridge-local error "the pull queue is full: call receive first" — never `Overloaded`, never a stall; a call in flight when the queue fills is cancelled and answered the same;

- broadcast: "accepted for local publish" — never "delivered to all peers";
- direct: "remote transport accepted at endpoint <id>" — never "remote human/Claude processed";
- remote route failure: `RemoteEndpointUnavailable` without endpoint-existence claims;
- unauthorized destination: explicit `UnauthorizedPeer`;
- not joined: explicit `ChannelNotJoined`;
- endpoint lease absent/conflict: explicit local endpoint error;
- overload/drop: explicit error or degraded status, not false success.


`status` includes the normalized `ConnectivitySummary` (direct-inbound classification, relay readiness/targets, relayed-path count, hole-punch activity). It does not expose raw relay/probe control operations to Claude.
