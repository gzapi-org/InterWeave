# Applications

> Activation and dependency order is governed by [`architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md`](../architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md) and ADR-0046.

Executable composition roots only. Application directories must stay thin: parse platform/CLI inputs, construct shared crates, start lifecycle, and render/publish results. Network policy, wire codecs, retention rules, trust, discovery, endpoint routing, and persistence semantics belong in reusable crates.

Built: `transport-daemon` and `transportctl` (Stage 13), `human-desktop` (Stage 15), `claude-channel` (Stage 16). `human-android` is Stage 17's and holds a README only.
