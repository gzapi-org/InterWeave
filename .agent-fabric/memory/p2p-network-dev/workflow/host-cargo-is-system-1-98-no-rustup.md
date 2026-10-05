---
role: "p2p-network-dev"
class: workflow
topic: "host-cargo-is-system-1-98-no-rustup"
description: "develop-qzapp has only the distro cargo (Fedora 1.98.1), no rustup. Since #142 (400fb1e3) the pin IS 1.98.1, so local clippy = CI clippy; before that the 1.97.1 pin was not honoured locally. Lessons that outlive the skew: never pipe a…"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-05"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - a30c1b6d1f8a2159
  - faf8cb2c1134e70d
---

## develop-qzapp has only the distro cargo (Fedora 1.98.1), no rustup. Since #142 (400fb1e3) the pin IS 1.98.1, so local clippy = CI clippy; before that the 1.97.1 pin was not honoured locally. Lessons that outlive the skew: never pipe a verify step, -D warnings is the whole check, an expect/allow lint must exist in the pinned clippy

Measured 2026-09-17 on `develop-qzapp` as `p2p-network-dev-01`: `which -a cargo` gives `/usr/sbin/cargo` and `/usr/bin/cargo` only, `rustup` is not on PATH, and `cargo clippy --version` is `0.1.98` while `rust-toolchain.toml` pins `channel = "1.97.1"`. `cargo xtask ci` therefore runs clippy on 1.98 and fails on `clippy::chunks_exact_to_as_chunks` in `crates/api/transport-api` — the "known 1.98-vs-1.97.1 lint" the Stage 11 handover names. CI (which honours the pin) is green on the same tree.

**Why:** on PR #84 round 17 the known lint hid a second, real clippy error (`assertions_on_constants` in a new test) in the first CI-equivalent run; only re-running `cargo clippy --workspace --all-targets -- -D warnings -A clippy::chunks_exact_to_as_chunks` exposed it. This is the [[known-failing-masks-new-failures]] shape with its local cause named.

**How to apply:** after `cargo xtask ci` reports the clippy step red, re-run clippy with exactly that one lint allowed and require exit 0; never grep the output. `cargo-deny` was also absent on this account (installed 2026-09-17 with `cargo install cargo-deny --locked`), so `check_dependencies.sh` and `check_vendored_advisories.sh` exit 2 ("not installed", not a policy verdict) until it is. A rustup install honouring the pin would remove the skew; that is devex-tooling's call (the pins are theirs), raise it there rather than editing the pin.

**Lost a second time, 2026-09-19 (PR #102 round 3).** The CI-equivalent script I ran for four PRs invoked `cargo clippy --workspace --all-targets -- -A clippy::chunks_exact_to_as_chunks` WITHOUT `-D warnings`, read its exit code (0) and `tail -1`, and never read the log; an unused variable in a new wire test sat as a warning in that log and would have failed CI's `-D warnings` `rust` context. `cargo xtask ci`'s own clippy stops at the first crate (the known lint in `transport-api`) and never reaches the test crates, so it cannot show it either. The script template is exactly: `cargo clippy --workspace --all-targets -- -D warnings -A clippy::chunks_exact_to_as_chunks; echo "clippy exit $?"` and the exit must be 0 — the flag is the whole check.

**The known lint stopped firing by 2026-09-25; the skew did not.** `cargo clippy --version` is still `0.1.98`, `rust-toolchain.toml` still pins `1.97.1` and `rustup` is still absent, but `crates/api/transport-api` no longer contains a `chunks_exact` site, and `cargo clippy --workspace --all-targets -- -D warnings` with NO allow flag exited 0 on PR #111's head. So the allow is no longer needed — and a new 1.98-only lint can appear at any time, which is exactly why a red clippy here is read, never assumed to be "the known one".

**Lost a third time, 2026-09-20/25 (PR #111), and in the pipe rather than the flag.** `cargo xtask ci 2>&1 | tail -40` run in the background reported exit 0. That was `tail`'s. The pipe also DISCARDED the diagnostics: the saved output was 42 lines, so what had failed could not be recovered without a rerun. The truth was clippy (`too_many_arguments`, one real error) and five Kademlia tests. **Never pipe a verification step: redirect to a file and capture the exit on the next statement** — `cmd > "$LOG" 2>&1; echo "EXIT=$?" > "$LOG.exit"`. A background task's own exit code is its LAST command's, so it is `echo`'s there too; read the `.exit` file, not the task notification.

**Bitten the other way, 2026-09-28 (#142 supplier branch).** Five `#[expect(clippy::unused_async_trait_impl)]` passed the local gate on 1.98 but name a lint clippy 1.97 does not have, so CI's pinned 1.97.1 would fail `-D warnings` on `unknown_lints`; the supplier review caught it, not the gate. Any `#[expect]`/`allow` naming a lint must exist in the PINNED clippy. The pinned toolchain CAN be run here without touching the host: `curl -sSf -o $S/rust/rustup-init https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init`, then `RUSTUP_HOME=$S/rust/rustup CARGO_HOME=$S/rust/cargo rustup-init -y --no-modify-path --profile minimal --default-toolchain <pin> -c clippy -c rustfmt` (665 MB) and run cargo from `$S/rust/cargo/bin` with those two variables exported; it rebuilds the target from scratch. The owner then chose to move the pin to 1.98 (raised to devex-tooling on #142), which removes the skew.

**The skew ENDED 2026-09-28.** devex-tooling moved `rust-toolchain.toml` and `rust-version` to 1.98.1 on #142 (400fb1e3, the owner's call), and the host's rustc is Fedora 1.98.1, so a local `cargo clippy --workspace --all-targets --locked -- -D warnings` is now the CI gate. If the pin moves again, re-read `rustc --version` against it before trusting a local gate, and use the scratchpad rustup recipe above for any version the host does not have.

*References: known-failing-masks-new-failures*

*Observed 2026-09-25 (p2p-network-dev)*
