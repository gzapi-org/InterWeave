# gossipsub

GossipSubMessageIdV1 source+sequence vectors and validation-order cases.

`gossipsub-message-id-v1.json` — SHA-256 over domain || u16be(len(source)) || PeerId bytes || u64be(wire sequence), including the ADR-0047 golden and the two-publishers-one-sequence pair that must not collide.
`gossipsub-topic-key-v1.json` — SHA-256 over domain || ChannelId, including the ADR-0047 golden and a case-differing twin.

Both recompute on every run. Validation-order cases are behaviour, not vectors, and live in the conformance suites.
