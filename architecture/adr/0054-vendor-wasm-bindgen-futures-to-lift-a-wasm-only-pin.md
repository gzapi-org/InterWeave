# Vendor wasm-bindgen-futures to lift a wasm-only pin the desktop client cannot resolve around

**Status:** Accepted (2026-10-03; the owner's arming of the pull request that lands this record — the Stage 15 record, plan §18 (14) — is the acceptance on record)

## Context

No Slint windowing backend resolves in this workspace's lockfile. `i-slint-backend-winit` 1.18.1 depends, under a `cfg` the lockfile cannot see (Apple targets that are not macOS), on `i-slint-renderer-skia`, which reaches `glow` 0.18 and so `js-sys ~0.3.100`; `libp2p-swarm` 0.48.0 pins `wasm-bindgen-futures = "=0.4.58"`, which pins `js-sys = "=0.3.85"`. Cargo's resolver is target-independent, so the two requirements meet in one graph and `cargo` reports "failed to select a version for js-sys" for the software, femtovg and skia renderers alike (rust-ui-dev's measurement of 2026-10-03 on a scratch copy of 4489ee24, message 01a10038-7471). The root `Cargo.toml` already records this pin: it forced `futures-timer` from 3.0.4 back to 3.0.3 in PR #109, with "revisit when `libp2p-swarm` relaxes that exact pin" as the trigger. rust-libp2p's main branch still carries the pin, tied to `gloo-timers` on wasm, and no release relaxing it is in sight (p2p-network-dev, 01a10051-f0b7).

Both sides of the conflict are wasm-only. Nothing this repository builds targets `wasm32`; `wasm-bindgen-futures`, `js-sys`, `wasm-bindgen` and `web-sys` are compiled on no target here. The conflict is a resolver fact about a graph that never runs, standing between Stage 15's desktop client and its window.

## Decision

1. **The workspace builds `wasm-bindgen-futures` from `third_party/wasm-bindgen-futures/`**, the 0.4.58 crates.io tarball (sha256 `70a6e77fd0ae8029c9ea0063f87c46fde723e7d887703d74ad2616d792e51e6f`, the lockfile's checksum for it), selected by a `[patch.crates-io]` entry in the root `Cargo.toml`, under ADR-0051's route — Decisions 1, 2, 5–8 of that record apply unchanged: the tarball minus its packaging files plus the upstream `LICENSE` and this repository's `INTERWEAVE.patch`; every file listed in `tools/checks/license_exempt.txt` with licence, holder, version and checksum; the `third_party/README.md` row; the first-party guards excluding it and the shipped-binary guards including it; `check_vendored_advisories.sh` covering it.
2. **The patch is three lines of `Cargo.toml` and nothing else**: the exact pins on `js-sys`, `wasm-bindgen` and `web-sys` become caret requirements at the same versions, so the resolver may select the `js-sys` the rest of the graph needs. No Rust source changes. The diff is recorded as `INTERWEAVE.patch` and reviewers audit the patch, not the tree.
3. **The crate is target-gated and inert here.** Every file under the vendored tree is compiled only on `wasm32`, which this repository does not build; the vendored code is in no shipped binary; the patch changes what cargo *resolves*, and its one native effect is that `futures-timer` may return from 3.0.3 to 3.0.4 (the root `Cargo.toml`'s #109 note), which the vendoring PR states. A future wasm target would have to revisit this record before relying on the vendored copy, because a relaxed pin on `wasm-bindgen`'s family is exactly what that family's exact pins exist to prevent on wasm.
4. **The smaller tree was chosen on measurement.** Vendoring `libp2p-swarm` 0.48.0 with its one pin relaxed would have the same resolver effect through a 1-line diff, but puts 45 files and 13,258 lines of the network core every connection runs through under this repository's ownership; `wasm-bindgen-futures` is 15 files, none compiled (p2p-network-dev's measurement, 01a10051-f0b7). A second workspace with its own lockfile for `apps/human-desktop` was refused: every tree check and the status guard assume one.
5. **`check_vendored_advisories.sh` must enrol the new crate on its own**, by the three sources its header names (the package graph, every manifest under `third_party/`, every `[patch.*]` path); the vendoring PR states that it does, with the guard's output, and a guard that passes without naming `wasm-bindgen-futures` is a finding against the PR, not a pass.
6. **The vendoring is p2p-network-dev's**, in the PR that precedes Stage 15's batch 4 (rust-ui-dev's windowing backend), after this record is on `main`; the patched graph must resolve with `slint`'s `backend-winit`, `renderer-software` and `accessibility` added, build the workspace, and leave every check and self-test green except `check_dependencies.sh`'s verdict on the Slint feature set's own licences and advisory, which is the owner's separate decision (plan §18 (14)) and not this record's.

## Alternatives considered

**Vendor `libp2p-swarm`** (rust-ui-dev's first proposal): the same effect, a 1-line diff, 13,258 lines owned — rejected under Decision 4. **A second workspace** for the desktop app: refused under Decision 4. **Wait for upstream**: no release in sight, and the pin has a TODO in rust-libp2p tied to wasm `gloo-timers`; it stays the revisit trigger, not the plan. **Drop winit for a different backend**: Slint 1.18 offers no windowing backend without the winit crate; the Qt backend is out under ADR-0039's choice of Slint with Rust and brings a C++ toolkit.

## Consequences

The repository owns a third vendored tree, 15 files of a wasm support crate it never compiles, differing from upstream by three lines of a manifest. The maintenance contract is ADR-0051's. The root `Cargo.toml`'s `futures-timer` note (PR #109) is read with this record: the pin that forced 3.0.3 is relaxed, so `futures-timer` 3.0.4 may resolve again — the vendoring PR says which version the lockfile ends on and why.

## Security implications

None on any built target: the vendored code is not compiled. The attack surface this introduces is the provenance one every vendored crate has — a tree in this repository that is not upstream's — and it is bounded as ADR-0051 bounds it: the tarball checksum, the listed files, the recorded diff, the advisory guard.

## Operational implications

Nothing is configured. A `cargo update` that would move `wasm-bindgen-futures` past 0.4.58 is refused by the patch until this record is revisited.

## Implementation implications

`third_party/wasm-bindgen-futures/` beside the two existing trees; the `[patch.crates-io]` entry with a comment naming this record; `license_exempt.txt` rows (MIT OR Apache-2.0, the wasm-bindgen authors, 0.4.58, the checksum above); the `third_party/README.md` row; `CLAUDE.md` §1's third_party sentence names the third ADR; the `futures-timer` note in the root `Cargo.toml` gains one line pointing here.

## Revisit conditions

Drop the vendored copy and the `[patch.crates-io]` entry when a `libp2p-swarm` release stops pinning `wasm-bindgen-futures` exactly, or when Slint's winit backend stops reaching `glow` on targets this repository does not build. Reopen Decision 3 before any `wasm32` target is added.
