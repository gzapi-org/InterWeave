# Generic transport boundary

**Status:** Accepted

## Context

The prompt requires payload agnosticism, replaceable discovery, and a backend that does not become the Claude-facing definition. Without a hard boundary, libp2p concepts or application workflow tend to leak upward.

## Decision

Keep four explicit layers: an MCP host, the MCP bridge, generic transport runtime, and network backend. The top layer was named Claude Code until A 2026-10-07; it is any host that speaks MCP, Claude Code the one proved so far, and its Channel extension is one DELIVERY MODE of the bridge (push), the other being a bounded pull tool for a host without push (ADR-0002, `CHANNEL-EVENT.md` §Delivery) — the owner's decision of 2026-10-07 on p2p-network-dev's question, after #221 widened the README's claim ahead of the tree. Host-specific concepts stop at the bridge; libp2p-specific concepts stop at the backend. The generic transport carries opaque payloads plus transport metadata and defines no application coordination semantics.

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

## Amendments

Full notes: [`history/0001-amendments.md`](./history/0001-amendments.md).

| Date | Amendment | Effect |
|---|---|---|
| 2026-10-07 | The top layer is an MCP host; Claude Code's Channel extension is one delivery mode of the bridge | Decision: the first layer reads "an MCP host" (Claude Code the one proved), and the bridge has two delivery modes — the Channel push and a bounded pull tool (ADR-0002, CHANNEL-EVENT.md §Delivery); host-specific concepts stop at the bridge as Claude-specific ones did. |

