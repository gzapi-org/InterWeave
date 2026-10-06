# SPIKE-009 — Android exact-key custody

Android Keystore wrapping, invalidation and background/user-presence behavior.

Do not treat experiments placed here as production implementation. Evidence and final decision must be recorded against [`architecture/roadmap/SPIKES.md`](../../architecture/roadmap/SPIKES.md); the verdict is architect-cto's to write there, not this file's.

**Status: the HOST HALF has run (2026-10-06); the DEVICE HALF has run (2026-10-06: D1–D7, four recorded parts; D6b repeated in part 4 with an addition only).** No verdict is recorded; the verdict is architect-cto's.

## The host half: what was established

[`harness/`](./harness) wraps and unwraps the Ed25519 seed through a proposed v1 envelope with AES-256-GCM, against the production identity derivation (`interweave-profile-identity`, pinned by revision). The wrapping key is a software AES-256 key standing in for an AndroidKeyStore key. The recorded run is [`harness/REPRODUCTION-2026-10-06.log`](./harness/REPRODUCTION-2026-10-06.log): **all 20 checks held.**

| id | observation |
|---|---|
| H1 | The fixture's 32-byte seed (the TEST-ONLY public all-zero vector, `fixtures/identity/`) derives the frozen PeerId through the production derivation, with no KDF in between (ADR-0033). |
| H2 | Under both unlock policies a wrap and unwrap give back the exact seed, which derives the same PeerId. Two wraps of one seed differ, because each wrap uses a fresh IV. |
| H3 | Every single-bit flip in every byte of the 66-byte envelope (528 flips) is refused; none yields a seed. By kind: 481 failed authentication, 32 had a bad magic, 8 an unknown version, 7 an unknown policy. |
| H4 | An envelope truncated, extended or empty is refused. |
| H5 | Another wrapping key — what a Keystore key invalidated and made again looks like to the envelope — fails authentication. |
| H6 | The header is checked or authenticated, never trusted. Even a VALID policy byte swapped (user-presence to background) fails authentication. |
| H7 | An envelope belongs to one profile. Unwrapped for another PeerId, it fails authentication. A seed that authenticates but derives another identity is refused after re-derivation. |
| H8 | 100 of 100 random identities give back their exact seed and PeerId. |

Two mutations of the envelope were made to show the checks are load-bearing: leaving the policy byte out of the associated data fails H3 and H6, and dropping the PeerId re-derivation fails H7.

## The proposed envelope: an open question, not a contract

ADR-0042 says "a versioned authenticated ciphertext wrapped by an AES-256-GCM key generated in AndroidKeyStore", and no document fixes its bytes. The harness measures this proposal, shaped so the device can produce exactly these bytes:

```text
magic "IWK1" (4) | version 0x01 (1) | policy (1) | iv (12) | ciphertext (32) | tag (16)   = 66 bytes
associated data = magic | version | policy | the profile's PeerId (UTF-8)
```

- **The IV is stored**, because an AndroidKeyStore AES-GCM key generates its own IV on encryption: randomized encryption is required by default, so the caller cannot choose the IV.
- **The header and the PeerId are associated data**, through Android's `Cipher.updateAAD`, so a valid policy swapped for the other, or a ciphertext moved to another profile, fails authentication. An unknown version or policy byte is refused earlier, by the header check, before any decryption (H3's Version and Policy counts).
- **The PeerId is not stored in the envelope.** It is the profile's, kept beside the envelope, and unwrap also re-derives it from the seed and compares.

Whether this layout becomes the format is a decision for the contract's owner, after the device half has shown AndroidKeyStore produces it. **Verified on the test device (D2, below):** a Keystore AES-GCM cipher takes this associated data and returns a 12-byte IV and a 128-bit tag, so the device frames exactly this 66-byte layout.

## What the host half did not establish

Everything that needs AndroidKeyStore or Android itself:
- hardware backing (TEE/StrongBox) and its reporting;
- user-presence and background-compatible key modes;
- lock, reboot and process restart;
- real key invalidation (new biometrics, lock-screen removal);
- the `background_restart_requires_user_authentication` diagnostic;
- the in-app 24-word picker and the phrase-exfiltration checklist (clipboard, IME, autofill, logs, analytics, saved state, crash artifacts).

## The device half: where it stands

