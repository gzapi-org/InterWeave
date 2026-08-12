# Claude-facing tool surface

Names are conceptual; final packaging may namespace them to avoid collisions.

| Tool | Input | Meaning |
|---|---|---|
| `broadcast` | `channel`, `content`, optional `content_type` | publish realtime to a logical channel; caller must already be joined |
| `send` | `peer`, `content`, optional `content_type` | direct 1:1 send over dedicated protocol to a trusted peer |
| `reply` | `reply_token`, `content`, optional `content_type` | follow the route of a prior inbound event subject to current trust/subscription state |
| `join` | `channel` | acquire local subscription |
| `leave` | `channel` | release local subscription |
| `identity` | none | show local transport PeerId/profile identity |
| `status` | none | high-level bridge/daemon/discovery/network health plus this bridge's joined channels |

`content_type` is the Claude-facing name only. The bridge maps it to/from generic transport `Payload.media_type`; libp2p envelopes also use `media_type`.

## What is not a Claude tool

- approve/trust/revoke peer;
- rotate key;
- edit private configuration;
- add bootstrap infrastructure;
- dump multiaddresses/Swarm internals;
- force Kademlia queries;
- inspect private keys;
- stop/shutdown the shared transport daemon;
- execute arbitrary network protocol operations.

Those are local administrative/diagnostic actions. This follows the Telegram pattern where a channel message cannot authorize access-policy mutation. The Channel IPC client is not granted the daemon's `admin.shutdown` capability.

## Reply semantics

`reply` uses an opaque token from Channel metadata. For a direct inbound message it sends directly to the source peer and therefore applies current outbound `PeerTrustPolicy`; if that peer has since been revoked, the operation fails as `UnauthorizedPeer` before dialing.

For a broadcast inbound message it publishes back to the same channel. The calling bridge must still hold its join reference. If it has left since receiving the message, `reply` fails as `ChannelNotJoined`; the reply token never implicitly rejoins.

Claude can choose explicit `send` if it wants a private response to a broadcast source, subject to the same trust policy.

## Status subscription visibility

`status` includes the caller bridge's current `joined_channels` (derived from `subscriptions()`) so a restarted/reconnected Claude can see what it has actually re-established. It may also report `profile_desired_channels` separately for operator context. The two fields must not be conflated: a profile-desired backend subscription does not authorize `broadcast` for a bridge and does not make that bridge an inbound broadcast consumer.

## Tool results

Wording must be exact:

- broadcast: "accepted for local publish" — never "delivered to all peers";
- direct: "remote transport accepted" — never "remote Claude processed";
- unauthorized destination: explicit `UnauthorizedPeer`, not a connectivity failure;
- not joined: explicit `ChannelNotJoined`, not an implicit join;
- overload/drop: explicit error or degraded status, not a false success.
