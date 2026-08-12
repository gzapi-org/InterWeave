# Implementation repository layout

The repository now has two deliberately separate halves:

- [`architecture/`](./architecture/README.md) is the frozen specification/source of truth.
- `apps/`, `crates/`, `tests/`, `fixtures/`, `test-data/`, `packaging/`, `spikes/`, and `xtask/` are tracked implementation landing zones.

This commit creates **structure only**. There are no production Rust crates, application binaries, Android Gradle project, installers, service units, or test executables yet.

The virtual root [`Cargo.toml`](./Cargo.toml) has no members. `workspace.metadata.claude-p2p-channel` records planned member/test paths without making them buildable. When a phase starts, add a crate manifest only for the crate/package being implemented and add that path to `[workspace].members` in the same change.

See [`architecture/docs/architecture/implementation-repository-layout.md`](./architecture/docs/architecture/implementation-repository-layout.md) and [`architecture/adr/0045-implementation-repository-layout.md`](./architecture/adr/0045-implementation-repository-layout.md) for the normative placement rules.