- **Device:** a dedicated test device is reachable over adb: a Samsung SM-A405FN running Android 11 (API 30). It advertises **no StrongBox feature**; its keystore is TEE-backed (HAL `mdfpp`). So StrongBox is recorded as absent on this device, not as tested.
- **Toolchain:** the pinned Android toolchain (`tools/host/android/`), checked with `android-toolchain.sh --check` before the run.
- **Harness:** [`device/`](./device). It is a Rust core (`device/harness`, the production derivation pinned at 7e2978d1, built with `cargo-ndk`) and a Java receiver (`device/app`) driven from the shell with `am broadcast`. `device/build.sh` builds the APK by hand from the pinned toolchain, without Gradle. The Rust core frames the envelope, checks the header and re-derives the PeerId, while the Keystore does the AES-GCM. `device/harness/tests/layout.rs` holds the two halves to one layout on the host: what the host half wraps, the device framing splits and re-frames byte for byte, and the device's associated data authenticates the host's ciphertext. Swapping version and policy in the device's associated data fails it.

## The device half: what was established (part 1)

The recorded run is [`device/REPRODUCTION-2026-10-06.log`](./device/REPRODUCTION-2026-10-06.log), with the phone's own result lines verbatim.

| id | observation |
|---|---|
| D1 | StrongBox requested: refused, `StrongBoxUnavailableException`, so absent on this device, not tested. A background-compatible key and a user-presence key (credential or Class-3 biometric, 60 s) are both generated **inside secure hardware**, origin generated. The user-presence key's authentication is **enforced by secure hardware**, and it reports `invalidated_by_biometric_enrollment: false`, as documented for a timed key. |
| D2 | The Keystore returns a 12-byte IV, a 128-bit tag and 48 sealed bytes. The device frames the 66-byte envelope with the header `IWK1 01 00` (background) or `IWK1 01 01` (user presence). The intact envelope gives back the seed, which re-derives the frozen fixture PeerId on the device. |
| D3 | 528 single-bit flips: **no seed**. 481 failed authentication (`AEADBadTagException`), 32 had a bad magic, 8 a bad version and 7 a bad policy, the host half's tally exactly. A valid policy byte swapped, and the envelope opened for another profile's PeerId, both fail authentication. |
| D4 (process) | After `am force-stop`, a fresh process unwraps the background key: the seed. Inside the user-presence window, a killed and restarted process unwraps that key too. |
| D5 | The user-presence key, with no person having unlocked within its 60 s: the wrap is refused `UserNotAuthenticatedException`. A person then unlocks with the device credential. Within the window the key wraps and unwraps, across a process kill. 75 s later, with no person, the unwrap is refused `UserNotAuthenticatedException`, while the background key still unwraps (the control). That refusal is what the `background_restart_requires_user_authentication` diagnostic reports. The harness neither replaces the key nor makes an identity, and the production mapping to that diagnostic is not this harness's to show. |

## The device half: what was established (part 2)

The recorded run is [`device/REPRODUCTION-2026-10-06b.log`](./device/REPRODUCTION-2026-10-06b.log). The captures it measures are not committed, because the recent-apps captures also show another app's content on the device.

| id | observation |
|---|---|
| D4 (lock) | With the keyguard showing, the background key unwraps (the seed), and the user-presence key is refused `UserNotAuthenticatedException`. |
| D7 | The fixture phrase typed into the picker restores the fixture PeerId through the production parse; only "valid"/"invalid" is recorded. Each check against the unprotected control screen: **screenshot**: control legible; secure, `screencap` refused (an empty file). **Screen recording**: control legible; secure, the activity is black, but **the IME window is not covered by `FLAG_SECURE`** (it showed no typed text in the frame measured). **Recents**: control's snapshot shows the words; secure's is blank. **IME**: the secure field reaches Samsung Keyboard as `NO_SUGGESTIONS` and `NO_PERSONALIZED_LEARNING`; the control's carries neither. **Autofill**: under the control, Samsung Pass received a fill request and Samsung's augmented service two; under the secure screen, neither received one, though a session flagged augmented-only was opened. **Logs and app storage**: no phrase word in logcat or under the app's data directory. |

**Two check records per Enter, in the part-2 log:** the picker's editor-action listener runs on both the key-down and the key-up of a hardware Enter (`input keyevent 66` sends both). That is read from Android's `TextView` behaviour, and the record pairs 11 ms and 37 ms apart fit it. So each mode recorded two checks. On the secure screen the field is cleared after the first, so the second parsed an empty field and recorded `invalid`. The first record of each pair is the result. The harness code is left as it ran, so the record matches the code.

