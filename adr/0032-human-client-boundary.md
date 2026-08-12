# Human client remains above transport; desktop uses IPC, Android uses an embedded local-session binding

**Status:** Accepted

## Context

Model B permits a human-facing client and Claude Code to share one profile PeerId while remaining independently addressable local applications. The architecture must decide whether a human client embeds its own libp2p stack, whether human chat concepts enter the generic transport, and whether UI data-plane traffic shares authority with trust/key/endpoint administration.

## Decision

A human-facing client is an **application above transport**. Desktop/TUI/CLI uses the existing profile daemon through IPC v2. Android is amended by ADR-0041: the same `TransportRuntime` is embedded in a foreground-service host and exposed through the neutral `LOCAL-CLIENT` in-process adapter rather than a standalone daemon.

- its local data-plane session claims/owns one configured EndpointId such as `human` (IPC connection on desktop; embedded service session on Android);
- it uses the same generic `join`, `leave`, `broadcast`, `send`, endpoint-directory query, identity/status, and event contracts as other local consumers; its data-plane kind receives `endpoints.query` by default when directory support is enabled, while Claude Channel does not;
- the human **UI/domain layer** does not own libp2p policy or private-key handling. Desktop delegates those to the daemon; Android packages the same Rust runtime/key owner in the foreground-service host below the UI;
- contacts, display names, avatars, verification state, conversation models, unread state, reactions, attachment UX, and persisted local history are application-level data above transport;
- application-local history may persist messages actually sent/received by the client, but does not create transport offline delivery or a daemon mailbox;
- trust mutation, endpoint configuration, discovery/bootstrap administration, and shutdown require the platform admin binding (desktop admin socket; Android `LocalAdminPort`); identity backup/restore remains a stopped-runtime/offline identity path. A data-plane session can never self-upgrade to admin authority;
- network message content can never automatically invoke those administrative capabilities;
- the human UI must display EndpointId as a remote peer-controlled route label unless a higher-level application identity system separately verifies a stronger binding.

On desktop, one executable may host both surfaces but they are separate IPC connections/authority domains and consume separate IPC slots. On Android, the same APK process contains distinct `LocalDataSession` and `LocalAdminPort` objects; that split prevents confused-deputy wiring but is not an OS sandbox against arbitrary same-process compromise. Identity recovery phrase export/restore is more sensitive still and remains stopped-runtime/offline rather than either data-plane/admin session.

## Alternatives considered

Embed a separate independent rust-libp2p stack in the human UI; give the human client a separate mandatory PeerId/profile (Model A); add human/chat semantics to the transport contract; let a single unrestricted IPC connection perform both messaging and administration; infer a human identity from `PeerId + EndpointId`.

## Consequences

On desktop, human and Claude clients can share connections, discovery state, trust policy, and one persistent network identity while receiving deterministic direct traffic through different EndpointIds. On Android, the human app owns its device profile/runtime; concurrently active devices use distinct PeerIds per ADR-0043. Activity restart does not rotate PeerId; service/process restart rebuilds network state using the same profile key. A human application may evolve its presentation/data model independently from the transport protocol.

The transport does not provide human-specific durable history, social identity, read receipts, typing indicators, group membership, or attachment storage. Those features require an application protocol/storage layer if later desired.

## Security implications

Desktop private transport key remains daemon-owned. Android key ownership sits in the embedded transport service below the UI and is protected at rest per ADR-0042. Desktop data-plane UI cannot obtain `admin.*` from the data socket; Android message/event code is constructed without `LocalAdminPort`. Default same-UID desktop admin-socket access and same-process Android compromise remain documented local residuals.

A route label such as `human` is not an authentication factor. Security decisions continue to use PeerId trust and explicit local administrative authority, not endpoint naming.

## Operational implications

Operators configure a human EndpointId and optionally make it the default direct endpoint. Multiple UI windows should normally share one human application process/service that owns the lease; independent clients need distinct configured EndpointIds. Human app databases are backed up/managed separately from daemon identity/config/cache state.

## Implementation implications

Provide a neutral local-client/session facade plus IPC v2 adapter for desktop and in-process adapter for Android, endpoint-directory/status APIs, endpoint-aware direct events, and separate administrative authority. ADR-0039 selects Rust + Slint for the first-party UI while preserving transport independence.

Testing must cover human+Claude same-profile routing, human restart/lease recovery, no daemon backlog while human is offline, separation of admin capabilities, and correct presentation of PeerId/EndpointId identity boundaries.

## Revisit conditions

Revisit if another desktop deployment requires operation without the daemon, needs independently portable network identity, requires cryptographic endpoint sub-identities, introduces durable network mailboxes, or proves that OS-level separation requires a stronger local authentication mechanism than owner-only IPC/capability policy.
