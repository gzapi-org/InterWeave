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
- **The header and the PeerId are associated data**, through Android's `Cipher.updateAAD`, so a changed policy, a changed version or a ciphertext moved to another profile fails authentication.
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
