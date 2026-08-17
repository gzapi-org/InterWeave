# Human-client message retention contract

Status: **Frozen first-party application contract; transport remains non-durable.**

This contract defines which HumanChatV2 message content may exist in durable first-party human-client storage on desktop and Android. It does not add a transport mailbox, daemon spool, network delivery guarantee, read receipt, or cross-device synchronization.

## 1. Core invariant

Message content is durable only in these states:

| Direction / state | Durable application content? |
|---|---:|
| outbound, not yet transport-terminal | **YES** |
| outbound, transport-terminal | **NO** |
| inbound, unread | **YES** |
| inbound, read and not explicitly kept by the receiver | **NO** |
| inbound, read and explicitly kept by the receiver | **YES** |

For direct sends, `AcceptedV2` is the transport-terminal event for retention. For broadcast sends, successful local `broadcast()` publication is the transport-terminal event because broadcast has no per-recipient acknowledgement. The UI must not call broadcast publication "delivered to recipients".

A message that is not durable may remain in bounded process memory for the current human-client session so the current conversation can render it. It is not serialized during shutdown and may disappear on crash/process death.

## 2. Outbound state machine

When the human chooses Send, the application first creates a durable pending-outbound record and only then invokes transport:

```text
compose
  |
  v
PENDING_OUTBOUND (durable)
  |
  +-- transient/ambiguous failure --> remains durable
  |
  +-- user cancels/deletes --------> delete durable record
  |
  `-- transport-terminal ----------> delete durable record
                                      |
                                      `--> optional RAM-only rendering until session ends
```

The durable pending record preserves the destination selector, HumanChatV2 `app_message_id`, payload/media type, creation time, and retry state needed by the application. It does not live in `TransportRuntime`, IPC, the daemon, peer cache, Kademlia, relay state, or endpoint queues.

A retry reuses the same HumanChatV2 `app_message_id` and resends the stored byte-identical payload (ADR-0050). Transport-level retry/idempotency continues to follow the frozen direct-message contract and its bounded dedup window; the human client must not claim exactly-once application delivery.

Pending outbound content is **not eligible for cross-device/system backup** in standard v1. Its purpose is local crash/restart survival, not delayed delivery from another device or restored image.

## 3. Inbound state machine

After the human client consumes a valid inbound application event, it commits the message as unread before normal UI presentation/notification:

```text
inbound event consumed by human client
  |
  v
UNREAD_INBOUND (durable)
  |
  v
locally read/presented under UI read policy
  |
  +-- receiver explicitly chooses Keep --> KEPT_INBOUND (durable)
  |
  `-- no Keep --------------------------> READ_EPHEMERAL (durable copy deleted)
```

`read` is a local UI state only. It does not generate a network read receipt and does not prove the human actually perceived the content.

The receiver may mark an inbound message `Keep` **only after it has entered the local read state**. Remote payload fields, EndpointId names, contacts, senders, notification actions, or application envelope fields can never force durable retention.

If a read-but-unkept message is still visible in the current process, a later explicit `Keep` action may write it back as `KEPT_INBOUND`. If the process exits before that action, the content is gone by design.

If the receiver removes `Keep` from a kept message, its durable content is deleted immediately. It may remain RAM-only for the current session.

## 4. Acceptance boundary and crash honesty

Transport direct `AcceptedV2` still means only that the remote endpoint queue accepted the event. It does **not** mean the human application committed the unread record.

Therefore there is a bounded handoff window between transport endpoint-queue acceptance and first-party human-store commit. The human client must consume and persist inbound events promptly, but it must not strengthen the network delivery claim. If the process/storage fails in that window, an already transport-accepted message can still be lost.

If the durable human store becomes unavailable or cannot accept new unread content, the first-party client must surface storage degradation and stop presenting itself as a healthy durable human receiver. It should release/disable the human direct endpoint **and suspend its local human broadcast joins/delivery** until storage capacity is restored rather than knowingly accepting an unbounded stream it cannot retain. Profile-level `channels.desired` may keep the GossipSub mesh warm with no local human consumer, preserving the existing no-buffer rule. This application reaction does not alter transport protocol semantics.

## 5. Durable store contents

The message-content store contains only:

```text
pending_outbound
unread_inbound
kept_inbound
```

There is no permanent general conversation-history table containing delivered outbound plus every received message.

Content-free application metadata may be stored separately when needed for indexes, contact routing, migration, diagnostics, bounded duplicate suppression, or UI preferences. Such metadata must not be sufficient to reconstruct deleted message bodies.

## 6. Backup eligibility

Android system backup/device transfer remains disabled for the human store.

A future **explicit, user-selected encrypted application backup** may include received message content only when:

```text
direction == inbound
AND (state == unread OR state == kept)
```

Standard-v1 backup must not include:

- pending outbound content;
- transport-terminal outbound content;
- read-but-unkept inbound content;
- RAM-only current-session history;
- notification cache/previews as a substitute message archive.

The reason pending outbound is excluded from portable backup is to avoid turning a restored/second device into an implicit delayed-send or replay source. A future portable outbox requires its own acknowledgement/replay design.

## 7. Shutdown and restart

On normal shutdown, crash, Android process death, or desktop UI termination:

Survives:

- pending outbound;
- unread inbound already committed to the human store;
- receiver-kept inbound;
- non-message application state allowed by its own policy.

Evaporates:

- transport-terminal outbound content;
- read-but-unkept inbound content;
- RAM-only rendered conversation content;
- temporary notification/rendering copies.

Restart reconstructs the visible conversation only from the surviving sets plus new live events. It does not ask the daemon/network for historical replay.

## 8. Security/privacy invariants

- A remote sender cannot request or force `Keep`.
- `Keep` is a local receiver action after read.
- Read state is never sent remotely in v1.
- Pending outbound storage is not a transport mailbox and does not grant offline reachability to the receiver.
- Deletion of durable message content must remove the application record and any application-owned plaintext indexes/caches that would reconstruct it; storage-media forensic guarantees depend on the selected database/filesystem/encryption layer and must not be overstated.
- Logs, analytics, crash reports, notification databases, OS backup, and search indexes must not become shadow message archives.

## 9. Conformance cases

The desktop and Android human clients must share tests proving:

1. outbound message is committed before first send attempt;
2. direct `AcceptedV2` deletes its durable pending copy;
3. failed/no-route/timeout outbound remains pending unless explicitly cancelled;
4. successful broadcast publication deletes its pending copy without rendering a false recipient-delivery claim;
5. inbound is committed unread before normal UI presentation/notification;
6. unread inbound survives process restart;
7. marking inbound read without Keep deletes durable content;
8. receiver Keep after read makes that inbound message durable;
9. sender/application payload cannot set Keep;
10. removing Keep deletes durable content;
11. transport-terminal outbound and read-unkept inbound disappear across restart;
12. explicit backup eligibility includes only unread/kept inbound message content and excludes pending outbound;
13. Android system backup/device transfer contains none of the human message-content store;
14. storage unavailable/full transitions the human endpoint to degraded/offline handling rather than silently claiming unread durability.
