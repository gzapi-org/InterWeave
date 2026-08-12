# Endpoint-addressing implementation research

Research refresh: 2026-08-12.

## rust-libp2p request-response implications

Current rust-libp2p `request_response` supports protocol families: one behavior can be constructed with multiple protocol identifiers that share request/response types, and each protocol can be marked inbound/outbound/full support. Requests use a new substream while the underlying peer connection can be reused.

That supports a clean direct-protocol major version boundary. The architecture therefore uses `/interweave/direct/2.0.0` for endpoint-addressed frames instead of embedding a second ad-hoc version negotiation inside payload content.

Primary sources:

- https://docs.rs/libp2p/latest/libp2p/request_response/
- https://docs.rs/libp2p/latest/libp2p/request_response/enum.ProtocolSupport.html

## Architectural inference

The source supports protocol-family negotiation, but it does not define this project's endpoint semantics. The following are project decisions:

- source/destination EndpointIds in direct v2;
- exclusive local endpoint leases;
- remote-default route behavior;
- separate endpoint-directory protocol;
- coarse `no_route` privacy response;
- endpoint IDs are routing metadata, not identity.
