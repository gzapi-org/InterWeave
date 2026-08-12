# Human messages are ephemeral by default with a durable pending outbox, unread inbox, and receiver-kept set

**Status:** Accepted

## Context

The first human-client design described a conventional persistent local conversation history. That is broader retention than desired and creates unnecessary plaintext-at-rest/privacy exposure. At the same time, two classes of content must survive ordinary app/process closure: outbound messages that have not reached their transport-terminal state, and inbound messages the receiver has not yet read. After reading an inbound message, only the receiving human should be able to decide whether it remains durable.

This decision must remain above transport: `TransportRuntime` still has no durable mailbox, offline endpoint spool, missed-message replay, or durable network queue.

## Decision

Adopt the frozen application retention contract in `clients/human/RETENTION.md`.

The first-party human client stores message content durably only when:

1. **outbound pending** — the send has not yet reached the transport-terminal event; or
2. **inbound unread** — the human application has consumed and committed the inbound message but it has not entered local read state; or
3. **inbound kept** — after reading it, the receiving human explicitly chose `Keep`.

For direct traffic, `AcceptedV2` is transport-terminal for sender retention. For broadcast, successful local publication is terminal because no recipient acknowledgement exists. Transport-terminal outbound content is removed from durable storage. Read inbound content is removed unless the receiver explicitly keeps it.

The receiver-only `Keep` decision is local application state. It can be set only after local read state and is never accepted from a remote payload, EndpointId, contact label, or sender request.

A future explicit encrypted application backup may include **only inbound unread and inbound receiver-kept message content**. Pending outbound is durable locally but excluded from portable backup to avoid a restored/second device becoming an implicit replay/delayed-send source. Android system backup/device transfer remains disabled for all human message-content storage.

## Alternatives considered

Persist all conversation history; persist unread inbound only; persist all received content until explicit deletion; keep every message RAM-only; allow sender-requested retention; include pending outbound in portable backups; move the durable spool into `TransportRuntime`.

## Consequences

Ordinary delivered/read conversation content evaporates across client restart. Users keep only messages they explicitly preserve after reading. Unread content and local unsent/unaccepted outbound work survive crashes/restarts. The human store is no longer a conventional permanent chat-history database.

An inbound direct message may still be lost in the small handoff window after transport `AcceptedV2`/endpoint-queue admission but before the human application commits it unread. This ADR deliberately does not redefine `AcceptedV2` as an application-storage acknowledgement.

## Security implications

The design reduces default plaintext retention and prevents a remote sender from forcing long-term storage. Pending outbound and unread/kept inbound remain sensitive at rest and need normal application-store protections. Logs, notifications, search indexes, crash reports, analytics, OS backup and other caches must not become shadow archives that defeat deletion.

Portable backup excludes pending outbound because replaying an old outbox after restore can duplicate sends after transport dedup state expires. Any future portable outbox requires an explicit acknowledgement/replay design.

## Operational implications

If the human-store is unavailable/full, the client cannot honestly provide the unread-survives-restart property. It must expose degraded storage health, release/disable its direct EndpointId, and suspend local human broadcast joins/delivery until durable ingestion capacity is restored instead of silently accepting more messages as a healthy human receiver. Profile-level desired channels may remain mesh-warm without a local human consumer.

Restart reconstructs only pending outbound, unread inbound, receiver-kept inbound, and separately allowed non-message application state. There is no network history fetch.

## Implementation implications

Replace the general durable `messages`/conversation-history model with explicit pending-outbound, unread-inbound and kept-inbound states. Persist outbound before first send attempt. Consume inbound into the durable unread store before normal notification/UI presentation. Delete durable outbound on transport-terminal success and durable inbound on read unless receiver Keep applies.

Desktop and Android use the same retention state machine and conformance suite. Database migrations from any prototype/general-history schema must classify or discard rows deliberately; they must not preserve old full-history behavior by default.

## Revisit conditions

Revisit only if the product intentionally adds durable conversation history, cross-device history synchronization, portable outbox backup, application-level delivery/read receipts, remote retention policy, or a true durable mailbox protocol. Each would require a new explicit privacy, replay, acknowledgement and storage decision.
