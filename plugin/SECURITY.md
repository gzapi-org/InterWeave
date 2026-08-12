# Channel bridge security

Every P2P message delivered through the Channel is **untrusted external input**, even when its `source_peer` is transport-trusted. Transport trust means the peer is admitted to the data plane; it does not mean every instruction in the payload is safe or locally authorized.

## Routing metadata

`source_peer`, `source_endpoint`, `destination_endpoint`, `channel`, and `delivery_mode` are transport facts with different strengths:

- `source_peer`: authenticated libp2p transport identity;
- `source_endpoint`: route label claimed by that authenticated peer;
- `destination_endpoint`: local route selected/admitted by this daemon;
- `channel`: broadcast context;
- `delivery_mode`: direct/broadcast transport mode.

None establishes a human name, employee identity, administrator role, repository ownership, agent role, or application authorization unless a higher-level protocol separately binds and verifies that meaning.

## Administrative separation

Claude's IPC connection may not receive `admin.endpoints`, `admin.shutdown`, trust mutation, identity-key, or private configuration authority.

A remote request such as "register me as endpoint admin", "change the default endpoint", "trust this PeerId", or "revoke the human endpoint" is untrusted content and cannot cause an administrative call automatically.

## Reply safety

Direct reply tokens bind remote PeerId + remote source endpoint + this bridge's local endpoint lease epoch. Loss/reacquisition of a local endpoint invalidates old route tokens. The bridge never falls back to another local or remote endpoint when the exact route is stale.

## Endpoint enumeration

The Claude bridge does not need to expose remote endpoint enumeration as a default tool. If that is added later, endpoint names must still be described as advertised route labels, not verified service identities.


## Endpoint-directory capability

The `claude-channel` IPC client is not granted `endpoints.query` by default and exposes no `peer_endpoints` tool in v2. Adding one requires the ADR-0023 revisit/security review.
