# Human client architecture — Model B

Status: architecture/design only. No human client implementation is included.

## Goal

Support a human-facing desktop/TUI/CLI client while preserving one profile-scoped PeerId shared with Claude and other local applications.

```text
                            remote network
                                 |
                                 v
                         PeerId / Noise identity
                                 |
                    +------------+-------------+
                    | profile transport daemon |
                    +----+---------------+-----+
                         |               |
                  EndpointRegistry   libp2p runtime
                    /          \
                   v            v
         endpoint=human      endpoint=claude
           IPC lease            IPC lease
              |                    |
       Human client UI       Claude bridge
```

The daemon remains the only owner of the private transport key, connections, discovery, GossipSub, direct protocols, and trust enforcement.

## Human-client process boundary

A human client has two conceptual surfaces:

### Data-plane session

Uses ordinary IPC v2:

- claims configured endpoint `human`;
- receives direct messages addressed to `human` or to a default route that resolves to `human`;
- joins/leaves broadcast channels;
- broadcasts;
- sends to `{peer, endpoint?}`;
- queries trusted-peer advertised endpoints;
- reads identity/status/peers;
- receives bounded transport events.

### Administrative/settings session

Trust changes, endpoint configuration, key rotation, daemon shutdown, bootstrap/discovery edits, and similarly sensitive actions use a separately capability-granted administrative IPC connection or `transportctl`-equivalent service.

A network message displayed in the human client must never automatically exercise administrative operations. UI actions that mutate trust/configuration require an explicit local user gesture and the admin capability path.

A single executable may host both UI surfaces, but the architecture treats them as separate authorities and separate IPC connections. A human app that keeps both sessions open consumes two of the profile's IPC client slots.

Identity recovery is intentionally **not** part of the administrative session: recovery words are private-key-equivalent and remain in the offline `transportctl identity backup/restore` path defined by ADR-0033.

## Human application model

Human-facing concepts live above transport:

```text
Contact {
  contact_id,          // application-local
  display_name,
  peer_id,
  endpoint_id,
  avatar?,
  verification?,
}

Conversation {
  peer_id,
  endpoint_id,
  local history,
  unread/read UI state,
}
```

The human client may persist conversation history it actually receives/sends. That database is application state and must not be confused with transport durability: peers sending while endpoint `human` is offline receive `no_route`; the daemon provides no offline mailbox.

## Direct conversation flow

Alice runs one PeerId with `human` and `claude` endpoints. Bob's human UI targets Alice's advertised `human` endpoint:

```text
Bob Human UI
  |
  | send(peer=Alice, endpoint=human, payload)
  v
Bob daemon
  |
  | DirectMessageV2
  | source_endpoint=human
  | destination_endpoint=human
  v
Alice daemon
  |
  | exact route
  v
Alice Human UI
```

Alice's Claude bridge does not receive that message.

If Bob instead targets `claude`, only Alice's active `claude` endpoint receives it.

## Peer-only direct flow

A caller may omit the remote endpoint:

```text
send(peer=Alice, endpoint=None, payload)
```

Alice's daemon resolves exactly one configured `default_direct_endpoint`. There is never implicit all-client fan-out.

For a human-primary profile it is reasonable to configure:

```text
default_direct_endpoint: human
```

This is operator configuration, not a global convention.

## Reply flow

For an inbound direct event at Alice `human`:

```text
source_peer = Bob
source_endpoint = human
destination_endpoint = human
```

The local reply route becomes:

```text
source = Alice/human
destination = Bob/human
```

If Alice's `human` IPC lease disappears, a stale local reply route fails; it never switches to `claude` or the profile default.

## Endpoint directory UX

The human UI can ask the daemon for a trusted peer's currently advertised endpoints:

```text
Peer Alice
  available routes:
    human
    claude
```

The transport returns only EndpointIds. The UI may map them to locally configured labels/icons, but those labels are application-owned.

Directory absence or stale data is normal. The user may enter an EndpointId manually; explicit send does not require directory discovery.

## Trust UX

Peer trust remains profile-level and distinct from endpoint routing.

A human UI may show:

```text
PeerId: 12D3...
Trusted: yes
Routes currently advertised: human, claude
```

