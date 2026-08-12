# No persistent offline message store

**Status:** Accepted; transport/runtime boundary retained, first-party human application retention clarified by ADR-0044.

## Context

A durable transport store changes security, retention, replay, quotas, deletion, ordering, and acknowledgement semantics. Endpoint addressing makes the temptation to queue for an offline human/Claude route especially strong.

## Decision

Do not write application messages to disk for later network, endpoint, Claude, or human-client delivery. If a target EndpointId has no active lease, direct v2 returns `no_route` and does not accept/store the message.

ADR-0044 defines a narrow first-party human-application exception above transport: the human app may durably retain pending outbound content, inbound unread content, and inbound content explicitly kept by the receiver after reading. This state lives in the human application store, not `TransportRuntime`, and does not create a remote mailbox or make an offline endpoint reachable.

## Alternatives considered

SQLite mailbox; append-only log; reuse peer cache; hidden IPC replay buffer; per-endpoint spool.

## Consequences

Architecture stays simple/private but endpoints must be online for realtime direct delivery.

## Security implications

No default payload-at-rest transport database. The human application retention set from ADR-0044 has its own security/privacy policy and must not leak into daemon/runtime storage.

## Operational implications

Transport restart/reconnect has no missed-message replay. A human-client restart may reconstruct only its application-owned pending-outbound, unread-inbound, and receiver-kept-inbound state; it never asks transport for history.

## Implementation implications

Do not serialize runtime/endpoint payload queues during shutdown. EndpointRegistry persists config only, not messages/leases. Peer cache and endpoint-directory cache never contain payloads. Human application persistence is implemented only in `human-store` under ADR-0044.

## Revisit conditions

Only through a new capability-backed durable-delivery design with retention, encryption-at-rest, quotas, and acknowledgement semantics.
