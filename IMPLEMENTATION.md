# InterWeave implementation repository layout

The repository now has two deliberately separate halves:

- [`architecture/`](./architecture/README.md) is the frozen specification/source of truth.
- `apps/`, `crates/`, `tests/`, `fixtures/`, `test-data/`, `packaging/`, `spikes/`, and `xtask/` are tracked implementation landing zones.

There are production Rust crates under `crates/` and `tests/`, activated one canonical stage at a time. There is no application binary, Android Gradle project, installer, or service unit yet: `apps/` and `packaging/` stay empty until the stage that needs them opens.

**Stages 0-10 are complete; Stage 11 is open.** SPIKE-004's phase A
closed 2026-09-01 (PASS for implementation, loopback only), so the
AutoNAT/Relay/DCUtR work is authorized; its phase B — the real-NAT
matrix — is required before the stage can close and has not run. The virtual root [`Cargo.toml`](./Cargo.toml) lists the active members and is authoritative — deliberately not restated here, because the copy of this sentence that named a roster went stale twice while the manifest stayed correct. `workspace.metadata.interweave.status` records the open stage in one machine-readable place. `workspace.metadata.interweave` records the remaining planned member/test paths without making them buildable; when a canonical bottom-up stage starts, add a crate manifest only for the crate/package being implemented and add that path to `[workspace].members` in the same change.

The toolchain is pinned in [`rust-toolchain.toml`](./rust-toolchain.toml), and edition, MSRV, inherited lints, shared dependency versions and the release profile are declared once at the workspace root. `cargo xtask ci` runs formatting, lints, tests, every tree check and every self-test in one pass.

See [`architecture/docs/architecture/implementation-repository-layout.md`](./architecture/docs/architecture/implementation-repository-layout.md) and [`architecture/adr/0045-implementation-repository-layout.md`](./architecture/adr/0045-implementation-repository-layout.md) for the normative placement rules.
Project and machine-facing namespace selection is frozen by [ADR-0047](./architecture/adr/0047-interweave-project-and-wire-namespace.md): display name **InterWeave**, machine/wire namespace `interweave`.

## Canonical construction order

The implementation SHALL follow [`architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md`](./architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md) and [ADR-0046](./architecture/adr/0046-bottom-up-implementation-order.md).

The historical numbered phases remain scope/release labels. They are **not** the literal dependency order. In particular, root connection/dial admission must be implemented and tested before Kademlia, AutoNAT, Relay, or DCUtR are enabled.

Canonical milestones are M1 contracts/domain, M2 authenticated local-network transport, M3 complete network engine, M4 desktop integrations, and M5 Android/security/packaging release.
