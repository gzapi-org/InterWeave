# gossipsub

GossipSubMessageIdV1 source+sequence vectors and validation-order cases.

`gossipsub-message-id-v1.json` — SHA-256 over domain || u16be(len(source)) || PeerId bytes || u64be(wire sequence), including the ADR-0047 golden and the two-publishers-one-sequence pair that must not collide.
`gossipsub-topic-key-v1.json` — SHA-256 over domain || ChannelId, including the ADR-0047 golden and a case-differing twin.
`broadcast-message-v1-frame.json` — the `BroadcastMessageV1` envelope bytes, including both absences (`media_type_len = 0` and an empty payload) and a `sent_at_ms` of `u64::MAX` that changes the bytes and nothing else.

All three recompute on every run.

Two classes of case are deliberately absent. **Validation-order behaviour**
— Accept/Ignore/Reject, and authenticity preceding any duplicate-cache
entry — is behaviour rather than a vector, and lives in the conformance
suites. **Decode failures** — a truncated frame, a declared payload past
the 49152-byte ceiling — are likewise tests rather than vectors: a fixture
holds frames that encode, and a 48 KiB payload would add 96 KiB of hex
here while pinning nothing the shorter vectors do not already pin.
