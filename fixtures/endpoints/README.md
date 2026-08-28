# endpoints

EndpointId/direct destination/directory grammar and canonical response fixtures.

`endpoint-id-grammar-v1.json` — 18 verdict vectors over `^[a-z][a-z0-9._-]{0,63}$`: the conventional names, both length bounds, and the rejections (leading digit/hyphen/dot, any uppercase, space, slash, colon, non-ASCII, trailing newline).

A verdict set repeats its results by design, so distinctness is off for this algorithm.

`endpoint-directory-v1-frame.json` — 5 framing vectors for `/interweave/endpoints/1.0.0`: the one-byte request, an empty directory, a single entry, the 2094-byte ceiling (32 entries at 64 bytes each), and a refusal. Byte order is big-endian, pinned in `transport/libp2p/ENDPOINTS.md`.
