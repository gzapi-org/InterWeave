# Human message retention amendment review — 2026-08-12

Status: architecture closure memo.

## Requested behavior

The human client must retain only the message states needed for unfinished sending, unread availability, or an explicit receiver decision after reading. Ordinary delivered/read conversation history must evaporate across app closure.

## Frozen result

| State | Durable? | Portable message backup? |
|---|---:|---:|
| outbound pending / retrying | yes | no |
| outbound direct `AcceptedV2` | no | no |
| outbound broadcast locally published | no | no |
| inbound unread | yes | future explicit encrypted backup: yes |
| inbound read, no Keep | no | no |
| inbound read, receiver Keep | yes | future explicit encrypted backup: yes |

Receiver Keep is possible only after local read state and cannot be requested by remote content. Read/Keep is local-only in v1 and creates no receipt.

## Boundary audit

- `TransportRuntime`/daemon remains non-durable and never accepts traffic for an offline EndpointId.
- `AcceptedV2` remains endpoint-queue admission, not human-store commit.
- the human application commits consumed inbound content unread before normal presentation/notification, while acknowledging the small transport-acceptance-to-store handoff window;
- outbound is persisted before first send attempt;
- Android system backup/device transfer still excludes the entire human-store;
- pending outbound is excluded from future portable message backup to avoid restored/second-device replay;
- shutdown/restart reconstructs only pending outbound, unread inbound, receiver-kept inbound and separately permitted non-message state;
- logs/notifications/search indexes/analytics must not become shadow archives.

## Supersession/clarification

ADR-0020 remains authoritative for the transport/runtime no-offline-store rule. ADR-0044 introduces the narrowly scoped first-party **application** retention state above that boundary. The previous prose allowing a conventional persistent human local history is superseded.
