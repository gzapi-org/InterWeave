# Human client is an IPC application above the shared transport daemon

**Status:** Accepted

## Context

Model B permits a human-facing client and Claude Code to share one profile PeerId while remaining independently addressable local applications. The architecture must decide whether a human client embeds its own libp2p stack, whether human chat concepts enter the generic transport, and whether UI data-plane traffic shares authority with trust/key/endpoint administration.

## Decision

A human-facing desktop, TUI, CLI, or background messaging client is an **application above transport**, attached to the existing profile daemon through IPC v2.

- its data-plane connection claims one configured EndpointId such as `human`;
- it uses the same generic `join`, `leave`, `broadcast`, `send`, endpoint-directory query, identity/status, and event contracts as other local consumers;
- it does not own a libp2p Swarm, private PeerId key, discovery provider, GossipSub mesh, direct protocol implementation, or Kademlia behavior;
- contacts, display names, avatars, verification state, conversation models, unread state, reactions, attachment UX, and persisted local history are application-level data above transport;
- application-local history may persist messages actually sent/received by the client, but does not create transport offline delivery or a daemon mailbox;
- trust mutation, endpoint configuration, identity/key operations, discovery/bootstrap administration, and daemon shutdown require a separately capability-authorized administrative IPC connection or `transportctl`-equivalent path;
- network message content can never automatically invoke those administrative capabilities;
- the human UI must display EndpointId as a remote peer-controlled route label unless a higher-level application identity system separately verifies a stronger binding.

The same executable may host both UI surfaces, but the architecture treats data-plane and administrative authority as separate IPC sessions/capabilities.

## Alternatives considered

Embed rust-libp2p directly in the human client; give the human client a separate mandatory PeerId/profile (Model A); add human/chat semantics to the transport contract; let a single unrestricted IPC connection perform both messaging and administration; infer a human identity from `PeerId + EndpointId`.

## Consequences

Human and Claude clients can share connections, discovery state, trust policy, and one persistent network identity while receiving deterministic direct traffic through different EndpointIds. Human-client restart does not rotate PeerId or tear down network connectivity. A human application may evolve its presentation/data model independently from the transport protocol.

The transport does not provide human-specific durable history, social identity, read receipts, typing indicators, group membership, or attachment storage. Those features require an application protocol/storage layer if later desired.

## Security implications

The private transport key remains daemon-owned. Data-plane UI compromise does not automatically grant transport administration when capability separation is correctly enforced. Same-OS-user malicious code remains a residual IPC threat as documented elsewhere.

A route label such as `human` is not an authentication factor. Security decisions continue to use PeerId trust and explicit local administrative authority, not endpoint naming.

## Operational implications

Operators configure a human EndpointId and optionally make it the default direct endpoint. Multiple UI windows should normally share one human application process/service that owns the lease; independent clients need distinct configured EndpointIds. Human app databases are backed up/managed separately from daemon identity/config/cache state.

## Implementation implications

Provide an IPC v2 client library suitable for human applications, endpoint-directory/status APIs, endpoint-aware direct events, and a separate administrative adapter/session. A future GUI/TUI implementation may use any presentation toolkit without creating dependencies from transport crates back into the UI.

Testing must cover human+Claude same-profile routing, human restart/lease recovery, no daemon backlog while human is offline, separation of admin capabilities, and correct presentation of PeerId/EndpointId identity boundaries.

## Revisit conditions

Revisit if a deployment requires the human client to operate without the daemon, needs independently portable network identity, requires cryptographic endpoint sub-identities, introduces durable network mailboxes, or proves that OS-level separation requires a stronger local authentication mechanism than owner-only IPC/capability policy.
