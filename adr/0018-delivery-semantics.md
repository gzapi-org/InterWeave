# Realtime best-effort delivery only

**Status:** Accepted; direct acceptance clarified for endpoint routing.

## Context

GossipSub and direct streams cannot honestly provide stronger end-to-end guarantees without durable state/ack protocols that are absent.

## Decision

Define realtime best-effort transport with bounded local at-most-once presentation after deduplication. There is no global ordering, exactly-once, durable queue, or offline mailbox.

For direct v2, `AcceptedV2` means the remote transport resolved one EndpointId and successfully enqueued the event into that endpoint's bounded local event queue. It does not mean the human/Claude/application processed or persisted it.

## Alternatives considered

Guaranteed delivery; exactly-once; total order; durable acknowledgement workflow; acknowledge before endpoint queue admission.

## Consequences

Offline/unavailable endpoints produce `no_route` instead of hidden buffering. Applications requiring durable delivery implement it above transport or use a future explicit capability.

## Security implications

Replay/duplicates are mitigated only within bounded windows. Endpoint acceptance must not be mistaken for application authorization or user receipt.

## Operational implications

Diagnostics expose failures/drops/no-route/empty meshes. Human UI may persist messages it actually receives without changing network guarantees.

## Implementation implications

Direct backend must await local endpoint queue admission before `AcceptedV2`. Tool/UI wording remains precise.

## Revisit conditions

Only with a designed durable backend/protocol and explicit retention/ack semantics.
