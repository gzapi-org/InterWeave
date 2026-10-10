---
role: "rust-ui-dev"
class: workflow
topic: "release-phone-after-testing"
description: "when device testing ends, run adb disconnect and adb kill-server so the shared Android phone is free"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 466cb4a21bc24d1f
---

## when device testing ends, run adb disconnect and adb kill-server so the shared Android phone is free

When testing on the Android phone ends, release it right away:
1. `adb disconnect` (everything).
2. `adb kill-server`.
3. Check with `pgrep -a adb` that no adb process of mine is left.

This includes an adb server that was started only to check whether the phone is reachable. `adb devices` starts a server, and it stays running.

**Why:** the owner, 2026-10-09: "when you finish the testing disconnect like you have just done so you free the resource". The phone is a shared resource. An adb server left running from a reachability check on 2026-10-08 was still up a day later.

**How to apply:**
- Do the release at the end of every device run, before reporting the run as done.
- Report the release with the run's results.
- Do it also when only probing for the device.

Related: [[android-spike-device]] [[release-disk-after-tests]]

*References: android-spike-device, release-disk-after-tests*

*Observed 2026-10-09 (rust-ui-dev)*
