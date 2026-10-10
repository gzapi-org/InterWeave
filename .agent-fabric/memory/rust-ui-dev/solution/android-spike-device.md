---
role: "rust-ui-dev"
class: solution
topic: "android-spike-device"
description: "SPIKE-008/009 test device (Samsung A40, adb over network) and its baseline facts; toolchain provisioning is devex-tooling's"
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

## SPIKE-008/009 test device (Samsung A40, adb over network) and its baseline facts; toolchain provisioning is devex-tooling's

2026-10-06: owner connected a DEDICATED test device for SPIKE-008/009 (full matrix allowed: reboots, network toggles, backup/restore -- on the harness app only). adb over network: 192.168.100.6:35707, Samsung SM-A405FN (a40eea), Android 11 / API 30, first_api 28, arm64-v8a, security patch 2023-03-01, FBE, verified boot green.
Baseline: NO StrongBox feature (TEE keystore HAL "mdfpp"); Doze light+deep enabled; backup: Google BackupTransportService active, LocalTransport, GMS D2dTransport; Samsung Smart Switch present (OEM device-transfer path outside Android backup rules -> SPIKE-008 restore matrix must include it); Samsung power manager (com.samsung.android.lool, sm.policy) present.
No /dev/kvm on host -> no emulator. Owner chose: devex-tooling provisions the Android SDK/NDK/JDK/Gradle (REQUEST 01a110a4-880a, seq 13273), I do not install it. Job j20.
Raw baseline kept in session scratch only; commit it under spikes/spike-00x/ with the harness.
Related: [[release-disk-after-tests]]
devex took it (seq 13279): next job after gzapp #1043, reply will name paths/env/pins.
2026-10-06 14:23 (seq 13373): devex staging versions now; shared install /opt/android-sdk (owner's choice); OWNER must run installer as root in the template, add group androiddev with rust-ui-dev-01, restart the AppVM. Rust: rustup + aarch64-linux-android in my account via devex's one-command script.
2026-10-06: owner asked me to keep the phone unlocked. Set global stay_on_while_plugged_in=7 (was 0; screen_off_timeout 600000 unchanged) -- restore both after the spikes. Phone has a secure lock (credential set): if it locks I cannot unlock over adb, tell the owner. Watch: Monitor polling isKeyguardShowing/AC/mStayOn every 60 s, re-arm at expiry.
SPIKE-009 HOST HALF done 2026-10-06 on local branch develop-qzapp/rust-ui-dev-01/spike/spike-009-host (NOT pushed): spikes/spike-009/harness (standalone, empty [workspace], pinned a41e1f3d = parent of recording commit 015fafc9 which touches only the spike dir) + .gitignore exception commit ddb656ae. 20 checks: fixture seed->golden PeerId; proposed 66-byte envelope "IWK1"|ver|policy|iv12|ct32|tag16, AAD=header+PeerId; 528 bit-flips refused; wrong key/len/version/policy/swapped policy/other PeerId/wrong-identity seed refused; 100 random round trips. Spike conventions: dir must be harness/, log REPRODUCTION-*.log, lock needs a named .gitignore exception. BLOCKER: check_spike_locks provenance only walks origin/main -> a first recorded run can't pass in PR CI; OBSERVATION 01a111a5-231c to devex (seq 13376). Envelope layout is a PROPOSAL (contract owner decides after device half).
devex REPLY 01a111aa (seq 13379): (1) spike-lock first-run fix d48b991f (walks origin/main AND HEAD) on develop-qzapp/devex-tooling/feat/android-toolchain -> hold my spike branch until it merges, then fold main and push. (2) staged pins: platforms 30+36, build-tools 30.0.3/36.0.0, NDK 28.2.13676358, cmdline-tools 23.0, platform-tools 37.0.1, Temurin 17.0.20.1+1 (/opt/android-sdk/jdk; set org.gradle.java.home), Gradle 9.6.0 (sha256 in pins file); env after owner's template install: ANDROID_HOME=/opt/android-sdk, ANDROID_NDK_HOME=.../ndk/28.2.13676358, ANDROID_JDK_HOME. (3) Rust: tools/host/android/rust-android.sh (rustup 1.29.1, 1.98.1 + aarch64-linux-android, cargo-ndk 4.1.2) -- branch not yet pushed; READ before running, I have no rustup today (Fedora rustc; ~/.cargo/bin has only cargo-deny, cargo-machete) -- check it does not shadow the workspace toolchain.
2026-10-06: devex PR #204 (toolchain + spike-lock fix d48b991f; owner arms). NO template install: SDK in this AppVM via bind-dirs /rw/bind-dirs/opt/android-sdk, owner runs one sudo --install. Rust side INSTALLED in my home (rust-android.sh from #204): rustup 1.29.1, 1.98.1 + aarch64-linux-android, cargo-ndk 4.1.2; ~/.cargo/bin first => cargo now rustup proxy on the pinned 1.98.1 (same version as Fedora's). After the SDK install: tools/host/android/android-toolchain.sh --check and rust-android.sh --check.

**2026-10-06 state:** #204 merged (toolchain on main, NOT yet installed on the host by fabric-coordinator; then run `tools/host/android/android-toolchain.sh --check` and once `tools/host/android/rust-android.sh`). PR #207 open (branch spike-009-host): SPIKE-009 host half + render-parity golden + SPIKE-008/009 device plans; 5 work + 9 fix commits, head 6240875c reviewed clean, 0 threads; arming needs the owner's word (or 8 work commits once the device-half harness lands). Phone's stay_on_while_plugged_in reverted to 0 once on its own — re-set to 7 if the monitor reports mStayOn=false. Lesson: Keystore invalidation rows need one cause per fresh key (lock-screen removal kills every auth-bound key; enrollment invalidates per-operation keys only; per-op key needs an enrolled Class-3 biometric to be generated).

**SPIKE-009 device part 1 (2026-10-06, branch spike/spike-009-device, local):** acd7c176 recording (device/harness pinned 7e2978d1, layout must be spikes/<n>/.../harness/Cargo.toml or the provenance phase skips it — reported to devex seq 13592), 217297c6 README, 2e6cfb26 gitignore. D1-D3, D4 process, D5 PASS; D4 lock run after (bg seed, up UserNotAuthenticated) — not yet recorded. Phone HAS a device credential (owner's PIN, unknown to me), no fingerprint enrolled, no StrongBox. Harness: am broadcast -n org.interweave.spike009/.Cmd --es cmd <c> --es alias bg|up|op; results via run-as cat files/results.jsonl. Build: bash spikes/spike-009/device/build.sh (hand build, no Gradle). Remaining needs the owner: reboot, D6a (PIN removal), D6b (2 fingerprints + BiometricPrompt activity), D7 picker activity.
herdr: second project (fabric-coordinator seq 13571), j24 queued, ~/projects/herdr cloned; one open PR per agent PER REPOSITORY (confirmed seq 13577). Owner to decide fresh session now vs after SPIKE-009.

**SPIKE-008 (2026-10-07):** branch spike/spike-008-device (pushed; no PR yet): part 1 recorded 19d905ad (pin d019ac06). Part 2 outputs saved in scratchpad s8part2-body.txt (P1, L9, L6 Deep-sleeping KILLS the FGS with no sticky restart, L7, L8 + S1 after reboot); L2 2-hour run in progress. Owner ruled B2 out: "no backup on google - by design". No mobile data on the phone (SIMs out of service). Wireless-debugging port changes on every Wi-Fi drop/reboot — the owner reads it out. Phone backup transport was switched to local and RESTORED to com.google.android.gms/.backup.BackupTransportService.

**Phone RELEASED to the owner (2026-10-07 ~08:20Z):** stay_on_while_plugged_in restored to 0, screen_off_timeout 600000 (unchanged), backup transport Google's, both harness apps uninstalled, adb disconnected, keep-awake monitor stopped. The owner's added fingerprints/PIN are theirs. Device verification of the human-store RETENTION 8 fix stopped at SPIKE-008 part 6 (before the #214 review-3 fixes F1/F2); those are proven on the host only. To use the phone again: the owner re-enables wireless debugging and reads out IP:port.
RELEASE RULE (owner, 2026-10-09): after any device run or probe, `adb disconnect` + `adb kill-server`; see [[release-phone-after-testing]].

*References: release-disk-after-tests, release-phone-after-testing*

*Observed 2026-10-09 (rust-ui-dev)*
