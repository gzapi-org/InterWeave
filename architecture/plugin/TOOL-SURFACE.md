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
| `status` | none | high-level bridge/daemon/discovery/network health, endpoint lease, and this bridge's joined channels |

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
transport_health
```

`joined_channels` and `profile_desired_channels` remain distinct. A profile-desired backend subscription does not authorize bridge broadcast or make it an inbound consumer.

## Tool results

Wording must be exact:

- broadcast: "accepted for local publish" — never "delivered to all peers";
- direct: "remote transport accepted at endpoint <id>" — never "remote human/Claude processed";
- remote route failure: `RemoteEndpointUnavailable` without endpoint-existence claims;
- unauthorized destination: explicit `UnauthorizedPeer`;
- not joined: explicit `ChannelNotJoined`;
- endpoint lease absent/conflict: explicit local endpoint error;
- overload/drop: explicit error or degraded status, not false success.


`status` includes the normalized `ConnectivitySummary` (direct-inbound classification, relay readiness/targets, relayed-path count, hole-punch activity). It does not expose raw relay/probe control operations to Claude.
