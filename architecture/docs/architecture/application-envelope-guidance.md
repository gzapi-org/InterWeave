# Non-normative application-envelope guidance

Transport v2 deliberately keeps GossipSub broadcast authorship at the PeerId level. Two local EndpointIds sharing one PeerId are therefore indistinguishable as transport-level broadcast authors.

First-party human/Claude applications that want a consistent display hint may use a small **application-layer** envelope such as:

```json
{
  "schema": "interweave.app-message/1",
  "from_endpoint": "human",
  "content_type": "text/plain",
  "content": "hello"
}
```

This is guidance, **not a transport contract**.

Rules:

- `from_endpoint` is an unauthenticated/peer-asserted application hint for broadcast; it is not EndpointId routing proof, trust, identity, role, or authorization;
- receivers must not grant authority because it says `human`, `admin`, `claude`, etc.;
- applications needing authenticated sub-identity/authorship must define signatures/keys above transport;
- transport does not parse, validate, rewrite, index, or route on this envelope;
- applications remain free to use plain text or another structured protocol.

The purpose of this recommendation is only to reduce divergent ad-hoc conventions between first-party clients while preserving the payload-agnostic transport boundary.
