# SPIKE-008 — Android execution / store-policy viability

Android foreground-service/lifecycle/backup/recovery-screen platform behavior.

Do not treat experiments placed here as production implementation. Evidence and final decision must be recorded against [`architecture/roadmap/SPIKES.md`](../../architecture/roadmap/SPIKES.md); the verdict is architect-cto's to write there, not this file's.

**Status: PART 1 RUN (2026-10-06: L1, L3, L4, L5, S1, E1, R1–R3, B1, B3).** Not yet run: L2, L6, L7, L8, L9, P1, B2, B4, B5 (below). No verdict is recorded; the verdict is architect-cto's.

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

## The harness

- **[`harness/`](./harness):** the Rust core, the **production human store** (`interweave-human-store`, pinned at d019ac06) over raw JNI. It seeds the three durable states through the store's own API, drives the transitions a client drives, and reports a census by TEST label, including the store's own `backup_eligible_content`. `harness/tests/store.rs` runs the same cycle on the host, with a reopen.
- **[`app/`](./app):**
  - a launcher Activity, the only start path;
  - the recovery Activity, non-exported, in its **own task** (`taskAffinity`, started with `FLAG_ACTIVITY_NEW_TASK`), `excludeFromRecents`, `FLAG_SECURE` set before content;
  - a `START_STICKY` foreground service of type `remoteMessaging` that traces a 30 s heartbeat, its lifecycle and the default network;
  - a shell-only receiver (DUMP) that writes the excluded files and an included control, and drives the store.
- **[`build.sh`](./build.sh):** a hand build from the pinned toolchain, without Gradle. It takes the target SDK and the backup posture: `allowBackup=false` with both rule forms (**standard v1**, `android-key-custody.md`), or `true` (the rules alone).

## What was established (part 1)

The recorded run is [`REPRODUCTION-2026-10-06.log`](./REPRODUCTION-2026-10-06.log), from a clean install of each variant.

| id | observation |
|---|---|
| L1 | Opening the launcher starts the foreground service. `startForeground` is granted with type 512 (`remoteMessaging`, an API 34 type) on this API 30 device, without enforcement. The notification is ongoing and no-clear (flags `0x62`), on a low-importance channel, naming nothing. |
| L3 | Screen off, battery reported unplugged, deep Doze forced for 180 s: the 30 s heartbeat ran on time throughout, the process lived, and no network loss was reported. |
| L4 | Backgrounded, the process SIGKILLed (`killProcess` from inside, standing in for a low-memory kill the shell cannot send to another app): the service was back about 1.4 s later as a sticky restart (null intent), in the foreground at once. |
| L5 | `am force-stop`: no process and no service record for 90 s. The launcher, a person's path, started it again. |
| S1 | The production store, with 3 pending, 3 unread and 3 kept, after driving U1 read without Keep, K1 unkept and P1 terminal: P2, P3 · U2, U3 · K2, K3, the same after the kill and after the force-stop. The store opens on Android only in an owner-only directory: the app's `files/` is `0771`, which the store refuses, and the harness makes `files/human/` `0700`, so the production client must do the same. |
| E1 | The store's `backup_eligible_content`: K1–K3 and U1–U3 after seeding, K2, K3, U2 and U3 after the transitions, never a pending row. This matches the host test. |
| R1 | The recovery screen: `screencap` refused (an empty file). The launcher control: captured. |
| R2 | The recovery task's recents snapshot is blank while it is the current task. Once left (Home), it is not shown in recents, though the system still holds the task; the one card shown resumed the launcher. An earlier build that opened recovery inside the launcher's task had that task listed with recovery on top: `excludeFromRecents` acts on a task's root, so the requirement's "dedicated Activity/task" is load-bearing. |
| R3 | `screenrecord` of the recovery screen: black. The control: not black. |
| B1 | Standard v1 (`allowBackup=false`): `bmgr backupnow` through the local transport answers "Backup is not allowed". The rules alone (`allowBackup=true`): the backup succeeds. |
| B3 | The rules alone: after backup, uninstall and reinstall, Android restored `marker/control.txt` (the included control) and nothing else. The wrapped identity, config, recovery scratch and human store were absent, and the store reopened empty. |

