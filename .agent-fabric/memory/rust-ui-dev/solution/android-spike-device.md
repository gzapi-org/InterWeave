---
role: "rust-ui-dev"
class: solution
topic: "android-spike-device"
description: "SPIKE-008/009 test device: no StrongBox, Smart Switch path, no emulator (no /dev/kvm), release after use; the owner unlocks it"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 2676716fc87ae1aa
---

## SPIKE-008/009 test device: no StrongBox, Smart Switch path, no emulator (no /dev/kvm), release after use; the owner unlocks it

The owner provides a DEDICATED Samsung Android 11 (API 30, arm64-v8a) test phone for SPIKE-008/009; the full matrix is allowed (reboots, network toggles, backup/restore) on the harness apps only.

Facts that drive the design:
- NO StrongBox feature: the keystore is the TEE only.
- Samsung Smart Switch is present: an OEM device-transfer path outside Android backup rules, so the SPIKE-008 restore matrix must include it.
- No /dev/kvm on the host, so there is no emulator; the phone is the only device.
- The phone has a secure lock and the owner holds the credential. If it locks, it cannot be unlocked over adb; tell the owner. The owner also re-enables wireless debugging and supplies the connection details each time.
- Deep sleeping on Samsung kills the foreground service with no sticky restart (see [[stage17-spikes-merged]]).

RELEASE RULE (owner, 2026-10-09): after any device run or probe, restore any changed setting (stay-on, backup transport), uninstall the harness apps, then `adb disconnect` + `adb kill-server`; see [[release-phone-after-testing]].

*References: release-phone-after-testing, stage17-spikes-merged*

*Observed 2026-10-09 (rust-ui-dev)*
