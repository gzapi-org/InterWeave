# Minimal Claude-facing Channel surface

**Status:** Accepted

## Context

Claude needs explicit outbound route choices while normal inbound communication must be push-based. A small surface reduces prompt/tool complexity and prevents administrative controls from being remotely induced.

## Decision

Expose `broadcast`, `send`, `reply`, `join`, `leave`, `identity`, and `status` as the minimal conceptual MCP tools. Inbound data arrives only through Channel notifications. Detailed peers/discovery diagnostics belong primarily to local CLI/config UX; a bounded read-only diagnostic tool may be added only if justified.

## Alternatives considered

poll tool for inbound messages; expose Swarm/multiaddr controls; trust-edit tools; mirror Telegram react/edit/file tools.

## Consequences

Some operational troubleshooting remains outside Claude and uses local CLI. Reply tokens simplify safe routing.

## Security implications

No trust/key/config mutation tool is exposed to remote-triggered Claude flows. Outbound send still follows trust/policy.

## Operational implications

Operators use daemon diagnostics for detailed network state. Channel tools remain stable even if backend changes.

## Implementation implications

Bridge maps tools to generic IPC/transport commands. Tool names may receive a namespace prefix at implementation packaging time to avoid collisions.

## Revisit conditions

Revisit if user studies show a required operation cannot be expressed via these primitives; keep admin mutations separate.
