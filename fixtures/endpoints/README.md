# endpoints

EndpointId/direct destination/directory grammar and canonical response fixtures.

`endpoint-id-grammar-v1.json` — 18 verdict vectors over `^[a-z][a-z0-9._-]{0,63}$`: the conventional names, both length bounds, and the rejections (leading digit/hyphen/dot, any uppercase, space, slash, colon, non-ASCII, trailing newline).

A verdict set repeats its results by design, so distinctness is off for this algorithm.
