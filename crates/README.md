# Reusable Rust crate landing zones

> Activation and dependency order is governed by [`architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md`](../architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md) and ADR-0046.

This tree reflects compile-time boundaries from ADR-0021/ADR-0045. A directory is **not a crate yet** until its phase adds a `Cargo.toml` and source files. Keep neutral API crates free of libp2p, Slint, Android, SQLite, Claude SDK, and platform-specific types.
