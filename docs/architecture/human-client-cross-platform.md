# First-party human client — cross-platform architecture

Status: architecture/design only. No client implementation is included.

## Selected product architecture

The first-party human client is a Rust application family with a shared Rust domain/storage/UI-model core and two deployment bindings:

```text
                         shared Rust human client
        +------------------------+-------------------------+
        |                        |                         |
        v                        v                         v
   human-core               human-store               human-ui
 contacts/routes          SQLite application DB       Slint UI model
 conversations            migrations/indexes          shared components
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
human-core/                 # NO libp2p; contacts, conversations, commands, validation
human-chat-protocol/        # first-party application envelope; NO transport internals
human-store/                # application SQLite model/migrations
human-ui-model/             # presentation state, NO OS/network backend
human-ui-slint/             # shared Slint components
human-transport-client/     # neutral LocalDataSession facade
human-desktop/              # Slint desktop executable + IPC adapter
human-android/              # Android Rust cdylib/Slint entry + embedded adapter
android-platform-bridge/    # tiny OS glue surface only; no domain/network logic
```

`human-core`, `human-chat-protocol`, `human-store`, and `human-ui-model` have no libp2p dependency.

## Human application state

The human client owns application state independently from transport:

```text
Contact
  contact_id
  local display name/avatar
  device routes[] -> {peer_id, endpoint_id, local label, verification notes}

Conversation
  conversation_id
  route -> {peer_id, endpoint_id}
  local message history
  unread/read state
  draft state

ObservedMessage
  application message id
  transport message id
  remote peer
  remote endpoint when direct
  channel when broadcast
  received/sent time
  application payload/render state
```

Transport keys, trust configuration, Kademlia state, relay reservations, EndpointId leases, and remote endpoint-directory caches never live in the human application database.

## First-party chat envelope

The human clients need one interoperable application convention while keeping it above transport. `clients/human/HUMAN-CHAT.md` defines `HumanChatV1` for text/reply metadata. It does not alter DirectMessageV2 or GossipSub.

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

- same `human-core` validation/storage semantics on desktop and Android;
- same network wire fixtures on both platforms;
- direct source endpoint is session-derived in both deployment modes;
- human UI can send/receive direct and broadcast traffic without libp2p concepts;
- platform process/lifecycle loss never creates hidden transport durability;
- recovery/config separation remains unchanged;
- desktop and Android can exchange `HumanChatV1` text using ordinary DirectMessageV2/GossipSub payloads.

## Detailed first-party design references

- UI and interaction semantics: [`human-client-ui.md`](./human-client-ui.md)
- platform packaging/lifecycle: [`human-client-packaging.md`](./human-client-packaging.md)
- desktop binding: [`human-client-desktop.md`](./human-client-desktop.md)
- Android binding: [`human-client-android.md`](./human-client-android.md)
- human application state: [`../../clients/human/STATE.md`](../../clients/human/STATE.md)
- HumanChatV1: [`../../clients/human/HUMAN-CHAT.md`](../../clients/human/HUMAN-CHAT.md)
