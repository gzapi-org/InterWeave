# InterWeave implementation repository layout

The repository now has two deliberately separate halves:

- [`architecture/`](./architecture/README.md) is the frozen specification/source of truth.
- `apps/`, `crates/`, `tests/`, `fixtures/`, `test-data/`, `packaging/`, `spikes/`, and `xtask/` are tracked implementation landing zones. `third_party/` holds vendored dependency sources under their own licences (ADR-0051); it is not a landing zone for first-party code.

There are production Rust crates under `crates/` and `tests/`, activated one canonical stage at a time. Since Stage 13 `apps/transport-daemon` and `apps/transportctl` are the first application binaries; there is no Android Gradle project, installer, or service unit yet: the rest of `apps/` and `packaging/` stay empty until the stage that needs them opens.

**Stages 0-14 are complete; Stage 15 is open** (`stage-15-desktop-human-client`).
Stage 14 closed 2026-10-03 on the plan record (#161), the batches 2+4+3
(#166), 5 (#167), 6 (#168), 7 (#169) and 8 (#170) and the rust-ui-dev
remit (#163): the facade, the store's application tables, the render and
presentation models and the reference Slint views, the in-memory fake
passing the same conformance functions as the real bindings, HumanChatV2
across two daemons, and the envelope contract `active` with the close;
the plan's §17 closing record carries what it did not prove.
Stage 15 builds the desktop human client, `apps/human-desktop`; Stage 16,
the Claude Code Channel bridge, runs beside it under the plan's §19
(SPIKE-001 PASS, 2026-10-03), the status naming the lowest open stage.
Stage 13 closed 2026-10-01 on the IPC v2 batches (#144, #145, #147, #151,
#154, #156, #157), the composition hardening (#159), the ledger audit
(#160) and the `peer.disconnected` producer (#162): the daemon, the IPC
client library passing the in-process binding's conformance suite over
real sockets, `transportctl`, and every `ipc` contract `active` with the
close; the plan's §16 closing record carries what it did not prove.
Stage 12 closed 2026-09-28 on the four composition batches (#135, #137,
#138, #139), the connectivity contracts flipping to `active` with the
close; the plan's §15 closing record carries what it did not prove.
Stage 11 closed 2026-09-27: SPIKE-004's phase A (2026-09-01) authorized
the AutoNAT/Relay/DCUtR work and its phase B — the real-NAT matrix — ran
as a containerised NAT row and five node rows, closed by the record of
2026-09-26 (effective on its landing), with four owner-deferred limits
carried by name in the plan's §14 closing record; the connectivity
contracts stayed `approved` until the composition root served them, in Stage 12. Stage 13 was the daemon and desktop IPC v2. The virtual root [`Cargo.toml`](./Cargo.toml) lists the active members and is authoritative — deliberately not restated here, because the copy of this sentence that named a roster went stale twice while the manifest stayed correct. `workspace.metadata.interweave.status` records the open stage in one machine-readable place. `workspace.metadata.interweave` records the remaining planned member/test paths without making them buildable; when a canonical bottom-up stage starts, add a crate manifest only for the crate/package being implemented and add that path to `[workspace].members` in the same change.

The toolchain is pinned in [`rust-toolchain.toml`](./rust-toolchain.toml), and edition, MSRV, inherited lints, shared dependency versions and the release profile are declared once at the workspace root. `cargo xtask ci` runs formatting, lints, tests, every tree check and every self-test in one pass.

See [`architecture/docs/architecture/implementation-repository-layout.md`](./architecture/docs/architecture/implementation-repository-layout.md) and [`architecture/adr/0045-implementation-repository-layout.md`](./architecture/adr/0045-implementation-repository-layout.md) for the normative placement rules.
Project and machine-facing namespace selection is frozen by [ADR-0047](./architecture/adr/0047-interweave-project-and-wire-namespace.md): display name **InterWeave**, machine/wire namespace `interweave`.

## Canonical construction order

The implementation SHALL follow [`architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md`](./architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md) and [ADR-0046](./architecture/adr/0046-bottom-up-implementation-order.md).

The historical numbered phases remain scope/release labels. They are **not** the literal dependency order. In particular, root connection/dial admission must be implemented and tested before Kademlia, AutoNAT, Relay, or DCUtR are enabled.

Canonical milestones are M1 contracts/domain, M2 authenticated local-network transport, M3 complete network engine, M4 desktop integrations, and M5 Android/security/packaging release.
