---
role: "rust-ui-dev"
class: solution
topic: "stage17-spikes-merged"
description: "SPIKE-008/009 device halves merged (#210, #214) and the human store's RETENTION 8 fix -- what landed and what each did not establish"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - c9ebffea124240d7
---

## SPIKE-008/009 device halves merged (#210, #214) and the human store's RETENTION 8 fix -- what landed and what each did not establish

Both Android spikes are recorded and merged: SPIKE-009 device half #210 (2026-10-06), SPIKE-008 device half #214 (c16cf0f9, 2026-10-07). Verdicts are architect-cto's, in SPIKES.md -- not written by rust-ui-dev.

#214 also changed PRODUCTION code: crates/human/store now runs secure_delete, truncates the WAL after every release (transport_terminal, mark_read, unkeep), at open of an existing store (existing = PRAGMA user_version through the connection, read AFTER the owner-only checks), and at close; rewrites once (VACUUM, settings key released_content_scrubbed) a store written before; a busy checkpoint is StoreError::LogNotTruncated. RETENTION.md 8 reworded and 9 case 15 added (architect-cto rulings 01a11548, 01a11574). Cost on the A40: 14-22 ms median per release vs 4-5 ms. Device-verified up to 03434804; the later review fixes (F1/F2, permission order) are host- and desktop-e2e-proven only (phone released).

Findings that bind Stage 17's Android client: Samsung Deep sleeping kills the FGS with no restart; a Wi-Fi reconnect is a NEW network; the store dir must be a subdirectory the store creates (files/ is 0771; never chmod); the recovery Activity needs its own task; invalidation surfaces as UnrecoverableKeyException (credential change) AND KeyPermanentlyInvalidatedException (biometric enrolment); the IME window is outside FLAG_SECURE -> in-app picker (already required by ADR-0042; the D7 harness tested a free-text stand-in). Related: [[spike-harness-from-the-requirement]], [[android-spike-device]].

*References: android-spike-device, spike-harness-from-the-requirement*

*Observed 2026-10-07 (rust-ui-dev)*
