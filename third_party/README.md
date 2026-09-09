<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrea Benetton -->
# Third-party material

Vendored dependency sources, each under its own licence and each the
subject of an ADR that says why a registry release would not do. Every
vendored file is listed with its provenance in
`tools/checks/license_exempt.txt` — this README is first-party and is
not among them. A subdirectory without entries is an unreviewed import.

| Directory | Upstream | Licence | Why vendored | Patch |
|---|---|---|---|---|
| `libp2p-autonat/` | `libp2p-autonat` 0.15.0 (crates.io) | MIT | ADR-0051 | `INTERWEAVE.patch` — `Behaviour::retest` |

Each copy is the registry tarball minus its packaging files
(`.cargo-ok`, `.cargo_vcs_info.json`, `Cargo.toml.orig`, `Cargo.lock`),
plus the upstream `LICENSE` and the recorded patch. The root
`Cargo.toml`'s `[patch.crates-io]` table is what makes the copy the one
the workspace builds.
