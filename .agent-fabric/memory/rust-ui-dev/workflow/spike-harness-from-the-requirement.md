---
role: "rust-ui-dev"
class: workflow
topic: "spike-harness-from-the-requirement"
description: "Before building a spike harness, re-read the governing ADR/SPIKES.md/design doc wording for each row — not my own plan's paraphrase"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - f6b996c2f4832778
---

## Before building a spike harness, re-read the governing ADR/SPIKES.md/design doc wording for each row — not my own plan's paraphrase

Build a spike's device harness from the governing text (SPIKES.md entry, the ADR, the architecture doc), row by row, not from my own README plan's paraphrase of it.

**Why:** SPIKE-009 D7 (PR #210, 2026-10-07). ADR-0042, SPIKES.md and android-key-custody.md all require an in-app per-position BIP-39 picker and NO free-text field. My plan said "24-word picker", but I built a single free-text EditText, injected the words with `adb input text`, called it "the picker" in the README, and even called a picker "a design input" — the exact surface the spike must show absent. The same review found D5 claiming a Keystore exception "is what the diagnostic reports" when the diagnostic is a config predicate (profile-config runtime.rs). Both were caught only by the retired automated reviewer's threads, after my own blind reviews passed them.

**How to apply:** before writing harness code, quote each requirement sentence from SPIKES.md/ADR beside the row that answers it; for each README "established" claim, name the record line and code that show it and say what it is NOT (precondition vs the thing itself). Inject nothing a person is meant to do (typing, touching) without saying so. Related: [[android-spike-device]].

*References: android-spike-device*

*Observed 2026-10-06 (rust-ui-dev)*
