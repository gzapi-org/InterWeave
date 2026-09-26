---
role: "p2p-network-dev"
class: workflow
topic: "host-cargo-is-system-1-98-no-rustup"
description: "On develop-qzapp the only cargo is the distro's 1.98 and rustup is absent, so rust-toolchain.toml's 1.97.1 pin is not honoured locally — the \"known chunks_exact lint\" is toolchain skew, and clippy must run with that ONE lint allowed…"
tier: 1
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - a30c1b6d1f8a2159
---

## On develop-qzapp the only cargo is the distro's 1.98 and rustup is absent, so rust-toolchain.toml's 1.97.1 pin is not honoured locally — the "known chunks_exact lint" is toolchain skew, and clippy must run with that ONE lint allowed rather than read past

Measured 2026-09-17 on `develop-qzapp` as `p2p-network-dev-01`: `which -a cargo` gives `/usr/sbin/cargo` and `/usr/bin/cargo` only, `rustup` is not on PATH, and `cargo clippy --version` is `0.1.98` while `rust-toolchain.toml` pins `channel = "1.97.1"`. `cargo xtask ci` therefore runs clippy on 1.98 and fails on `clippy::chunks_exact_to_as_chunks` in `crates/api/transport-api` — the "known 1.98-vs-1.97.1 lint" the Stage 11 handover names. CI (which honours the pin) is green on the same tree.

**Why:** on PR #84 round 17 the known lint hid a second, real clippy error (`assertions_on_constants` in a new test) in the first CI-equivalent run; only re-running `cargo clippy --workspace --all-targets -- -D warnings -A clippy::chunks_exact_to_as_chunks` exposed it. This is the [[known-failing-masks-new-failures]] shape with its local cause named.

**How to apply:** after `cargo xtask ci` reports the clippy step red, re-run clippy with exactly that one lint allowed and require exit 0; never grep the output. `cargo-deny` was also absent on this account (installed 2026-09-17 with `cargo install cargo-deny --locked`), so `check_dependencies.sh` and `check_vendored_advisories.sh` exit 2 ("not installed", not a policy verdict) until it is. A rustup install honouring the pin would remove the skew; that is devex-tooling's call (the pins are theirs), raise it there rather than editing the pin.

**Lost a second time, 2026-09-19 (PR #102 round 3).** The CI-equivalent script I ran for four PRs invoked `cargo clippy --workspace --all-targets -- -A clippy::chunks_exact_to_as_chunks` WITHOUT `-D warnings`, read its exit code (0) and `tail -1`, and never read the log; an unused variable in a new wire test sat as a warning in that log and would have failed CI's `-D warnings` `rust` context. `cargo xtask ci`'s own clippy stops at the first crate (the known lint in `transport-api`) and never reaches the test crates, so it cannot show it either. The script template is exactly: `cargo clippy --workspace --all-targets -- -D warnings -A clippy::chunks_exact_to_as_chunks; echo "clippy exit $?"` and the exit must be 0 — the flag is the whole check.

**The known lint stopped firing by 2026-09-25; the skew did not.** `cargo clippy --version` is still `0.1.98`, `rust-toolchain.toml` still pins `1.97.1` and `rustup` is still absent, but `crates/api/transport-api` no longer contains a `chunks_exact` site, and `cargo clippy --workspace --all-targets -- -D warnings` with NO allow flag exited 0 on PR #111's head. So the allow is no longer needed — and a new 1.98-only lint can appear at any time, which is exactly why a red clippy here is read, never assumed to be "the known one".

**Lost a third time, 2026-09-20/25 (PR #111), and in the pipe rather than the flag.** `cargo xtask ci 2>&1 | tail -40` run in the background reported exit 0. That was `tail`'s. The pipe also DISCARDED the diagnostics: the saved output was 42 lines, so what had failed could not be recovered without a rerun. The truth was clippy (`too_many_arguments`, one real error) and five Kademlia tests. **Never pipe a verification step: redirect to a file and capture the exit on the next statement** — `cmd > "$LOG" 2>&1; echo "EXIT=$?" > "$LOG.exit"`. A background task's own exit code is its LAST command's, so it is `echo`'s there too; read the `.exit` file, not the task notification.

*References: known-failing-masks-new-failures*

*Observed 2026-09-25 (p2p-network-dev)*
