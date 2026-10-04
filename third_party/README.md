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
| `libp2p-autonat/` | `libp2p-autonat` 0.16.0 (crates.io) | MIT | ADR-0051 | `INTERWEAVE.patch` — two patches: `Behaviour::retest` on the client (Decision 3); `DialBackOutcome` on the server's public `Event` (Decision 3a, 2026-09-18: the crate's `result` is `Ok` for any delivered response, a negative one included, so the dial status the response carried is now a field beside it — additive, no existing field changed) |
| `libp2p-mdns/` | `libp2p-mdns` 0.49.0 (crates.io) | MIT | ADR-0053 | `INTERWEAVE.patch` — ADR-0053 rules 2–5, 7 and 8: a cap on the record store in the provider's shape (`MAX_DISCOVERED_PEERS` peers, `MAX_ADDRESSES_PER_DISCOVERED_PEER` addresses each; within the bound hit the soonest to go is evicted and reported as expired, each batch netted and its retractions reported first) and on each interface's discovered queue and send buffer; the announcer's TTL clamped to `MAX_RECORD_TTL`; each answer — the peer answer and the service-discovery answer — sent at most once a second per interface, stamped when it leaves the send buffer, sent or failed, and never queued twice; `Event::InterfaceFailed` for a failed bind or join, a receive error that ends an interface, and a send error, and `Event::WatcherFailed` for an interface-watcher error, once until it recovers, a watcher that fails twice in a row no longer polled (the `Provider` trait exported so a test can end one); the drop counts, `Behaviour::drop_counts()`; and (rule 8) the two INFO lines for a discovered and an expired record no longer carry the address, and `Behaviour::discovered_records()` yields each held record with its expiry, which the runtime's refresh (rule 10) reads; and `impl Drop for Behaviour`, which aborts every interface task, so a replaced behaviour stops (rule 5, A 2026-09-26) — additive, no existing field or variant changed |
| `wasm-bindgen-futures/` | `wasm-bindgen-futures` 0.4.58 (crates.io) | MIT OR Apache-2.0 | ADR-0054 | `INTERWEAVE.patch` — the exact pins on `js-sys`, `wasm-bindgen` and `web-sys` in `Cargo.toml` relaxed to caret requirements at the same versions, so the resolver can select the `js-sys` Slint's windowing backend needs; no source changed. Every file is compiled only on `wasm32`, which this repository does not build |

Each copy is the registry tarball minus its packaging files
(`.cargo_vcs_info.json`, `Cargo.toml.orig`, `Cargo.lock`), plus the
upstream `LICENSE` (for `wasm-bindgen-futures`, the tarball's own
`LICENSE-MIT` and `LICENSE-APACHE`) and the recorded patch. `.cargo-ok` is NOT one of
them — it is cargo's own extraction marker, written under
`registry/src/` when a tarball is unpacked, so a re-vendorer who expects
it in the archive is looking for a file that was never there. The root
`Cargo.toml`'s `[patch.crates-io]` table is what makes the copy the one
the workspace builds.

To check that the recorded hunks are still present in the file the patch
names, reverse-apply it **from the workspace root with `-p0`**:

```sh
git apply --check --reverse -p0 third_party/libp2p-autonat/INTERWEAVE.patch
```

The paths inside the diff are already workspace-relative, so the two
invocations a reader reaches for first — `-p1` from the root, or running it
from inside the vendored directory — both fail with `No such file or
directory` and make a clean tree look divergent. ADR-0051 Decision 8 names
re-applying the diff as part of every libp2p bump; this is how to confirm
the hunks landed.

**It proves less than "the tree is unchanged".** The patch touches four
files in twelve hunks; the vendored tree has 31 files. An unrecorded edit to
another file, or to a region of a patched file outside the hunks' context,
passes this check silently. Tying the remaining bytes to the upstream
tarball -- ADR-0051's named follow-up -- is
`tools/checks/check_vendored_provenance.py`: it compares each tarball
with the sha256 `tools/checks/license_exempt.txt` records, reverse-applies
the patch, and requires every remaining byte and file to be the tarball's.
