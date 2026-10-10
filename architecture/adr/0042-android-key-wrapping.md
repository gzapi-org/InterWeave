# Android protects the portable Ed25519 seed with Android Keystore wrapping

**Status:** Accepted for Android first-party implementation

## Context

ADR-0033 requires exact recovery of the existing 32-byte Ed25519 secret. Android Keystore provides strong non-exportable key storage for supported algorithms, but the libp2p Ed25519 seed still must be supplied to the Rust identity implementation and remain recoverable by the portable mnemonic.

## Decision

Store the transport Ed25519 secret on Android only as a versioned authenticated ciphertext wrapped by an AES-256-GCM key generated in `AndroidKeyStore`. Prefer hardware-backed protection when available. Support explicit `background-compatible` and `user-presence` unlock policies. Never silently substitute a different Android-native identity key algorithm.

**The format and the seam (A 2026-10-10, on SPIKE-009's Result 5 and §20 step 6's build).** The wrapped secret is the **IWK1 v1 envelope**: `IWK1 (4) | version 0x01 (1) | policy (1) | iv (12) | ciphertext (32) | tag (16)`, 66 bytes, with associated data `magic | version | policy | PeerId (UTF-8)` — the header is authenticated and never trusted, checked in SPIKE-009's order before any cipher call, and the PeerId is re-derived from the unwrapped seed and compared. The on-disk record is ONE owner-only file under the embedded identity directory, `identity.iwk1` = envelope (66) `|` PeerId (UTF-8, at most 128 bytes), written with an exclusive create at provisioning so two first runs cannot both win; provisioning refuses when a record exists. The platform cipher is a **seam** (`SeedCipher`: seal, open; `Send + Sync`, its calls may block) the Android Keystore implements in the app's platform crate; its failure set is CLOSED and is contract — `KeyInvalidated`, `KeyMissing`, `UserNotAuthenticated`, `Authentication`, and `Unavailable(String)` for any other platform error, which is returned to the caller for its log and decides nothing (unlock answers "try again", never recovery) — adding a member is a contract change. Unlock yields the profile identity the embedded launch takes, or one of `Unprovisioned`, `RecoveryRequired(cause)`, `UserNotAuthenticated` (stay offline; a user-presence window that has not been opened is not a loss of the key) and `Unavailable`; only `RecoveryRequired` enters recovery, none mints a key over an established profile, and on `KeyInvalidated` or `KeyMissing` the record stays unchanged byte for byte and the identity returns only through its own recovery phrase. `seal` deletes the policy's Keystore key and generates a new one, so a restore after invalidation never reuses the invalidated alias. Zeroizing the seed is best-effort and is stated as a limit: the plaintext lives in the Java heap as a byte array for one call. A header bit flip that selects the other valid policy asks for that policy's key and is refused as `KeyMissing` when it is absent and as an authentication failure when present — both refusals, both tested.

## Alternatives considered

Plain app-private seed file; generate unrelated Android Keystore EC identity; store the mnemonic; require biometric on every network operation; cloud custody.

## Consequences

The same PeerId/recovery phrase works across platforms. Android gains materially better at-rest protection, but once unwrapped the Ed25519 secret is present in the app process and is not claimed to be hardware-nonextractable.

## Security implications

Keystore/ciphertext failure is fail-closed. User-presence mode intentionally sacrifices unattended restart. Phrase theft remains full identity compromise. Android phrase screens use secure-window protection, an in-app BIP-39 picker and no clipboard path. Standard-v1 Android system backup/device-transfer excludes identity/recovery/configuration and the entire human-store rather than treating platform backup as recovery. Any future explicit message backup follows ADR-0044 and includes only inbound unread/receiver-kept content.

## Operational implications

Device restore/Keystore invalidation may require mnemonic recovery. Protection level should be visible in local diagnostics without exposing secrets.

## Implementation implications

SPIKE-009 validated (closed PASS 2026-10-09, `SPIKES.md`) Android Keystore AES-GCM wrapping, lifecycle, exact 32-byte seed import and the two invalidation signals, within the bounds its Result records; among what it did not establish (`SPIKES.md`'s "Not established" paragraph lists all) — the recovery screen's exfiltration controls (the per-position picker, `FLAG_SECURE`, the IME-free path, the clipboard refusal), the user-presence/stay-reachable restart diagnostic, the client-side clauses of the stage gate (enters recovery, never mints, restores from the phrase), StrongBox and API 34+ reporting (another device), and the D6b re-run isolating the biometric-enrollment invalidation — are §20 steps 7–8's instrumented tests, carried there by name; backup and device-transfer exclusion was measured by SPIKE-008 (B1, B3) (A 2026-10-10). The custody path's host tests over real files with a software cipher seam are step 6's proof; the Keystore implementation of the seam and its device test land with the app shell. Recovery tooling remains stopped-runtime/offline.

## Revisit conditions

Revisit if Android provides a portable/hardware Ed25519 primitive compatible with exact libp2p PeerId recovery or if a future identity-version ADR intentionally changes the transport key format.

## Amendments

Full notes: [`history/0042-amendments.md`](./history/0042-amendments.md).

| Date | Amendment | Effect |
|---|---|---|
| 2026-10-10 | The IWK1 v1 envelope is the identity record's format; the platform cipher is a seam with a closed failure set; SPIKE-009's client-side clauses are steps 7–8's tests | Decision: the IWK1 v1 envelope (66 bytes, authenticated header, PeerId in the associated data and re-derived on unwrap) is the format; the record is one owner-only `identity.iwk1` = envelope `|` PeerId, exclusive-create at provisioning; `SeedCipher` is the seam with the closed five-member failure set (`Unavailable` decides nothing); unlock yields the identity or one of `Unprovisioned | RecoveryRequired | UserNotAuthenticated | Unavailable`, only `RecoveryRequired` entering recovery, none minting; `seal` rotates the alias; zeroization is a stated limit. Implementation implications: SPIKE-009 reads as closed, its client-side clauses carried to steps 7–8's tests. |
