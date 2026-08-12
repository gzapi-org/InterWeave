# Implementation repository layout and test placement

**Status:** Accepted.

## Context

The architecture has reached a point where implementation can start without reopening the major transport, discovery, Model B endpoint, human-client, Android, retention, security, or Internet-reachability boundaries. Leaving architecture documents and future production/test code in the same root namespaces would make it easy to confuse specification with implementation, scatter tests by convenience, or let application composition roots accumulate domain logic.

The repository also needs to support two local deployment bindings (desktop daemon/IPC and Android embedded runtime) while proving that both implement the same neutral `LocalDataSession` semantics.

## Decision

1. Move the complete specification tree under repository-root `architecture/`. Internal specification paths such as `contracts/...`, `transport/...`, and `roadmap/...` are architecture-root-relative.
2. Reserve repository-root `apps/` for executable/platform composition roots only: `transport-daemon`, `transportctl`, `claude-channel`, `human-desktop`, and `human-android`.
3. Reserve repository-root `crates/` for reusable Rust packages grouped by responsibility: neutral API, config, transport, discovery, local bindings, Claude adapter, and human application layers.
4. Keep neutral API crates free of libp2p, UI, Android, SQLite, Claude SDK, and platform-specific types. Concrete libp2p remains under `crates/transport/libp2p`; human persistence/UI/platform code remains under `crates/human/*`.
5. Reserve repository-root `tests/` for cross-crate/network/conformance/E2E suites. Pure local tests belong beside future source; public crate-consumer tests may use future `<crate>/tests/`; real OS tests use desktop/Android E2E or Android instrumented tests.
6. Put shared test harnesses in `tests/support`; production code must never depend on that package.
7. Put frozen, normative byte-exact conformance vectors in `fixtures/`. Put mutable/non-normative topology, malformed, and scenario inputs in `test-data/`.
8. Keep empirical implementation experiments in `spikes/`, mapped one-to-one to `architecture/roadmap/SPIKES.md`. Spike code does not become production code by copying it silently; accepted results become ADR/contract updates and permanent tests.
9. Keep platform packaging under `packaging/`; Android application Gradle/manifest/resources will live under `apps/human-android/android/`, while release-policy/package orchestration remains under `packaging/android/`.
10. Reserve `xtask/` for future developer/test/fixture/packaging orchestration only.
11. Introduce a root virtual `Cargo.toml` whose workspace has **zero members** in this skeleton commit. Planned member/test paths are recorded only as workspace metadata. A path becomes a real crate/test package only when its phase adds a crate manifest/source and simultaneously adds it to `[workspace].members`.
12. This repository-organization commit must not create production `.rs` files, application manifests, Gradle application code, service units, installers, or executable test harnesses.

The complete physical tree and ownership rules are in `docs/architecture/implementation-repository-layout.md`.

## Alternatives considered

Keep architecture at repository root; create a separate implementation repository; flat `crates/` with no responsibility grouping; one giant integration-test crate; keep golden fixtures inside implementation crates; turn every architecture component into a crate immediately; create empty buildable crates during the architecture phase.

## Consequences

The repository gets more top-level directories, but specification and implementation become visibly distinct. Test placement is predictable, frozen fixtures are shareable across implementations, desktop and Android can share conformance suites, and crate boundaries can be introduced phase-by-phase rather than all at once.

Moving the architecture tree changes repository-root paths. Git renames preserve history, architecture-internal paths retain their original meaning relative to `architecture/`, and root navigation must point into that tree.

## Security implications

Compile-time and filesystem boundaries reinforce existing security boundaries: neutral APIs cannot casually import libp2p/platform/UI state; test-only harnesses cannot become production dependencies; admin/data-plane/platform code stays separated; normative security fixtures are not hidden in one implementation. Keeping spike code non-production prevents empirical shortcuts from silently bypassing accepted policy.

## Operational implications

CI can gate layers independently: formatting/lints/unit; contract/fixture; network/conformance; security; desktop E2E; Android host/instrumented; packaging. Heavy Android/network matrices need not block every fast local unit run, but release gates remain defined by the architecture roadmap.

## Implementation implications

Create crate/application manifests only when their phase begins. Keep application `main`/platform hosts thin. Put a test at the lowest layer that completely proves the behavior. When a spike settles version-sensitive behavior, migrate the lasting assertion into the corresponding permanent test suite/fixture.

## Revisit conditions

Revisit grouping if real compile times, ownership, platform toolchains, or release packaging justify splitting the monorepo. Do not collapse the architecture/implementation distinction, neutral API boundaries, fixture/test-data distinction, or test-only dependency direction without a new ADR.
