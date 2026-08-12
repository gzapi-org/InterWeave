# Frozen conformance fixtures

> Activation and dependency order is governed by [`architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md`](../architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md) and ADR-0046.

Normative deterministic vectors shared by Rust implementations, platform bindings and future third-party clients. A fixture changes only with the corresponding contract/version/ADR change. Do not put mutable topology/scenario data here; use `test-data/`.
