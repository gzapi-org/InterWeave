---
role: "p2p-network-dev"
class: solution
description: "Why libp2p-autonat is built from third_party/ with one patch, and what that obliges on every libp2p bump"
tier: 2
knowledge_scope: full
distilled_at: "2026-09-17"
origin:
  - agent: user
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - 9b4be488620bda68
---

## Why libp2p-autonat is built from third_party/ with one patch, and what that obliges on every libp2p bump

ADR-0051 (2026-09-09, PR #85). The workspace builds `libp2p-autonat` 0.15.0 from `third_party/libp2p-autonat/` via `[patch.crates-io]`, not from the registry.

**Why.** The released v2 client tests a candidate address ONCE: a result leaves it `Received`/`Failed`, the tick sweeps only `Untested`, re-reporting only raises a score, and `validate_addr` (the one public mutator) sets `Received`. So one address, one server, one probe per process — ADR-0035's two distinct observers is unreachable and AUTONAT.md §5's refresh has no mechanism. Upstream `master` is identical.

**The patch is one method**: `Behaviour::retest(&Multiaddr) -> bool`, returning a tested candidate to the sweep. WHEN to call it is `ReachabilityManager`'s (refresh / second observer / retry). It also recovers the two stuck-`Pending` paths: a server reporting success with no dial-back received, and a request dropped for want of a handler slot.

**Obligations this creates:**
- A path-patched crate is invisible to Dependabot **and to cargo-deny**. MEASURED with `atty 0.2.14` (RUSTSEC-2021-0145 + -2024-0375): as a registry dep `cargo deny check advisories` FAILS on both; path-patched to a copy of the same source it prints `advisories ok`. A patched crate has no `source`/`checksum` in `Cargo.lock` and the advisories check skips it. `tools/checks/check_vendored_advisories.sh` is the compensating control — it resolves each vendored crate at its version from the REGISTRY in a throwaway workspace and runs the advisory check there. Do not assume any lockfile-driven tool sees a path-patched crate.
- Every libp2p bump re-vendors the matching tarball, re-applies `INTERWEAVE.patch`, re-records the sha256 in `license_exempt.txt` and the ADR.
- `third_party/` is a landing zone ADR-0045 did not enumerate; a subdirectory without a `license_exempt.txt` entry fails `check_license_headers.sh`.
- A byte-equality guard against the pristine tarball is a NAMED FOLLOW-UP, not built — it needs the tarball available to CI.

**Verified before deciding**: `[patch.crates-io]` with a `path` source passes `cargo-deny check sources licenses bans` (deny.toml bars git and unknown registries, not paths); MIT was already on the allow-list; `license_exempt.txt` was empty until this.

**Why:** a registry dependency could not satisfy an accepted ADR, and the alternatives (amend ADR-0035 to one observer, write our own client, git dependency) were each rejected for a recorded reason.
**How to apply:** before bumping libp2p or touching AutoNAT, read ADR-0051 and check `INTERWEAVE.patch` still applies. See [[autonat-client-crate-facts]], [[stage-11-progress]].

*References: autonat-client-crate-facts, stage-11-progress*
