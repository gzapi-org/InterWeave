# Test packages and placement

> Activation and dependency order is governed by [`architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md`](../architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md) and ADR-0046.

Cross-crate/system tests live here. Each suite becomes an independent test package only when implementation starts. Keep a test at the **lowest layer that completely proves the behavior**:

1. pure/local logic -> `#[cfg(test)]` beside future crate source;
2. public crate API behavior -> future `<crate>/tests/`;
3. multi-crate/network/conformance behavior -> one of these root suites;
4. real OS behavior -> desktop/Android E2E or Android instrumented tests.

Shared harness code belongs in `tests/support/`; production crates must never depend on it.

Frozen byte-exact vectors live in `fixtures/`, not inside one test package. Mutable scenario inputs live in `test-data/`.
