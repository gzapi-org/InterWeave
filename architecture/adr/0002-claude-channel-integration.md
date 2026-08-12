# Reuse official Claude Channel and Telegram patterns

**Status:** Accepted

## Context

The official Telegram Channel is the closest implementation reference and demonstrates how Claude expects a local messaging bridge to behave. Current Claude documentation also defines plugin `channels` packaging that must be revalidated at implementation time.

## Decision

Use the current Claude Code Channel contract: stdio MCP server, `claude/channel` capability, push `notifications/claude/channel`, ordinary tools for outbound actions, explicit Channel instructions, and sender/trust gating before notification delivery. Adopt Telegram's proven content/meta separation and terminal-only trust mutation principle. Adapt transport ownership into a daemon. Do not opt into remote permission relay in v1.

## Alternatives considered

Inventing a parallel Claude integration; embedding P2P details in prompt tags; mechanically copying Telegram tools/state/poller architecture; remote permission relay by PeerId alone.

## Consequences

Claude-facing behavior stays familiar and testable. Packaging remains version-sensitive and requires a pre-implementation compatibility spike.

## Security implications

The strongest adopted pattern is admission before Claude injection. Trust config cannot be changed merely because an inbound Channel message asks for it. Permission relay is deferred because PeerId is not equivalent to an authorized human approver.

## Operational implications

The bridge remains session-scoped and can be restarted independently. The daemon carries network continuity.

## Implementation implications

Future plugin package will bind its Channel to an MCP server according to the current plugin reference; implementation uses the MCP SDK version supported by the target Claude Code release.

## Revisit conditions

Revisit if Anthropic changes the Channel extension contract or graduates it from research preview with incompatible lifecycle/packaging semantics.
