# First-party human client — cross-platform architecture

Status: the shared crates and the desktop client are built (Stages 14 and 15, closed 2026-10-03 and 2026-10-06; `apps/human-desktop` over `crates/human/*`); the Android binding is Stage 17's, open as of 2026-10-07, with `apps/human-android` and `crates/human/android-platform` as landing zones. The design below is the one the build followed; where the built layout differs from the first blueprint, the built one is named.

## Selected product architecture

The first-party human client is a Rust application family with a shared Rust domain/storage/UI-model core and two deployment bindings:

```text
                         shared Rust human client
        +------------------------+-------------------------+
        |                        |                         |
        v                        v                         v
   human-core               human-store           ui-model + ui-slint
 retention state          SQLite application DB    presentation model and
 machine, validation      migrations/indexes       the reference Slint views
        |                        |                         |
        +------------------------+-------------------------+
                                 |
                    +------------+-------------+
                    |                          |
                    v                          v
             Desktop binding              Android binding
             daemon + IPC v2          embedded TransportRuntime
```

The network wire protocols, PeerId/EndpointId model, trust policy, Kademlia, GossipSub, DirectMessageV2, endpoint directory, AutoNAT v2, Relay v2, and DCUtR are identical on both platforms.

## Rust/UI selection

The reference first-party UI is **Slint with Rust** so desktop and Android can share UI components and Rust view models while retaining platform-specific layouts. This is a first-party implementation decision, not a transport requirement: IPC/network protocols remain language-neutral.

Application/business/network logic is Rust. Android may contain the minimum Java/Kotlin/JNI component glue required by Android OS services, notifications, Keystore/Biometric APIs, and lifecycle callbacks. No trust, routing, cryptography, message parsing, persistence schema, or network policy is implemented in that shim.

## Shared crate blueprint

```text
crates/human/core/               # NO libp2p; the retention state machine, validation
crates/human/chat-protocol/      # HumanChatV2 envelope, subset validator, bounded decoder, render model
crates/human/store/              # application SQLite model/migrations
crates/human/client-api/         # the neutral client-facing port the facade implements
crates/human/transport-client/   # the LocalDataSession facade: outbox, inbox, re-open, retention
crates/human/ui-model/           # presentation state, NO OS/network backend
crates/human/app-core/           # the headless application root the windows bind to
crates/human/ui-slint/           # the reference Slint views; the only crate naming slint
crates/human/android-platform/   # tiny OS glue surface only (Stage 17; a landing zone today)
apps/human-desktop/              # the desktop executable: app-core + ui-slint over ipc-client
apps/human-android/              # the Android host (Stage 17; a landing zone today)
```

`core`, `chat-protocol`, `store`, `transport-client` and `ui-model` name nothing under `crates/transport/*`, no libp2p and no Slint, and `ui-slint` is the only crate under `crates/human/*` that reaches Slint and the only workspace member that declares it (`apps/human-desktop` reaches it through `ui-slint`); `tools/checks/check_human_layering.sh` enforces it (built as the first blueprint's `human-*` names with the directory layout of `implementation-repository-layout.md`).

## Human application state

The human client owns application state independently from transport. Message content follows ADR-0044 rather than a permanent history model:

```text
Contact
  contact_id
  local display name/avatar
  device routes[] -> {peer_id, endpoint_id, local label, verification notes}

ConversationIndex
  conversation_id
  route -> {peer_id, endpoint_id}
  unread count / last activity

PendingOutbound        # durable until transport-terminal
UnreadInbound          # durable until read
KeptInbound            # durable only after receiver Keep post-read
CurrentSessionMessage  # RAM-only terminal/read-unkept rendering
```

Transport keys, trust configuration, Kademlia state, relay reservations, EndpointId leases, and remote endpoint-directory caches never live in the human application database.

The same retention state machine is implemented on desktop and Android: pending outbound and unread inbound survive restart; inbound read without receiver Keep and transport-terminal outbound evaporate. A future portable message backup may contain only unread/kept inbound content, never pending outbound.

## First-party chat envelope

The human clients need one interoperable application convention while keeping it above transport. `clients/human/HUMAN-CHAT.md` defines `HumanChatV2`: markdown text/reply metadata with a size-triggered bounded compression fallback (ADR-0050). It does not alter DirectMessageV2 or GossipSub.

For direct messages, authenticated transport metadata (`source_peer`, peer-asserted `source_endpoint`) is authoritative for routing display. Application fields never override it. For broadcasts, an application `from_endpoint` hint is explicitly unauthenticated because the broadcast transport remains PeerId/channel scoped.

## Security/UI rules

Incoming text is untrusted content. The UI must not:

- render active HTML/JavaScript;
- execute links automatically;
- auto-open attachments/files;
- translate remote endpoint labels into authority claims;
- mutate trust/configuration because a message asks it to;
- expose recovery words in normal message/admin channels.

Links require an explicit local click and OS handoff. Rich preview/network fetching is opt-in and privacy-sensitive.

## Multi-device rule

A concurrently active desktop and Android device use **different profile PeerIds by default**. A BIP-39 recovery phrase is disaster recovery/migration for one transport identity, not an account-sync seed to clone onto multiple simultaneously active nodes.

The human contact model may locally group several device routes under one person, but this grouping is application metadata, not transport-authenticated human identity. A future signed multi-device identity/account protocol requires a separate ADR/application protocol.

## Cross-platform acceptance criteria

- same `human-core` validation/storage/ADR-0044 retention semantics on desktop and Android;
- same network wire fixtures on both platforms;
- direct source endpoint is session-derived in both deployment modes;
- human UI can send/receive direct and broadcast traffic without libp2p concepts;
- platform process/lifecycle loss never creates hidden transport durability; only ADR-0044 application retention survives;
- recovery/config separation remains unchanged;
- desktop and Android can exchange `HumanChatV2` text using ordinary DirectMessageV2/GossipSub payloads.

## Detailed first-party design references

- UI and interaction semantics: [`human-client-ui.md`](./human-client-ui.md)
- platform packaging/lifecycle: [`human-client-packaging.md`](./human-client-packaging.md)
- desktop binding: [`human-client-desktop.md`](./human-client-desktop.md)
- Android binding: [`human-client-android.md`](./human-client-android.md)
- human application state: [`../../clients/human/STATE.md`](../../clients/human/STATE.md)
- HumanChatV2: [`../../clients/human/HUMAN-CHAT.md`](../../clients/human/HUMAN-CHAT.md)
- message retention: [`../../clients/human/RETENTION.md`](../../clients/human/RETENTION.md)
