# Claude Channel event contract

This document specifies bridge output, not the generic network transport.

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
| `message_id` | normalized 128-bit transport message ID |
| `received_at` | RFC3339 UTC timestamp |
| `channel` | logical ChannelId; only for broadcast |
| `reply_token` | opaque, short-lived local bridge routing token |
| `payload_encoding` | `utf8` or `base64url` |
| `content_type` | optional safe media type |

At the bridge boundary, transport `Payload.media_type` maps one-for-one to Claude-facing `meta.content_type`; the libp2p and generic transport layers use the name `media_type`, while only the Claude-facing representation uses `content_type`.

`source_peer` proves only a transport cryptographic identity. It must not be described as an employee, agent, host role, or authorization principal unless a higher-level protocol establishes that binding.

## Reply token

A reply token is local, opaque, unguessable, short-lived, and never a libp2p handle. It maps:

- direct inbound -> source PeerId;
- broadcast inbound -> source ChannelId and broadcast mode.

Default TTL: 30 minutes, bounded maximum entries: 2048 per bridge process. Tokens disappear on bridge restart. Explicit `send`/`broadcast` can be used after token expiry.

When multiple bridges share one profile, each bridge that receives the same admitted direct transport event creates its **own** reply token resolving to the same source PeerId. A token identifies a route, not an exclusive claim on that network message.

A broadcast reply token does **not** confer or recreate a subscription. If the calling bridge has left the mapped channel since receiving the event, `reply` fails with `ChannelNotJoined`; it does not implicitly rejoin or publish on another client's subscription.

## Sanitization

All metadata values are bounded strings. The bridge rejects/normalizes control characters and never constructs channel markup by concatenating unescaped peer-controlled strings. Payload stays in `content`; routing metadata stays in `meta`.
