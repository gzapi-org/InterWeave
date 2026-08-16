# InterWeave implementation repository layout

The repository now has two deliberately separate halves:

- [`architecture/`](./architecture/README.md) is the frozen specification/source of truth.
- `apps/`, `crates/`, `tests/`, `fixtures/`, `test-data/`, `packaging/`, `spikes/`, and `xtask/` are tracked implementation landing zones.

There are no production Rust crates, application binaries, Android Gradle project, installers, or service units yet.

**Stage 0 is open.** The virtual root [`Cargo.toml`](./Cargo.toml) has exactly two members — [`xtask`](./xtask/README.md), the command runner, and `tests/support`, the test-only harness. Neither is product code. `workspace.metadata.interweave` records the remaining planned member/test paths without making them buildable; when a canonical bottom-up stage starts, add a crate manifest only for the crate/package being implemented and add that path to `[workspace].members` in the same change.

The toolchain is pinned in [`rust-toolchain.toml`](./rust-toolchain.toml), and edition, MSRV, inherited lints, shared dependency versions and the release profile are declared once at the workspace root. `cargo xtask ci` runs formatting, lints, tests, every tree check and every self-test in one pass.

See [`architecture/docs/architecture/implementation-repository-layout.md`](./architecture/docs/architecture/implementation-repository-layout.md) and [`architecture/adr/0045-implementation-repository-layout.md`](./architecture/adr/0045-implementation-repository-layout.md) for the normative placement rules.
Project and machine-facing namespace selection is frozen by [ADR-0047](./architecture/adr/0047-interweave-project-and-wire-namespace.md): display name **InterWeave**, machine/wire namespace `interweave`.

## Canonical construction order

The implementation SHALL follow [`architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md`](./architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md) and [ADR-0046](./architecture/adr/0046-bottom-up-implementation-order.md).

The historical numbered phases remain scope/release labels. They are **not** the literal dependency order. In particular, root connection/dial admission must be implemented and tested before Kademlia, AutoNAT, Relay, or DCUtR are enabled.

Canonical milestones are M1 contracts/domain, M2 authenticated local-network transport, M3 complete network engine, M4 desktop integrations, and M5 Android/security/packaging release.
