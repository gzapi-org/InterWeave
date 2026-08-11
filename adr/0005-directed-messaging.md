# Direct request-response protocol for one-to-one messages

**Status:** Accepted

## Context

A dedicated direct protocol is mandatory. Request-response provides failure/timeout surfaces and protocol negotiation while avoiding a custom Swarm behavior. A tiny acceptance response removes ambiguity about whether the remote transport accepted a frame.

## Decision

Use rust-libp2p `request_response` with protocol ID `/claude-p2p-channel/direct/1.0.0`. Each message is a bounded request and receives `Accepted` or coarse `Rejected`. Underlying connections are reused; logical exchanges use independent substreams. No automatic retry.

## Alternatives considered

Custom raw StreamProtocol; one-way fire-and-forget stream; per-message new connection. GossipSub-based directed traffic is excluded by requirement and not an alternative.

## Consequences

Success is stronger than a local socket write but weaker than application processing. Concurrent sends are unordered. Timeouts/cancellation can race remote acceptance.

## Security implications

Noise-authenticated PeerId is checked by PeerTrustPolicy before `Accepted`. Rejection messages are deliberately coarse to avoid policy disclosure.

## Operational implications

ConnectionManager may dial known addresses within the command deadline. Operators can distinguish unknown peer, dial failure, protocol mismatch, rejection, and timeout.

## Implementation implications

Custom codec rejects oversized declared lengths before allocation. Caller retries with the same message ID if desired; receiver dedup prevents duplicate local presentation within TTL.

## Revisit conditions

Revisit if implementation shows request-response overhead is excessive or a standard libp2p direct-stream abstraction offers better semantics without losing explicit acceptance/failure behavior.
