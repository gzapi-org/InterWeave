# Human-client application state and persistence

Status: design only. Message-content retention is normative through [`RETENTION.md`](./RETENTION.md) and ADR-0044.

## Selected store

Use a versioned SQLite database behind a Rust `human-store` crate on desktop and Android. The application storage schema is independent of transport configuration/state and may be deleted/rebuilt without changing PeerId or trust policy.

The store is **not** a conventional permanent conversation-history database. Message content is durable only in the retention states defined by `RETENTION.md`.

## Logical tables

Conceptually:

```text
contacts(contact_id, display_name, avatar_ref?, notes?, created_at, updated_at)
contact_routes(contact_id, peer_id, endpoint_id, device_label?, verification_note?, last_seen?)
conversation_index(conversation_id, peer_id, endpoint_id?, channel_id?, title?, last_activity?)
pending_outbound(local_row_id, conversation_id, app_message_id, transport_message_id?,
                 destination_peer?, destination_endpoint?, channel_id?, media_type?, payload,
                 created_at, last_attempt_at?, retry_state)
unread_inbound(local_row_id, conversation_id, app_message_id, source_peer, source_endpoint?,
               channel_id?, media_type?, payload, received_at)
kept_inbound(local_row_id, conversation_id, app_message_id, source_peer, source_endpoint?,
             channel_id?, media_type?, payload, received_at, read_at, kept_at)
settings(key, value)
```

A current-session RAM model may also hold transport-terminal outbound messages and read-but-unkept inbound messages for display. Those rows are not serialized as general message history.

No transport private key, trust allowlist, endpoint lease, Kademlia bucket, relay reservation, AutoNAT evidence, direct dedup record, or endpoint-directory cache is stored here.

## Retention transitions

- user sends -> durable `pending_outbound` is committed **before** the first transport call;
- direct `AcceptedV2` -> delete durable pending content;
- successful broadcast publication -> delete durable pending content, without claiming recipient delivery;
- inbound event consumed -> durable `unread_inbound` is committed before normal notification/UI presentation;
- inbound becomes locally read -> delete durable unread content unless the receiver explicitly chooses `Keep`;
- receiver chooses `Keep` after read -> durable `kept_inbound`;
- receiver removes `Keep` -> delete durable kept content.

Remote data can never set the Keep state.

## Message-status honesty

Application state may record:

- local send requested / pending;
- transport accepted remotely for direct;
- broadcast locally published;
- transport rejected/timeout;
- local inbound received;
- local UI read state;
- local receiver Keep state.

It must not label transport acceptance as “read by human” or “processed by app” without a future application-level receipt protocol. Local `read`/`Keep` are not sent remotely in v1.

## Store health

Unread durability is a human-client property, not a transport guarantee. If SQLite is unavailable/full/corrupt such that new unread content cannot be committed, the human client exposes degraded storage health, releases/disables its direct EndpointId, and suspends its local human broadcast joins/delivery until storage is healthy rather than present itself as a normally durable receiver. Profile `channels.desired` may remain network-prewarmed without local delivery.

There remains a bounded handoff window after transport endpoint-queue acceptance and before human-store commit. `AcceptedV2` is not redefined.

## Migrations

Every schema migration is transactional and versioned. Migration failure puts the human app database into recovery/read-only/export mode; it never triggers transport identity regeneration.

A migration from any prototype/general-history schema must not preserve full history by default. It must explicitly classify rows into pending-outbound, unread-inbound, receiver-kept-inbound, or discard message content according to ADR-0044.

## Backup policy

Android system backup/device transfer excludes the entire human-store database.

A future explicit encrypted application backup may include **message content only from `unread_inbound` and `kept_inbound`**. `pending_outbound` is local crash/restart state but is excluded from portable backup to prevent restored/second devices from becoming implicit replay/delayed-send sources.

Contacts/configuration backup remains a separate policy and does not change message-retention eligibility.
