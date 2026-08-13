# direct-v2

DirectMessageV2 framing and DirectContentFingerprintV1 vectors.

## `direct-message-v2-frame.json`

The request frame implementations must agree on byte for byte. Six vectors covering the cases a codec gets wrong: an explicit destination, `destination_endpoint_len = 0` for the receiver's default, `media_type_len = 0` for absence, an empty payload that still carries its `u32be` length, both endpoint labels at the 64-byte ceiling, and a pair differing only in `sent_at_ms` — the frames differ, the content fingerprint does not.

These are **derived** from the layout in `architecture/transport/libp2p/DIRECT.md`, not published by any ADR, so the file is anchored by its `adr` list rather than a per-vector `frozen_by`.

Writing them surfaced a gap: the frame's byte order was never stated. `DIRECT.md` now pins big-endian, which is the only choice consistent with the IPC length prefix and the content fingerprint — three places that would otherwise disagree about one repository's byte order.

## `direct-content-fingerprint-v1.json`

The content fingerprint stored alongside a positive direct dedup entry (ADR-0019). It is what stops an admitted retry from being rerouted to a different local application, and what stops one idempotency key from silently aliasing two different message bodies — so implementations must agree on it byte for byte.

`golden-text-plain-hello` is the golden **re-frozen by ADR-0047**: its value changed when the wire namespace became `interweave`, because the domain prefix participates in the hash. That is the whole reason this file is checked rather than trusted — the value moved once already, under an edit nobody thought of as a protocol change.

The other six vectors are derived from the same algorithm and pin the edges an implementation gets wrong:

| Vector | Pins |
|---|---|
| `absent-media-same-payload` | media presence changes the hash; absence is not an empty string |
| `absent-media-empty-payload` | the 4-byte length precedes even a zero-length payload |
| `present-media-empty-payload` | must not collide with the absent-media empty payload |
| `human-chat-envelope-media` | media strings containing `+`, `;`, `=` |
| `media-length-boundary-128` | the 128-byte ceiling is a limit on valid input, not on hashing |
| `payload-high-bytes` | raw bytes, no UTF-8 normalization |

Inputs are hex so no re-encoding of this file can change what is hashed; `payload_utf8` is a reader convenience and never the input.

Verify with:

```
python3 tools/checks/verify_fixture_vectors.py
```

It implements the algorithm from `architecture/contracts/ENDPOINTS.md` rather than from this file — a verifier that reads its rule from the artifact it checks proves nothing.
