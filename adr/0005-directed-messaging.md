# Direct request-response protocol for one-to-one messages

**Status:** Accepted; wire framing/version amended by ADR-0030.

## Context

A dedicated direct protocol is mandatory. Request-response provides failure/timeout surfaces and protocol negotiation while avoiding directed-over-GossipSub. A small acceptance response makes the remote transport admission boundary explicit.

The earlier architecture draft named `/claude-p2p-channel/direct/1.0.0` before local endpoint addressing existed. ADR-0030 introduces one PeerId with multiple addressable local endpoints and therefore requires a wire-major change before production implementation.

## Decision

Use rust-libp2p `request_response` with the endpoint-aware implementation target:

```text
/claude-p2p-channel/direct/2.0.0
```

Each message is a bounded request carrying source EndpointId and optional destination EndpointId, and receives `AcceptedV2` with the resolved endpoint or a coarse `RejectedV2`. Underlying connections are reused; logical exchanges use independent substreams. No automatic retry.

The architecture-only `/direct/1.0.0` format is superseded before implementation and is not a required compatibility target.

## Alternatives considered

Custom raw StreamProtocol; one-way fire-and-forget stream; per-message new connection; v1 peer-only request with local fan-out; application-payload endpoint routing. GossipSub-based directed traffic remains excluded.

## Consequences

Success is stronger than a local socket write but weaker than application processing. Concurrent sends are unordered. Timeouts/cancellation can race remote acceptance. Remote default routing is explicit and never fan-out.

## Security implications

Noise-authenticated PeerId is checked by PeerTrustPolicy. Endpoint policy may narrow profile trust. Source endpoint is derived from the sender's local data-session lease and cannot be spoofed by a local command argument; remote source endpoint remains peer-asserted routing metadata, not a sub-identity.

## Operational implications

ConnectionManager may dial known addresses within the command deadline. Operators can distinguish peer reachability/protocol failures from a coarse `RemoteEndpointUnavailable` route result without receiving a remote endpoint-existence oracle.

## Implementation implications

Custom codec rejects oversized/invalid endpoint lengths before allocation. Receiver sends `AcceptedV2` only after one resolved endpoint event queue accepts the message. Caller retries with the same message ID if desired; receiver dedup includes source/destination endpoint context.

## Revisit conditions

Revisit if request-response overhead is excessive, a standard direct-stream abstraction offers better semantics, or endpoint-addressed compatibility with an actually deployed legacy version becomes necessary.
