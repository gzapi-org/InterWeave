# SPIKE-008 — Android execution / store-policy viability

Android foreground-service/lifecycle/backup/recovery-screen platform behavior.

Do not treat experiments placed here as production implementation. Evidence and final decision must be recorded against [`architecture/roadmap/SPIKES.md`](../../architecture/roadmap/SPIKES.md); the verdict is architect-cto's to write there, not this file's.

**Status: NOT RUN.** This file holds the plan the device run follows. No harness exists yet, and nothing has been measured. The harness is written once the Android toolchain is installed, because an Android app nobody can build is a guess, not a harness.

## The device

A dedicated test device, reachable over adb, on which the owner allowed the full matrix: force-stops, kills, reboots, network toggles, and backup and restore, applied to the harness app only (2026-10-06).

| fact | value |
|---|---|
| model | Samsung SM-A405FN (Galaxy A40) |
| Android / API | 11 / 30, first API 28 |
| ABI | arm64-v8a |
| security patch | 2023-03-01 (an old but real floor, not the current target) |
| Doze | light and deep enabled |
| backup transports | Google `BackupTransportService` (active), `LocalTransport`, GMS `D2dTransport` |
| OEM | Samsung power manager (`com.samsung.android.lool`, `sm.policy`) and **Smart Switch** (an OEM device-transfer path outside Android's backup rules) |

**API 30 shapes the matrix**, because the declarations the spike asks about arrived later:
- The `remoteMessaging` foreground-service type exists from API 34, and `dataExtractionRules` from API 31.
- On this device, an app targeting the current Play API still runs under API 30's rules: its backup exclusions come from `fullBackupContent`, and a service type the platform does not know is not enforced.
- So the harness declares both forms, and the matrix records which one the device obeyed.
- The current-API behaviour itself needs an API 34+ device, which is named as not run here, not inferred.

## The harness (to be written)

`spikes/spike-008/harness/`: one Android app, built at two target SDKs (30 and the current Play target).
- A foreground service that holds a small Rust core: the production identity and a session stand-in. The service type is declared both ways, and the service writes a timestamped lifecycle trace to app-private storage.
- A launcher Activity, the only user-visible start path.
- A non-exported recovery Activity with `android:excludeFromRecents="true"` and `FLAG_SECURE` set before any content is drawn.
- Backup rules: `fullBackupContent` and `dataExtractionRules`. Both exclude the wrapped identity, the configuration, recovery temporary state and the human store. A marker file is deliberately left included, as the control.
- A seeded human store: pending outbound, unread inbound, kept inbound, read-and-not-kept and transport-terminal rows. This tests ADR-0044's restart retention.

The trace and the store are read back over adb (`run-as`). No result rests on what the app says about itself where adb can observe it directly.

## The experiments

| id | what | driven by | recorded |
|---|---|---|---|
| L1 | start from the launcher; the foreground notification appears | `am start`; `dumpsys activity services` | trace, notification present |
| L2 | backgrounded for 10, 30 and 60 minutes, screen on (stay-awake) and off | Home key; wait | whether the service lives; trace gaps |
| L3 | Doze forced | `cmd deviceidle force-idle`, then `unforce` | trace across idle; network reachability |
| L4 | process reclaimed | `am kill`, and memory pressure via `am send-trim-memory` | restart or not; how long until it restarts |
| L5 | force-stop, then relaunch | `am force-stop`; `am start` | nothing restarts until a person starts it; state after relaunch |
| L6 | Samsung's own restriction | the app put to "sleeping apps" / "deep sleeping" in device care | service survival under the OEM rule (documented if the setting needs a person to toggle) |
| L7 | Wi-Fi off, on, and switched; mobile data off and on | `svc wifi`, `svc data`, `cmd connectivity` | the network callbacks the service sees, in order |
| L8 | reboot | `adb reboot` | what starts on its own (nothing should, without a person) |
| L9 | notification behaviour | `dumpsys notification`; a person's swipe | the foreground notification cannot be dismissed while the service runs; content is free of identifiers |
| R1 | recovery Activity: screenshot | `screencap` while it shows | the capture is black, not the phrase |
| R2 | recovery Activity: recents | `dumpsys activity recents`; the task-switcher snapshot | not listed, or listed with a blank snapshot |
| R3 | recovery Activity: screen recording | `screenrecord` while it shows | frames are black |
| B1 | backup rules, local transport | `bmgr transport LocalTransport`; `bmgr backupnow`; inspect the archive | the excluded paths are absent, the control is present |
| B2 | Google cloud backup | `bmgr transport` to the GMS transport; `bmgr backupnow` | which rule form the device applied (API 30 means `fullBackupContent`) |
| B3 | restore into a fresh install | `bmgr restore`, or uninstall then reinstall | the app enters recovery-required onboarding; no new PeerId is made for an established profile; no identity is replaced |
| B4 | device-to-device transfer | the GMS D2D path where it can be exercised; otherwise named as not run | as B3 |
| B5 | Samsung Smart Switch | an OEM transfer to and from this device, if a second device or a PC can be used; otherwise named as not run | as B3, against an OEM path that does not read Android's rules |
| S1 | ADR-0044 restart retention | `am kill`, and reboot, with the seeded store | pending, unread and kept rows survive; read-unkept and terminal rows do not |
| P1 | target-SDK / service-type policy matrix | builds at targetSdk 30 and at the current target | the manifest, the service start result, any `ForegroundServiceTypeException`, and the Play policy text quoted against the date read |

**Battery and network observations** (`dumpsys batterystats`, `dumpsys netstats`) are recorded for L2, L3 and L7 as measurements, not checks.

## What this device cannot answer

- **The current target API's runtime rules**, `remoteMessaging` and `dataExtractionRules`: they need an API 34+ device.
- **Other OEMs' power managers:** this is one Samsung device.
- **Play review itself:** the policy matrix quotes the declared requirements, and only a submission tests them.