It must not render `claude` as cryptographic evidence that the remote process is Claude, nor `human` as proof that a particular person is present.

Endpoint-specific inbound/outbound ACLs can narrow which trusted peers may use the human endpoint. They never bypass profile trust.

## Broadcast UX

Broadcast remains ChannelId-based. EndpointId is not placed into GossipSub envelopes merely because multiple local applications exist.

Each local IPC client owns its own join references. If human and Claude both join `project-alpha`, both receive broadcast events because both explicitly joined, not because they share a PeerId.

If only the human endpoint joins, Claude receives nothing. `channels.desired` can keep the network mesh warm but still does not imply local delivery or buffering.

## Attachments and richer chat

The generic transport continues to carry opaque payload bytes with optional `media_type`. A human client may define a separate application protocol, for example a versioned JSON/CBOR chat envelope for text, attachments, read markers, reactions, or contact cards. For first-party interoperability, `application-envelope-guidance.md` recommends a minimal unauthenticated `from_endpoint` broadcast hint, but it remains application data and must never be treated as transport-authenticated authorship.

That application protocol must remain above this project. In particular, this transport does not define:

- human display names;
- read receipts;
- typing state;
- attachment storage;
- message edit semantics;
- social graphs;
- group membership;
- application E2EE.

## Local endpoint lifecycle

1. profile daemon starts and loads endpoint configuration;
2. human UI connects to IPC v2 and requests endpoint `human`;
3. daemon validates configured/enabled endpoint and grants exclusive lease;
4. `human` becomes locally routable and, if configured, remotely advertised;
5. UI disconnect/restart removes lease immediately; EndpointId leases require negotiated IPC keepalive by default, so a half-open/wedged first-party client is closed and its lease released after bounded missed probes;
6. remote directory stops listing `human` after fresh query/cache expiry;
7. direct requests during downtime receive `no_route`;
8. reconnect obtains a new lease and routing resumes without changing PeerId.

## Multiple windows/processes

Endpoint ownership is exclusive. Multiple UI windows should normally share one local application process/service that owns endpoint `human` rather than each opening a transport lease.

If truly independent human clients are required, configure distinct endpoints (`human`, `human.secondary`) or revisit the exclusive-lease decision explicitly. The daemon does not silently duplicate direct events across them.

## Security considerations

### Endpoint squatting

Configured-only registration plus an exclusive lease prevents accidental arbitrary route creation. Same-user malware remains capable of attacking same-user IPC and is a residual local threat.

### Endpoint spoofing over the network

A remote `source_endpoint` is authenticated only as data sent by the authenticated PeerId. It is not a sub-key or role certificate.

### Endpoint enumeration

Remote listing is opt-in (`advertise: true`), trusted-peer-only, bounded, and returns no descriptive metadata.

### Confused deputy

Network content never triggers trust changes, endpoint configuration, identity rotation, or daemon shutdown automatically. Admin operations are separate IPC capabilities.

### Default-route confusion

The default endpoint is explicit profile configuration. It is never inferred from client connection order and never changes because another endpoint happens to be online.

## Suggested initial profile

```yaml
endpoints:
  default_direct_endpoint: human
  directory:
    enabled: true
  entries:
    - id: human
      enabled: true
      advertise: true
      allowed_client_kinds: [human-client]
      inbound: { policy: inherit-profile-trust }
      outbound: { policy: inherit-profile-trust }
    - id: claude
      enabled: true
      advertise: true
      allowed_client_kinds: [claude-channel]
      inbound: { policy: inherit-profile-trust }
      outbound: { policy: inherit-profile-trust }
```

`allowed_client_kinds` is for configuration hygiene, not strong local authentication.

## Human-client acceptance criteria

- human and Claude share one PeerId without duplicate direct delivery;
- remote can explicitly address either endpoint;
- peer-only direct traffic resolves the configured default only;
- endpoint directory shows only active advertised routes;
- human endpoint restart does not rotate PeerId;
- offline endpoint creates no daemon-side backlog;
- human app can persist local history without changing transport delivery claims;
- admin actions require separate capability path and explicit local action;
- broadcast delivery still follows each client's join state;
- endpoint names never become asserted person/application identities in transport diagnostics or UI security indicators.
