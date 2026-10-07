# Reusable Rust crates

> Activation and dependency order is governed by [`architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md`](../architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md) and ADR-0046.

This tree reflects compile-time boundaries from ADR-0021/ADR-0045. Every directory here is a crate that builds, except `human/android-platform`, which waits for Stage 17; `[workspace].members` in the root `Cargo.toml` is the roster, and a directory is a crate only once its stage adds a `Cargo.toml` and source files. Keep neutral API crates free of libp2p, Slint, Android, SQLite, Claude SDK, and platform-specific types.
