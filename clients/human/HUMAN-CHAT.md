# HumanChatV1 application envelope

Status: **first-party application protocol design; not part of transport v2**.

The desktop and Android human clients need an interoperable text-message envelope while the generic transport remains payload-agnostic.

## Media type

```text
application/vnd.claude-p2p-human-chat+json;v=1
```

## Envelope

Conceptual canonical fields:

```text
{
  "v": 1,
  "kind": "text",
  "app_message_id": "<128-bit printable id>",
  "text": "<UTF-8 text>",
  "reply_to": "<app_message_id>?",
  "sent_at_ms": <diagnostic integer?>,
  "from_endpoint": "<EndpointId>?"
}
```

Rules:

- UTF-8 JSON, bounded so the encoded payload remains <= the transport effective payload limit;
- `app_message_id` is application history/reply identity, not DirectMessageV2 dedup identity;
- `sent_at_ms` is display/diagnostic only and never trust/replay authority;
- direct transport metadata is authoritative for `source_peer` and peer-asserted `source_endpoint`; any conflicting application `from_endpoint` is ignored for routing/authority;
- on broadcast, `from_endpoint` is an explicitly **unauthenticated display hint** because transport broadcast origin is PeerId-only;
- unknown fields are ignored for forward compatibility within v1 bounds;
- active markup/script is not supported; `text` renders as plain text;
- attachments, edits, reactions, typing indicators, delivery/read receipts, and multi-device sync are not part of HumanChatV1.

A later richer chat protocol can version independently of transport.
