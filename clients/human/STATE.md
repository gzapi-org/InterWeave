# Human-client application state and persistence

Status: design only.

## Selected store

Use a versioned SQLite database behind a Rust `human-store` crate on desktop and Android. The application storage schema is independent of transport configuration/state and may be deleted/rebuilt without changing PeerId or trust policy.

## Logical tables

```text
contacts(contact_id, display_name, avatar_ref?, notes?, created_at, updated_at)
contact_routes(contact_id, peer_id, endpoint_id, device_label?, verification_note?, last_seen?)
conversations(conversation_id, peer_id, endpoint_id?, channel_id?, title?, last_activity)
messages(local_row_id, conversation_id, direction, app_message_id, transport_message_id?,
         source_peer?, source_endpoint?, media_type, payload/render_text, sent_at?, received_at?, status)
read_state(conversation_id, last_read_row_id)
drafts(conversation_id, text, updated_at)
settings(key, value)
```

No transport private key, trust allowlist, endpoint lease, Kademlia bucket, relay reservation, AutoNAT evidence, direct dedup record, or endpoint-directory cache is stored here.

## Message status honesty

Application history may record:

- local send requested;
- transport accepted remotely;
- transport rejected/timeout;
- local inbound accepted.

It must not label transport acceptance as “read by human” or “processed by app” without a future application-level receipt protocol.

## Migrations

Every schema migration is transactional and versioned. Migration failure puts the human app database into recovery/read-only/export mode; it never triggers transport identity regeneration.
