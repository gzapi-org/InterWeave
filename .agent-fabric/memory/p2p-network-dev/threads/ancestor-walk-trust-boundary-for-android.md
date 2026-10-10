---
role: "p2p-network-dev"
class: threads
topic: "ancestor-walk-trust-boundary-for-android"
description: "Owed at Stage 17 §20's first Android package: resolve_private_dir_as / resolve_guarded_dir_as take a composition-supplied trust boundary (ADR-0028 A 2026-10-08 second, #220 0d84cfd7)"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - a85407aec512f702
---

## Owed at Stage 17 §20's first Android package: resolve_private_dir_as / resolve_guarded_dir_as take a composition-supplied trust boundary (ADR-0028 A 2026-10-08 second, #220 0d84cfd7)

architect-cto ruled 2026-10-08 (seq 23025, written on #220 at 0d84cfd7): the ancestor walk added in #224 refuses every Android app-private directory (/data, /data/data are 0771 system:system on AOSP). The walk must stop at a TRUST BOUNDARY the composition supplies to profile-config: canonicalised once when supplied; the walk from the directory's parent stops at the canonical component equal to it; links above it are not judged; the boundary and everything below are judged as today; a refusal names the boundary when the boundary itself fails.
- Daemon and transportctl supply "/" (no change on Linux).
- The Android embedded runtime supplies the app data dir as the platform reports it (never hard-coded) and keeps its private dirs directly under it at 0700 (files/ is 0771 app:app and would be refused).

**Why:** without it the embedded runtime cannot start on Android.
**How to apply:** nothing before §20's first Android package; in that batch, add the parameter (profile-config persist.rs walk + resolve_guarded_dir_as), the daemon's and transportctl's "/", and tests. Related: [[pr224-carried-hardening]].

*References: pr224-carried-hardening*

*Observed 2026-10-08 (p2p-network-dev)*
