# Frozen conformance fixtures

> Activation and dependency order is governed by [`architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md`](../architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md) and ADR-0046.

Normative deterministic vectors shared by Rust implementations, platform bindings and future third-party clients. A fixture changes only with the corresponding contract/version/ADR change. Do not put mutable topology/scenario data here; use `test-data/`.

## Verification

```
python3 tools/checks/verify_fixture_vectors.py
```

Every vector file declares its `algorithm.id`; the verifier implements that algorithm from the specification — never from the fixture's own description — and recomputes each vector. An algorithm it does not know is a **failure, not a skip**: a vector file nothing can verify is exactly what this guards against. Vectors marked `frozen_by` are goldens re-frozen by an ADR, and every vector in a file must hash distinctly, because collisions between the edge cases are the bug the edge cases exist to catch.

A drifted vector is a protocol break, not a test failure. Changing one is a decision (ADR-0049, `CLAUDE.md` §7).
