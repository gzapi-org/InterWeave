# Human client — shared UI and interaction design

Status: architecture/design only. This document defines first-party interaction semantics, not a transport protocol.

## 1. Information architecture

The desktop and Android clients expose the same conceptual areas with platform-appropriate navigation:

```text
Chats
Channels
Contacts / Routes
Network
Settings
  +-- Profile / PeerId
  +-- Trust
  +-- Endpoints
  +-- Connectivity infrastructure
  +-- Recovery / device security
  `-- Diagnostics
```

Desktop may use multi-pane navigation; Android uses mobile navigation and one-primary-task screens. The shared Rust UI model exposes the same state/actions without requiring identical layouts.

## 2. Onboarding

First-run onboarding is explicit about transport identity:

1. create a new profile identity, or enter the dedicated recovery flow;
2. show the resulting local PeerId as a machine/device identity, never a human account name;
3. configure or import trust/bootstrap/connectivity settings separately from identity recovery;
4. create/enable the local `human` EndpointId;
5. explain direct-vs-relayed reachability without exposing infrastructure peers as contacts;
6. optionally join channels and import contact routes.

A new identity is not silently created when an established profile fails to unlock. Recovery and configuration restore remain separate operations.

## 3. Contacts and routes

A contact is application metadata and may contain multiple device routes:

```text
Alice
  Desktop  -> PeerId A / human
  Phone    -> PeerId B / human
