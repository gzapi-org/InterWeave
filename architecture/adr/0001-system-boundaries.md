# Generic transport boundary

**Status:** Accepted

## Context

The prompt requires payload agnosticism, replaceable discovery, and a backend that does not become the Claude-facing definition. Without a hard boundary, libp2p concepts or application workflow tend to leak upward.

## Decision

Keep four explicit layers: Claude Code, Channel MCP bridge, generic transport runtime, and network backend. Claude-specific concepts stop at the bridge; libp2p-specific concepts stop at the backend. The generic transport carries opaque payloads plus transport metadata and defines no application coordination semantics.

## Alternatives considered

A single combined Claude/libp2p API; an application-specific coordination service; exposing Swarm/Multiaddr directly to Claude.

## Consequences

Adds translation layers and versioned contracts, but allows independent evolution and testing. Some diagnostics must deliberately omit backend detail.

## Security implications

The boundary prevents remote network data from becoming privileged application control by construction. Trust/admission remains a transport concern; Claude permissions remain a Claude concern.

## Operational implications

Operations can inspect deeper backend diagnostics through a local CLI without expanding the Claude tool surface.

## Implementation implications

Create transport-neutral types first. Backend adapters map to/from them. No production implementation may cross the dependency direction.

## Revisit conditions

Revisit only if a required capability cannot be represented without exposing a backend primitive, and document why that primitive is truly portable.
