# Claude-facing tool surface

Names are conceptual; final packaging may namespace them to avoid collisions.

| Tool | Input | Meaning |
|---|---|---|
| `broadcast` | `channel`, `content`, optional `content_type` | publish realtime to a logical channel |
| `send` | `peer`, `content`, optional `content_type` | direct 1:1 send over dedicated protocol |
| `reply` | `reply_token`, `content`, optional `content_type` | follow the route of a prior inbound event |
| `join` | `channel` | acquire local subscription |
| `leave` | `channel` | release local subscription |
| `identity` | none | show local transport PeerId/profile identity |
| `status` | none | high-level bridge/daemon/discovery/network health |

## What is not a Claude tool

- approve/trust/revoke peer;
- rotate key;
- edit private configuration;
- add bootstrap infrastructure;
- dump multiaddresses/Swarm internals;
- force Kademlia queries;
- inspect private keys;
- execute arbitrary network protocol operations.

Those are local administrative/diagnostic actions. This follows the Telegram pattern where a channel message cannot authorize access-policy mutation.

## Reply semantics

`reply` uses an opaque token from Channel metadata. For a direct inbound message it sends directly to the source peer. For a broadcast inbound message it publishes back to the same channel. Claude can choose explicit `send` if it wants a private response to a broadcast source.

## Tool results

Wording must be exact:

- broadcast: "accepted for local publish" — never "delivered to all peers";
- direct: "remote transport accepted" — never "remote Claude processed";
- overload/drop: explicit error or degraded status, not a false success.
