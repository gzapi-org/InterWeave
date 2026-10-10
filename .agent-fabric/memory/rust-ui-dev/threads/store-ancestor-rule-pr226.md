---
role: "rust-ui-dev"
class: threads
topic: "store-ancestor-rule-pr226"
description: "j32 — human store routed through profile-config's ancestor walk (PR #226); Android /data 0771 system conflict raised to architect-cto"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 26d6c017021f8391
---

## j32 — human store routed through profile-config's ancestor walk (PR #226); Android /data 0771 system conflict raised to architect-cto

2026-10-08, job j32 DELIVERED: PR #226 MERGED 36e6adc6 (7 work, 7 fix, armed on the owner's word); architect-cto told (seq 24489). History: PR #226 (branch develop-qzapp/rust-ui-dev-01/fix/store-ancestor-rule): commits 77533edf (store) and 956fc69b (desktop classify). 2 work commits, persistence = security boundary, so arming needs a review of the head AND the owner's word.

How it works now: `private_dir` in crates/human/store/src/store.rs runs resolve_guarded_dir on the nearest existing ancestor BEFORE creating, then resolve_owned_private_dir. The db and its companions are opened under the resolved path. Refusals return StoreError::DirectoryNotPrivate. The store now depends on interweave-profile-config; this reverses the old "store must not depend on configuration" comment, because the ADR says the walk runs once. The funnel refuses the state directory ITSELF if it is a link; it judges only links above it.

OPEN: ADR-0028 A 2026-10-08 as written refuses Android's app-private dirs: AOSP init.rc makes /data and /data/data system:system 0771. Sent as an OBSERVATION to architect-cto-01 (seq 22974, id 01a11b1f-2246-7143-9cba-5d2808d526df), proposing that the walk stop at the app's data dir. Not measured on the device; the phone was released. This must be settled before §20's first Android package. Also: once #226 lands, ADR-0028's clause "(the human store's open is carried)" is architect-cto's to mark done.

RULED 2026-10-08 (architect-cto seq 22980, reply to mine): option (a). The walk judges up to a TRUST BOUNDARY the composition supplies: `/` for the daemon and transportctl, and the app's data dir as Context reports it on Android (never a hard-coded /data/data/<pkg>). p2p-network-dev adds the parameter in profile-config's resolve_*_as with §20's first Android package. The new ADR-0028 amendment goes on #220. The store side is mine (queued job). When #226 lands, NAME IT to architect-cto so the carried clause is marked done.

ADDED 2026-10-08 (architect-cto seq 23022, #220 at 0d84cfd7). (1) AOSP creates files/, cache/ and code_cache/ at 0771 with the app's gid, so the Android store's private dir must sit DIRECTLY under the app data dir, created at 0700, never under files/ or cache/. (2) The platform reports /data/user/0/<pkg> but the canonical path is /data/data/<pkg>, so the boundary is canonicalised once and the walk stops at the canonical component equal to it. Both bind j35.

j35's dependency (fabric-coordinator seq 23917, 2026-10-08): no p2p job existed. The parameter was offered to both p2p-network-dev holders (02 now also on InterWeave); whoever takes it tells me its job. Keep j35 blocked. Once agent-fabric#118 lands, re-block it with --on-request <the taker's message id>.

Lock class (seq 24026, p2p-network-dev-01, branch develop-qzapp/p2p-network-dev-01/fix/judge-before-create): profile-config's create_private_dir will itself judge the nearest existing ancestor and then create one component at a time, the same shape as #226's private_dir/create_each. When it lands, j36 does two things: add the creates-nothing assertion in apps/human-desktop/tests/startup.rs, and replace the store's create_each with create_private_dir, so the code exists in one place.

Lesson: a compose-then-fill-then-send line chained with `;` sent an EMPTY skeleton when the fill failed (seq 24483; INFO's sections are CONTEXT/NOTES, not INFO). Always chain fill && send.

Related: [[stage17-spikes-merged]] [[android-spike-device]]

*References: android-spike-device, stage17-spikes-merged*

*Observed 2026-10-08 (rust-ui-dev)*
