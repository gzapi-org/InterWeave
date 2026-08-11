# Minimal Claude-facing Channel surface

**Status:** Accepted

## Context

Claude needs explicit outbound route choices while normal inbound communication must be push-based. A small surface reduces prompt/tool complexity and prevents administrative controls from being remotely induced.

## Decision

Expose `broadcast`, `send`, `reply`, `join`, `leave`, `identity`, and `status` as the minimal conceptual MCP tools. Inbound data arrives only through Channel notifications. Detailed peers/discovery diagnostics belong primarily to local CLI/config UX; a bounded read-only diagnostic tool may be added only if justified.

`broadcast` requires the calling bridge to hold an active join reference for the channel. `send` is subject to the same v1 PeerTrustPolicy as inbound/data-plane admission and fails locally with `UnauthorizedPeer` for an untrusted destination. A broadcast `reply` after the bridge has left that channel fails with `ChannelNotJoined`; it never silently rejoins.

## Alternatives considered

poll tool for inbound messages; expose Swarm/multiaddr controls; trust-edit tools; mirror Telegram react/edit/file tools; permit outbound sends to arbitrary discovered PeerIds.

## Consequences

Some operational troubleshooting remains outside Claude and uses local CLI. Reply tokens simplify safe routing but do not confer membership/subscription authority.

## Security implications

No trust/key/config mutation tool is exposed to remote-triggered Claude flows. Outbound direct sends cannot bypass the profile's peer trust boundary. Broadcast cannot silently expand local subscription state.

## Operational implications

Operators use daemon diagnostics for detailed network state. Channel tools remain stable even if backend changes. `UnauthorizedPeer` and `ChannelNotJoined` are user-visible policy/state outcomes rather than generic network failures.

## Implementation implications

Bridge maps tools to generic IPC/transport commands. Tool names may receive a namespace prefix at implementation packaging time to avoid collisions. Claude-facing `content_type` maps explicitly to transport `media_type` at the bridge boundary.

## Revisit conditions

Revisit if user studies show a required operation cannot be expressed via these primitives; keep admin mutations separate.
