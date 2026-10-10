---
role: "devex-tooling"
class: solution
topic: "codeql-local-measurement"
description: "Reproducing an InterWeave CodeQL (Rust) alert locally, or testing a model pack — host has no rust-src, and the full tree times out"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "devex-tooling"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 0e3ce1bb7c2d6d58
---

## Reproducing an InterWeave CodeQL (Rust) alert locally, or testing a model pack — host has no rust-src, and the full tree times out

Measured 2026-10-08 (job j48, InterWeave #233):

- Bundle: `gh release download codeql-bundle-v<ver> -R github/codeql-action -p codeql-bundle-rust-linux64.tar.zst` (~98 MB; match the `tool.version` on the alert, e.g. 2.27.1) into scratch.
- **No rust-src on develop-qzapp** (Fedora system rustc, no rustup): extraction "succeeds" with 2279 macro-expansion failures and the query returns 0 — every panic!/assert! sink is invisible. Fetch `rust-src-<toolchain>.tar.xz` from static.rust-lang.org (check its .sha256) and export `RUST_SRC_PATH=<x>/rust-src/lib/rustlib/src/rust/library` before `codeql database create --build-mode=none`. Check the log for 0 "macro expansion failed".
- **The full tree does not extract within 30 min** (spikes/, third_party/). `git archive` the ref into scratch, cut `[workspace].members` down to the crates under test plus their path-dep closure, and drop `[patch.crates-io]`. A few crates extract in about 2 minutes.
- A control first: the baseline must reproduce the GitHub alerts before a 0 means anything.
- Model pack: `codeql database analyze <db> <ql> --additional-packs=<dir> --model-packs=<name>`. barrierModel on `ReturnValue` with kind `log-injection` removes a function source from rust/cleartext-logging. Variables are sources by NAME, and no row can reach them.

*Observed 2026-10-08 (devex-tooling)*
