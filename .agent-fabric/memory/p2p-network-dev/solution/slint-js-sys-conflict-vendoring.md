---
role: "p2p-network-dev"
class: solution
topic: "slint-js-sys-conflict-vendoring"
description: "Why slint backend-winit does not resolve beside libp2p-swarm 0.48.0, and the measured vendoring options (2026-10-03)"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-05"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - ee2a27ddaf01dccc
---

## Why slint backend-winit does not resolve beside libp2p-swarm 0.48.0, and the measured vendoring options (2026-10-03)

libp2p-swarm 0.48.0 pins wasm-bindgen-futures =0.4.58, which pins js-sys
=0.3.85 / wasm-bindgen =0.2.108 / web-sys =0.3.85; slint backend-winit's
skia renderer pulls glow 0.18, which needs a newer js-sys, so the graph
does not resolve. rust-libp2p master still has the `=` pin (TODO about
gloo-timers on wasm), so no relaxing release is in sight.

Measured for architect-cto (reply 01a10051-f0b7-7ca6-833e-c426ac100da5):
- (a') vendor wasm-bindgen-futures, 3 `=` lines relaxed: 15 files,
  wasm-only (never compiled natively), 7 licence-header exemptions owed.
  Recommended.
- (a) vendor libp2p-swarm, 1 line: 45 files, 13,258 Rust lines of the
  network core, 37 exemptions owed.
Both resolve and build. check_dependencies is red in both for the slint
graph alone: BSL-1.0 (clipboard-win, error-code), the Slint licence on
i-slint-backend-winit/-renderer-skia/-renderer-software, and ttf-parser
unmaintained (RUSTSEC-2026-0192). Not checked: whether
check_vendored_advisories enrols a new vendored crate by itself.

Switching slint feature sets grew target/ to 152 GiB (disk at 91%); clean
after each graph switch. See [[never-share-a-target-dir-across-worktrees]].

Decided 2026-10-04 (the owner, recorded in plan §18 (14) on #175 at 997dd707; relay seq 11467):
- ADR-0054 accepts (a'). The vendoring is my PR (job j12). It precedes rust-ui-dev's B4 and lands after #175 is on main.
- The i-slint crates of the windowing backend are admitted under Slint Royalty-free 2.0, per crate, in the same exception shape deny.toml already uses.
- BSL-1.0 is NOT admitted: the policy graph is restricted to the Linux targets, which leaves clipboard-win and error-code (Windows-only) outside it.
- Completed the same day (87a193f7, seq 11476): winit + renderer-software + accessibility; RUSTSEC-2026-0192 is ignored in deny.toml with its reason, revisited at the next Slint bump; [graph] targets = the Linux targets built.
- My vendoring PR contains: third_party/wasm-bindgen-futures, the [patch.crates-io] entry, the license_exempt.txt rows, the third_party README row, and check_vendored_advisories naming it. It opens after #175 merges and before R1a, because it blocks B4.
- The deny.toml edits are rust-ui-dev's B4, not mine.

*References: never-share-a-target-dir-across-worktrees*

*Observed 2026-10-03 (p2p-network-dev)*
