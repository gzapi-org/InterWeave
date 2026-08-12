# Profile-scoped identities with explicit endpoint multiplexing

**Status:** Superseded by ADR-0030 for direct-message routing; profile identity decision remains accepted historical rationale.

## Context

Several Claude/human/application instances on one host must coexist without accidental key sharing. The original v1 design allowed explicit daemon/profile sharing but had no network-visible local route selector, so admitted direct messages were duplicated to every event-capable same-profile IPC client.

That was an honest safe default before endpoint addressing existed, but it does not satisfy deterministic routing for a human client and Claude sharing one PeerId.

## Decision

Retain the original identity boundary:

- one persistent network identity per named transport profile;
- not one PeerId per Claude conversation;
- not one implicit host-global PeerId;
- multiple local applications share a profile only by explicitly selecting the same profile/socket;
- independent profiles have independent keys/state/sockets.

For current direct-routing semantics, ADR-0030 supersedes v1 fan-out with explicit `EndpointId` leases under the shared PeerId. Broadcast remains per-client join-reference filtered.

## Alternatives considered

PeerId per local process; one mandatory host-global PeerId; daemon multiplexes hidden per-client PeerIds; v1 all-client direct fan-out; first-connected/round-robin direct selection; explicit EndpointId routing.

## Consequences

The network still sees one profile PeerId, while direct v2 can distinguish local application routes under that PeerId. EndpointId does not become a second transport identity.

## Security implications

Explicit profile sharing prevents accidental key sharing. Endpoint policy and leases add routing isolation but do not change the same-user IPC residual threat. Remote endpoints cannot alter profile trust or local endpoint configuration.

## Operational implications

Operators can run separate profiles when they want independent trust/identity, or configure one profile with endpoints such as `human`, `claude`, and `automation.build` when connection/identity sharing is intentional.

## Implementation implications

Profile path/key/socket ownership stays unchanged. Current implementation target adds EndpointRegistry/leases per ADR-0030 instead of v1 direct event duplication.

## Revisit conditions

Revisit if cryptographically independent sub-identities inside one daemon are required, or if endpoint leases must become shared/multicast.