**What part 1 did not establish:**
- **L3, the network in Doze:** the trace shows the process and the callback, not whether a connection could be made; no traffic was attempted. Forced Doze is not natural Doze.
- **L4, a real low-memory kill:** a self-SIGKILL stood in.
- **R2 on other launchers and API levels:** One UI on API 30 only.
- **B3, the onboarding:** the harness has no onboarding. That a reinstall without the identity enters recovery-required and never manufactures a PeerId (`android-key-custody.md`, `human-client-android.md`) is the production client's to show; what was shown is that nothing sensitive came back.
- **B1 through Google's transport (B2), device transfer (B4) and Smart Switch (B5):** not run. B2 uploads to the owner's account and waits on the owner's word; B4 and B5 need a second device.

## The harness, as planned before the run

`spikes/spike-008/harness/`: one Android app, built at two target SDKs (30 and the current Play target).
- A foreground service that holds a small Rust core: the production identity, the production human store (`interweave-human-store`, the store under test) and a session stand-in. The service type is declared both ways, and the service writes a timestamped lifecycle trace to app-private storage.
- A launcher Activity, the only user-visible start path.
- A non-exported recovery Activity with `android:excludeFromRecents="true"` and `FLAG_SECURE` set before any content is drawn.
- Backup rules: `fullBackupContent` and `dataExtractionRules`. System backup and device transfer carry none of the human store (`RETENTION.md` §6), so both exclude the wrapped identity, the configuration, recovery temporary state and the human store. A marker file is deliberately left included, as the control.
- A seeded human store, through the store's own API: three pending outbound, three unread inbound and three kept inbound rows (the only states `RETENTION.md` §5 lets the store hold). Then, before any kill, the transitions are DRIVEN in-process: one unread row read without Keep, one kept row unkept, and one pending row taken to transport-terminal. Each transition deletes its durable copy. This tests ADR-0044's restart retention. Seeding a read-unkept or terminal row directly would test a start-up sweep the contract does not have.

The trace and the store are read back over adb (`run-as`). No result rests on what the app says about itself where adb can observe it directly.

## The experiments, as planned before the run

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
| S1 | ADR-0044 restart retention | the transitions above, then `am kill`, and separately reboot | the remaining pending, unread and kept rows survive with their content; the read-unkept, unkept and terminal rows' content is gone, read back over `run-as` with `sqlite3` as well as through the store (RETENTION conformance 11) |
| E1 | future explicit message-backup eligibility (`RETENTION.md` §6, conformance 12) | the store's `backup_eligible_content` over the S1 store, before and after the transitions, on the device | exactly the unread and kept inbound rows; no pending outbound, terminal, read-unkept or unkept row. The host control is `crates/human/store/tests/storage_mechanics.rs`'s `backup_eligible_content_excludes_pending_outbound`. This is a predicate, so the device run shows that it holds on Android's SQLite build and nothing more: no explicit backup feature exists to exercise, and none is built here |
| P1 | target-SDK / service-type policy matrix | builds at targetSdk 30 and at the current target | the manifest, the service start result, any `ForegroundServiceTypeException`, and the Play policy text quoted against the date read |

**Battery and network observations** (`dumpsys batterystats`, `dumpsys netstats`) are recorded for L2, L3 and L7 as measurements, not checks.

## What this device cannot answer

- **The current target API's runtime rules**, `remoteMessaging` and `dataExtractionRules`: they need an API 34+ device.
- **Other OEMs' power managers:** this is one Samsung device.
- **Play review itself:** the policy matrix quotes the declared requirements, and only a submission tests them.
