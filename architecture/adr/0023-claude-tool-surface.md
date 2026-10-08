# Minimal Claude-facing transport tool surface

**Status:** Accepted; direct send extended by ADR-0030.

## Context

Claude needs enough operations to participate in the Channel without being exposed to libp2p internals or local administration. Endpoint-addressed profiles add one necessary direct-routing parameter but do not justify exposing endpoint configuration/trust administration through the Channel.

## Decision

Expose conceptual tools:

- `broadcast(channel, content, content_type?)`;
- `send(peer, endpoint?, content, content_type?)`;
- `reply(reply_token, content, content_type?)`;
- `join(channel)`;
- `leave(channel)`;
- `identity()`;
- `status()`;
- `receive(max?)` — in PULL delivery mode only (ADR-0002 A 2026-10-07): takes the direct messages and broadcasts the bridge holds for the session, never waits, one structured result `{events, remaining, paused}`; absent in push mode, where the Channel notification delivers (A 2026-10-07).

The bridge itself owns one configured EndpointId lease over IPC v2. `send.endpoint` selects the **remote** endpoint; source endpoint always comes from the bridge lease. Omitting remote endpoint asks the remote profile to use its configured default endpoint.

`reply` uses route metadata captured from the inbound event, including direct remote source endpoint and this bridge's local lease epoch.

`status` includes local profile PeerId, this bridge's EndpointId/lease health, bridge-owned joined channels, profile desired channels, high-level transport health, and in pull mode the pull queue's depth, whether the drain is paused and since when (A 2026-10-07); while the drain is paused, `send`, `reply`, `broadcast`, `join` and `leave` refuse at once with a bridge-local error naming the state, never a stall (A 2026-10-07).

Do not expose trust approval/revocation, endpoint creation/rebinding, identity rotation/recovery, daemon shutdown, forced discovery/Kademlia queries, private keys, or raw Swarm/multiaddr internals as Channel tools. **`peer_endpoints` is deliberately not a Claude-facing v2 tool and `claude-channel` is not granted `endpoints.query` by default.**

## Alternatives considered

Expose every daemon operation; hide endpoint destination inside payload conventions; add an endpoint-administration tool to Claude; require explicit endpoint on every direct send with no remote default.

## Consequences

Claude can address a human or another local service under one PeerId without learning libp2p internals. The surface remains compact — seven tools in push mode, eight in pull mode, where the one addition is the delivery itself (A 2026-10-07); endpoint is an optional parameter rather than a new tool.

## Security implications

Remote content cannot mutate local endpoint/trust configuration. Source endpoint cannot be spoofed by tool input. `source_endpoint` metadata from a remote peer is routing metadata, not application/human identity proof.

## Operational implications

A bridge that cannot obtain its configured EndpointId lease remains usable for non-direct diagnostics/broadcast as policy allows, but direct send/reply reports endpoint lease failure clearly.

## Implementation implications

Bridge handshake includes configured endpoint claim. Tool schemas add optional remote `endpoint` to `send`. Reply-token storage includes remote source endpoint, local destination endpoint, and lease epoch.

## Revisit conditions

Revisit `peer_endpoints` only if a concrete user workflow requires Claude to enumerate remote endpoint directories **and** a separate tool-surface/security review approves granting `endpoints.query`; also revisit if richer application-specific service discovery is intentionally added above transport.

## Amendments

Full notes: [`history/0023-amendments.md`](./history/0023-amendments.md).

| Date | Amendment | Effect |
|---|---|---|
| 2026-10-07 | `receive` is the eighth tool, in pull delivery mode only | Decision: `receive(max?)` takes the direct messages and broadcasts the bridge holds for the session, never waits, one structured result `{events, remaining, paused}`, absent in push mode; `status` reports the pull queue's depth and whether the drain is paused and since when; while paused the five session-bound tools refuse at once with a bridge-local error; the surface is seven tools in push mode and eight in pull mode (ADR-0002 A 2026-10-07, `CHANNEL-EVENT.md` §Delivery, `TOOL-SURFACE.md`). |