**What D7 did not establish:**
- **Whether Samsung Keyboard honours the flags:** what it learns, suggests or uploads is not observable from adb.
- **That the IME window cannot leak:** it sits outside `FLAG_SECURE`, so a keyboard showing key-press popups could put keystrokes into a recording or a screenshot. An in-app word picker that needs no IME would close it, and that is a design input for the contract's owner.
- **The clipboard:** copy, cut and paste are refused by construction (no action mode), not measured, since Android 11's shell cannot read the clipboard.
- **Saved state and crash artifacts:** the field saves no instance state by construction; only the app's own storage was searched, not system_server's.
- **The logcat and storage searches had no positive control:** the harness never writes a word, so they show that nothing leaked there, not that the search would see a leak.

## The device half: what was established (part 3)

The recorded run is [`device/REPRODUCTION-2026-10-06c.log`](./device/REPRODUCTION-2026-10-06c.log). A person at the phone made each change; the harness recorded the result.

| id | observation |
|---|---|
| D4 (reboot) | After `adb reboot` and the person's first unlock, the background key unwraps. The user-presence key made before the reboot is refused `UserNotAuthenticatedException` once its 60 s have passed, and unwraps (the seed) inside the window after a later unlock. Both survive a reboot. |
| D6a | Fresh keys of both modes: each wrapped, and the timed one also unwrapped (the background one was not unwrapped before the event; its unwraps after it are what show it sound). The person changes the screen lock to Swipe (`device_secure: false`). The background keys, fresh and old, still unwrap: the control. Both user-presence keys, fresh and old, are **invalidated**. With the PIN set again, they stay invalidated: it is permanent. **On this device the invalidation surfaces as `UnrecoverableKeyException` from `KeyStore.getKey`**, before any cipher, and not as the `KeyPermanentlyInvalidatedException` this plan expected; `KeyInfo` cannot be read for the key either. A production check written for the latter alone would miss it. |
| D6b | With one Class-3 fingerprint enrolled: a fresh per-operation key (biometric, every use; `invalidated_by_biometric_enrollment: true`) wraps and unwraps through `BiometricPrompt`, one touch each. A fresh timed key wraps and unwraps inside the 60 s a touch opens. These are the controls. Then the enrollment changed. **This device's Settings offered no way to add a fingerprint without re-enrolling the first** (the person went through Screen lock type; two enrolled after). After the change, BOTH keys are invalidated (`UnrecoverableKeyException: User changed or deleted their auth credentials`), and the background key still unwraps. |

**What D6b did not establish:** that adding a fingerprint, and only that, invalidates the per-operation key and spares the timed one. The event this device's Settings allowed included re-enrolling the first fingerprint, and possibly a pass through the credential flow. So the timed key's invalidation cannot be attributed: Android documents enrollment invalidation for per-operation keys only, and a credential change or an emptied fingerprint set invalidates both. Part 4 repeated it with an addition only. What holds either way: no key that was invalidated ever gave back a seed, and the background-compatible key was untouched by every credential and biometric change.

## The device half: what was established (part 4)

The recorded run is [`device/REPRODUCTION-2026-10-06d.log`](./device/REPRODUCTION-2026-10-06d.log). It is D6b again, with an addition only: the Fingerprints page did offer "add" once two were enrolled.

| id | observation |
|---|---|
| D6b (repeat) | Fresh keys made with two fingerprints enrolled. The person ADDS a third from Settings → Biometrics and security → Fingerprints, without changing the credential (count 2 → 3). The **timed key survives**: its `KeyInfo` reads and it wraps inside the window the person's PIN entry opened. The **per-operation key is invalidated**: its `KeyInfo` still reads, but `Cipher.init` throws **`KeyPermanentlyInvalidatedException`**. The background key unwraps: the control. This is Android's documented behaviour, and it attributes part 3's loss of the timed key to the re-enrollment path, not to the addition. |

**Two invalidation signals, both seen on this device:** `UnrecoverableKeyException` from `KeyStore.getKey` after a credential change (D6a, part 3), and `KeyPermanentlyInvalidatedException` from `Cipher.init` after a biometric enrollment (part 4). Recovery has to be entered on either.

