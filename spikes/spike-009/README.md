# SPIKE-009 — Android exact-key custody

Android Keystore wrapping, invalidation and background/user-presence behavior.

Do not treat experiments placed here as production implementation. Evidence and final decision must be recorded against [`architecture/roadmap/SPIKES.md`](../../architecture/roadmap/SPIKES.md); the verdict is architect-cto's to write there, not this file's.

**Status: the HOST HALF has run (2026-10-06); the DEVICE HALF has not.** No verdict is recorded. AndroidKeyStore itself — TEE/StrongBox, user presence, lock/reboot/process restart, invalidation, the phrase UI — has not been exercised on any device yet, and nothing below speaks for it.

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

Whether this layout becomes the format is a decision for the contract's owner, after the device half has shown AndroidKeyStore produces it. **Assumed, not yet verified on a device:** that a Keystore AES-GCM cipher takes associated data and returns a 12-byte IV and a 128-bit tag in this arrangement.

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
- **Toolchain:** the Android toolchain is being provisioned by devex-tooling, as a shared install that needs a root step by the owner.
- **Harness:** the device harness will reuse this envelope layout, so the two halves meet in the same bytes.

## The device half: the plan (NOT RUN)

`spikes/spike-009/harness-android/` (to be written once the toolchain is installed): a small app whose Rust core is this harness's `envelope` and the production derivation, built with `cargo-ndk` for arm64-v8a and called over JNI. Keystore operations are Kotlin; every byte decision is Rust's, so the device and host halves judge one envelope with one code path. Results are written to app-private storage and read back over adb (`run-as`), and the fixture seed is the TEST-ONLY public vector only.

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
