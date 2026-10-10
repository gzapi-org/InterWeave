---
role: "p2p-network-dev"
class: threads
topic: "j48-embedded-runtime-step-1"
description: "§20 step 1 (j48) LANDED in #241 (0cf71c2f, 2026-10-09): embedded host, TrustBoundary + runtime root, 4th conformance runner; two P3s carried (walk_judged comments to j50, log_filter DEBUG pin to j49)"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 714648125ad4d609
---

## §20 step 1 (j48) LANDED in #241 (0cf71c2f, 2026-10-09): embedded host, TrustBoundary + runtime root, 4th conformance runner; two P3s carried (walk_judged comments to j50, log_filter DEBUG pin to j49)

Ruled 2026-10-09 (architect-cto seq 32946, on my proposal 32914 + 32937):
- Crate `crates/transport/embedded` (interweave-transport-embedded), mine; no Android/JNI/UI type; deps composition, profile-config, profile-identity, local-client-api only. EmbeddedHost::start(EmbeddedLaunch{app_data_dir, profile, identity injected}), own tokio runtime, deployment must be embedded-android, ProfileLock, InProcessBinding (no second adapter), stop(grace).
- Layout doc edit is architect-cto's SUPPLY onto my branch after the crate's first commit.
- ONE root `<app_data>/interweave` 0700 created by the runtime, config/data/state/cache under it 0700. TrustBoundary carried by ProfilePaths, canonicalised once; TrustBoundary::root() for daemon/transportctl/claude-channel/human-desktop.
- Tests: fourth runner tests/local-client-conformance/tests/embedded.rs; boundary itself 0771/not ours refused naming it; a private dir under <app_data>/files/ refused IN the embedded runner (gate (d)).
- aarch64 target check: devex-tooling's CI job (my REQUEST seq 32998); until then the closing record says unverified. This account has no rustup (tools/host/android/rust-android.sh is the per-account installer — the owner's to run).
- Seams to rust-ui-dev-01 (REQUEST seq 33004): Service API, HumanStore::open(&ProfilePaths), audit filter (theirs: logcat writer).
- Landing: all on #241's branch (fix/nss-zero-budget); a9d6e60e widened profile-config's linux cfgs to android.

State 2026-10-09 ~11:00Z: built on #241 at 48b0b31d (8 work of mine + 1 j47 + rust-ui-dev's store supply folded unrebased, 62bdfb35..884dd1bb). Gate (d) ruled (seq 33218): ADR-0028's walk ACCEPTS Android files/ (0771, group-of-one), so TrustBoundary carries a runtime root and every _within fn refuses outside it (text before walk incl. "..", resolved after). Next: xtask ci, two blind reviews (requests in scratch: profile-config+composition; embedded+runner+store), architect's layout-doc supply, devex aarch64 job (seq 32998, unanswered).
Lesson: TrustBoundary's internal _in fns are unconfined on purpose -- create_private_dir judges the boundary itself (above the root) as nearest existing ancestor; confine only the public _within entries.

Armed 2026-10-09 at 93db8777 after 3 review rounds (2 blind parts, 2 re-reviews). CARRY to j50 (next profile-config change, circuit-route supply onto p2p-network-dev-02's j6 branch):
- P3: persist.rs walk_judged -- the comments "Above the boundary nothing is judged..." (error arm) and "A link above the boundary is the platform's..." (link block) need "before the walk has entered the boundary".
- P3: crates/transport/embedded/tests/log_filter.rs -- pin the widest DEBUG arm: daemon "        tracing::Level::DEBUG\n    } else {" vs copy "        Level::DEBUG\n    } else {" (that's the embedded crate, not profile-config: carry to j49's PR instead if j50 stays profile-config-only).
- Risks: after entering, platform links reached via absolute targets judged to / (device run); boundary.name on uncovered path; WORKERS doesn't bound tokio blocking pool; peer cache dir not judged (architect's call).
- Tell devex-tooling when #241 lands (their j60's second commit is automatic).

Merged 2026-10-09 as 0cf71c2f; devex told (seq 44612). j48 delivered.

RE-ROUTED 2026-10-09: both carried P3s (walk_judged comments; log_filter DEBUG pin) go to j49's PR, which touches profile-config (wildcard-only rule) and embedded (network_changed) -- j50 stays the one grammar change dev-02 asked for.

*Observed 2026-10-09 (p2p-network-dev)*