```

UI labels such as `Alice`, `phone`, `human`, avatars, and notes are **not authenticated by transport**. Trust indicators are tied to the exact PeerId. Adding a second route to an existing contact does not inherit trust from the first route.

When endpoint-directory lookup is used, returned EndpointIds are displayed as remote-advertised routing labels, not verified application roles. Unknown/offline/policy-denied remote routes remain intentionally indistinguishable at the transport error layer.

## 4. Conversations

A direct conversation is keyed locally by the selected remote route `{PeerId, EndpointId?}`. A channel conversation is keyed by ChannelId. Human-readable titles can be changed without altering transport addresses.

The first-party text payload uses `HumanChatV2`. Text is markdown rendered inside the closed subset of ADR-0050 — raw HTML shown literally, allowlisted link schemes, no automatic image fetch, raw source always viewable. Remote content cannot trigger trust/config changes, commands, link opening, downloads, recovery operations, or admin actions.

## 5. Message-status language

The UI must not claim stronger delivery semantics than transport provides.

Allowed states include:

```text
Sending
Accepted by remote transport
Failed / timed out
Received locally
```

Do not label `AcceptedV2` as `Read`, `Seen`, `Processed by person`, or even stronger application delivery unless a future application-level receipt protocol supplies that evidence.

For broadcast, current-session UI may show that publication was accepted by the local transport; it cannot claim every subscriber received the message. Durable pending content is removed on successful publication per ADR-0044.

## 6. Message retention interaction

The human UI implements [`clients/human/RETENTION.md`](../../clients/human/RETENTION.md):

- outbound pending messages survive restart until direct `AcceptedV2`, successful broadcast publication, explicit cancel/delete, or a future separately designed terminal policy;
- inbound unread messages survive restart;
- when an inbound message enters local read state, its durable unread copy is removed unless the receiver explicitly chooses **Keep**;
- **Keep** is available only after local read state and is always a receiver-local action;
- read-but-unkept inbound and transport-terminal outbound messages may remain visible in RAM for the current session, but they disappear across app/process restart;
- removing Keep deletes the durable kept copy;
- no remote payload, EndpointId, sender label, notification action, or application extension may force Keep.

The UI should distinguish `Unread` from `Kept`. Unread is temporary durability for user availability; Kept is an explicit post-read retention choice. Neither state is transmitted to the sender in v1.

## 7. Connectivity display

Ordinary users see normalized status only:

```text
Online — direct reachable
Online — relay available
Online — outbound/partial reachability
Offline / transport stopped
```

An established peer route may optionally show `direct` or `relayed`. A DCUtR `PeerPathChanged` updates the route indicator without creating a fake reconnect/new-message event.

Infrastructure PeerIds, AutoNAT probe details, relay multiaddrs, Kademlia buckets, and raw failure traces belong to advanced diagnostics/admin surfaces.

## 8. Trust/settings interaction

Trust mutation is always an explicit local settings action. A message may contain a PeerId as text, but clicking/copying it cannot auto-allowlist it.

Desktop settings use the admin IPC binding. Android settings use `LocalAdminPort`. Network/event callbacks never receive either authority.

High-impact changes require a confirmation view that shows the exact PeerId and scope. Removing trust should warn that active application connectivity to that peer will be evicted.

## 9. Recovery UX

### Desktop

Standard v1 keeps mnemonic backup/restore in the stopped-runtime `transportctl identity ...` workflow. The human UI may open documentation or guide the operator to the tool but does not obtain mnemonic words through daemon IPC.

### Android

There is no standalone CLI requirement. A dedicated local recovery flow stops/locks the TransportRuntime, invokes the identity component directly, and may display/import the 24 words on-device. It is not routed through `LocalDataSession` or `LocalAdminPort`. During the complete recovery flow a dedicated non-exported Android recovery Activity is excluded from Recents and uses `FLAG_SECURE` before any phrase material is rendered. The phrase must not enter screenshots/screen recording/task snapshots, clipboard, autofill, analytics, logs, saved-instance state or normal free-text IME input. Import uses the in-app BIP-39 word-list picker defined by the Android custody design; any temporary IME-assisted filtering requests no suggestions/autocorrect and `IME_FLAG_NO_PERSONALIZED_LEARNING`, but that request is not treated as a guarantee that an arbitrary IME behaves correctly. Exact platform behavior is validated in SPIKE-008/009 and release testing.

Both platforms state prominently: complete profile disaster recovery requires the recovery phrase **and** separately backed-up configuration/trust/endpoint settings.

## 10. Notifications

Notification content is generated only from messages already accepted into the human application/session.

- notification previews are user-configurable;
- sensitive previews may be hidden on lock screens;
- a notification tap opens local conversation context only;
- notification action buttons, if added later, must not perform trust/admin/recovery operations;
- a notification never proves the sender is a human merely because the route endpoint is named `human`.

Desktop notification integration is optional per OS packaging. Android stay-reachable mode uses its foreground-service notification independently from per-message notifications.

## 11. Accessibility and localization

First-party UI must provide semantic labels/actions, keyboard navigation on desktop, screen-reader-friendly controls, scalable text, sufficient touch targets on Android, and localization-safe layouts. PeerIds and cryptographic identifiers must remain copyable in exact canonical form even when surrounding UI is localized.

## 12. Error presentation

Map stable transport/local errors to user-actionable messages without exposing hidden route-policy distinctions. Examples:

- `UnauthorizedPeer` -> peer is not trusted for this profile;
- `RemoteEndpointUnavailable`/wire `no_route` -> selected route is currently unavailable;
- `PeerUnreachable` -> no usable network path;
- `Overloaded` -> remote/local transport is temporarily busy;
- `EndpointInUse` -> this local endpoint is already owned by another client/session.

Raw internal codes remain available in diagnostics.

## 13. Shared UI acceptance tests

- no UI label promotes EndpointId/display name to authenticated identity;
- AcceptedV2 never renders as read/seen;
- DCUtR path change does not create a duplicate logical connection/conversation event;
- explicit trust mutation shows exact target PeerId;
- remote text cannot invoke admin/recovery handlers;
- desktop and Android render the same HumanChatV2 fixture consistently;
- pending outbound and unread inbound survive restart; read-unkept/transport-terminal messages do not;
- receiver Keep can be set only after read and cannot be forced by remote content;
- accessibility tree contains meaningful labels/actions for message, route, trust, and connectivity controls.

## 14. Android availability-policy diagnostic

When Android is configured with both `availability_mode=stay-reachable` and `key_unlock_policy=user-presence`, the configuration is valid but the UI/status model must expose `background_restart_requires_user_authentication=true`. User-facing copy must explain that the endpoint can stay reachable while the currently unlocked service remains alive, but it cannot automatically recover reachability after process/service restart until the user authenticates.

## 15. HumanChat reply rendering

An inbound HumanChatV2 `reply_to` may reference an application message that is not present in the current retention/session store. The message is still valid and must render normally; the client may show a neutral `Referenced message unavailable` placeholder and preserve the referenced ID for bounded diagnostics/retention metadata. It must not auto-fetch, create transport traffic, reject the message, or infer tampering solely because the referenced message is absent locally.
