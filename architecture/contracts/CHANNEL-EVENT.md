# Claude Channel event contract

This document specifies bridge output, not the generic network transport. It is updated for transport v2 endpoint addressing.

## Notification

The bridge emits:

```text
notifications/claude/channel
```

with Channel `content` and string-valued `meta` in the form supported by the target Claude Code Channel reference.

## Content

- UTF-8 transport payload: forwarded as text essentially unchanged.
- non-UTF-8 payload: base64url string representation and `meta.payload_encoding=base64url`.
- the bridge does not parse JSON/application protocols to infer meaning.

## Metadata

Proposed stable keys:

| Key | Meaning |
|---|---|
| `source` | constant `p2p` |
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

All metadata values are bounded strings. The bridge rejects/normalizes control characters and never constructs channel markup by concatenating unescaped peer-controlled strings. Payload stays in `content`; routing metadata stays in `meta`.
