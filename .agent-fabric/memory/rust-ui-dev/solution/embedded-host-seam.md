---
role: "rust-ui-dev"
class: solution
topic: "embedded-host-seam"
description: "the agreed seam between p2p-network-dev's embedded runtime (§20 step 1) and my Android Service, the human store and the audit sink"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - ea03efc59bfe4fdb
---

## the agreed seam between p2p-network-dev's embedded runtime (§20 step 1) and my Android Service, the human store and the audit sink

Agreed 2026-10-09 with p2p-network-dev-01 (their REQUEST seq 33004, my reply 33010, theirs 33068). Shape seen by architect-cto at seq 32946.

The crate is `crates/transport/embedded` (interweave-transport-embedded). It holds no Android, JNI or UI type.

(i) The Service calls the host:
- `EmbeddedHost::start(EmbeddedLaunch{app_data_dir, profile, identity})` blocks. I call it off the main thread.
- `binding()` returns the InProcessBinding app-core binds to.
- `runtime()` returns the tokio Handle that drives the facade's futures. app-core has no tokio.
- `wait_shutdown_requested()` blocks. admin.shutdown uses it to ask the Service to stop.
- `stop(grace)` returns a u64: the neutral events dropped over the runtime's life. It is a diagnostic only.
- `EmbeddedRefused` is closed, not non_exhaustive: LockHeld, NotEmbedded, ProfileInvalid, DirectoryRefused, IdentityRejected, Internal. Each has a log-only detail string, and the UI shows the variant through a stable key.
- A second start() in one process is refused with LockHeld (flock is per open file description). So the Service keeps ONE process-wide handle and finds it again after a START_STICKY restart.
- Process death releases the lock.

(ii) Store: `HumanStore::open(&ProfilePaths, StoreOptions)`. The trust boundary rides inside as `paths.boundary()`, and the file name moves into the store.
- I write it as a contributor branch `develop-qzapp/rust-ui-dev-01/for/p2p-network-dev-01/store-trust-boundary`, two commits: the store, then apps/human-desktop/startup.rs.
- They fold it into #241 (`develop-qzapp/p2p-network-dev-01/fix/nss-zero-budget`) after their boundary commit's sha arrives.
- The old walk names keep walking to "/", so nothing breaks before my commit. This is job j35.

(iii) Audit: they export a filter admitting `interweave::audit` at INFO. The logcat writer and its crate are mine, and gate (g)'s device test proves it.

On-disk layout: the runtime's root is `<app_data>/interweave`, mode 0700, created by the runtime, with config/data/state/cache under it. Never under files/ or cache/.

Related: [[store-ancestor-rule-pr226]] [[stage17-spikes-merged]]

Landed on #241's branch, 2026-10-09 (relay 01a12046):
- My store range was folded unrebased at 48b0b31d.
- The Service opens the store with `HumanStore::open_profile(host.paths(), ..)`. `EmbeddedHost::paths()` is the host's own ProfilePaths, so the two cannot drift.
- Also available: `EmbeddedHost::listening()`, `log_level()`, and `interweave_transport_embedded::log_admits(target, level, profile)`, the logcat writer's filter (d4c4f9d2, 0e4b22a9).
- `HumanStore::open(path)` stays public: it fails closed on Android, and p2p-network-dev-01 ruled the change not worth it.

*References: stage17-spikes-merged, store-ancestor-rule-pr226*

*Observed 2026-10-09 (rust-ui-dev)*