**Limit:** the per-operation key in part 4 was never used before the event. Its prompt went unanswered, so its pre-event health rests on its generation and `KeyInfo`, not on a completed unwrap. Part 3's per-operation key did complete one, and the same configuration is used here.

## The device half: the plan, as written before the run

[`device/`](./device), above. This is the plan the run followed, kept as written; what was measured is in parts 1–4 above, and where they differ the parts are the record. Two of its expectations were not measured: the control field's IME-suggestion leak (only its flags were read) and the clipboard check. Results are written to app-private storage and read back over adb (`run-as`). The fixture seed is the TEST-ONLY public vector only.

| id | what | recorded |
|---|---|---|
| D1 | Generate an AndroidKeyStore AES-256-GCM key in each mode: background-compatible (no user authentication); user-presence with a timeout (`setUserAuthenticationParameters(t > 0, …)`); and, only if the device's biometric is Class 3, per-operation biometric (`setUserAuthenticationParameters(0, AUTH_BIOMETRIC_STRONG)`, used through a `BiometricPrompt` `CryptoObject`), the one mode Android invalidates on enrollment. | `KeyInfo.isInsideSecureHardware` (the API 30 report) and the key's properties; StrongBox requested and refused (`StrongBoxUnavailableException`), recorded as absent, not as tested |
| D2 | Wrap the fixture seed: the Keystore generates the IV, the header and PeerId go in through `updateAAD`; Rust frames the envelope. | the IV and tag lengths; the envelope byte-for-byte against the host layout; Rust unwrap with the AES key unavailable is impossible, so the check is the Keystore decrypt giving back the seed and Rust re-deriving the frozen PeerId |
| D3 | Tamper with the stored envelope: H3's single-bit flips over adb, through the Keystore. | every flip refused (`AEADBadTagException` or a header refusal); none yields a seed |
| D4 | Durability: process restart (`am kill`), screen lock and unlock, reboot. | unwrap succeeds after each in the background mode; in the user-presence mode, what is asked of the person and when |
| D5 | Background restart in user-presence mode: the service restarts with no person present. | `UserNotAuthenticatedException`, surfaced as the diagnostic `background_restart_requires_user_authentication`, never as a new identity |
| D6a | Lock screen removed, on FRESH keys of the background and timed modes, with no biometric involved. | `KeyPermanentlyInvalidatedException` on the timed key; the background key still unwraps (the control); the app enters recovery and never makes a new key over the profile silently |
| D6b | Precondition: one fingerprint is ALREADY enrolled when the fresh keys are generated, so that the event is a second enrollment (Class 3 where the per-operation key is made, since Android generates a key that needs authentication on every use only while such a biometric is enrolled). The event is a SECOND fingerprint enrolled, with the secure lock screen kept throughout, on FRESH keys of the timed and (if Class 3) per-operation modes, each having wrapped and then unwrapped the seed once (the per-operation key through `BiometricPrompt`) before the event. Removing the lock screen deletes the enrolled biometrics and invalidates every auth-bound key itself, so it is never part of this row. | the timed key SURVIVES (Android documents enrollment invalidation for per-operation keys only, so its survival is recorded as the documented non-invalidation); the per-operation key throws `KeyPermanentlyInvalidatedException`; with no Class-3 biometric, only the timed key's survival is recorded |
| D7 | The 24-word picker on a screen with `FLAG_SECURE`, entering the fixture's phrase. | `screencap` and `screenrecord` black; nothing in the clipboard (`cmd clipboard`/`dumpsys clipboard`); the IME sees a field that disables suggestions and learning; no autofill request (`dumpsys autofill`); `logcat` holds no word; no saved-instance or crash artifact holds one (`run-as` file search for each word) |

**Each check has a control** that shows its observation was live: an envelope left intact unwraps (D3), a field without the flags DOES leak to the IME's suggestions and the screenshot (D7), a background-mode key restarts unattended (D5), a background-mode key survives the lock-screen removal that invalidates the timed one (D6a), and the fresh per-operation key unwraps through `BiometricPrompt` before the enrollment it is then invalidated by (D6b).

**This device cannot answer:**
- StrongBox: the device has none.
- The current API's Keystore behaviour: it needs a newer device.
- Class-3 biometric binding and enrollment invalidation of a per-operation key (D6b), if the device's biometric is not Class 3.
