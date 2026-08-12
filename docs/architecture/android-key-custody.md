# Android transport-key custody

Status: architecture/design only.

## Constraint

The network identity is fixed to an exact exportable 32-byte Ed25519 secret so BIP-39 entropy encoding can recover the same PeerId. Android Keystore should therefore protect the key **at rest** without changing the Ed25519/libp2p identity format.

## Selected v1 Android design

Do not generate a different Android-native signing identity. Instead:

1. generate/import the normal 32-byte Ed25519 secret under the existing identity contract;
2. generate an AES-256 wrapping key in `AndroidKeyStore`;
3. prefer hardware-backed TEE/StrongBox storage when the device supports the required AES-GCM key properties, but do not make StrongBox availability a correctness requirement;
4. encrypt the 32-byte Ed25519 secret with authenticated AES-GCM under the Keystore key;
5. persist only the versioned ciphertext/envelope and public expected PeerId in app-private storage;
6. unwrap into process memory only while the Rust transport service needs the identity;
7. zeroize best-effort temporary secret buffers after import/runtime shutdown.

The Android Keystore wrapping key is non-exportable; the Ed25519 secret necessarily becomes available to the Rust process after unwrap because rust-libp2p needs the exact portable key material. Therefore this improves at-rest extraction resistance but is **not** an HSM/non-exportable Ed25519 identity claim.

## Unlock policies

Two explicit policies are supported:

### background-compatible

Keystore wrapping key does not require per-operation biometric confirmation. This allows a user-enabled foreground service to restart/unlock while Android permits it. Device/app storage and Keystore protections are the at-rest boundary.

### user-presence

Keystore unwrap requires current device credential/strong biometric authorization. If the process dies, background reachability cannot resume until the user authenticates locally. This intentionally trades availability for stronger local-use control.

The UI must show this tradeoff rather than silently weakening user-presence policy after a restart. The combination `availability_mode=stay-reachable` plus `key_unlock_policy=user-presence` is valid, but it has a mandatory derived diagnostic `background_restart_requires_user_authentication=true`: after process/service restart the endpoint cannot become reachable until local user authentication succeeds. The UI must not describe that configuration as continuously reachable across restart.

## Recovery

The portable 24-word recovery phrase remains the disaster-recovery path and represents the same 32-byte secret. It is never stored by the app after display/import. Phrase display/import requires explicit local flow with the transport service stopped/profile exclusively locked; `expected_peer_id` verification remains mandatory under ADR-0033.

### Recovery UI hardening

Android recovery screens are treated as private-key surfaces, not ordinary text forms:

- recovery runs in a dedicated non-exported Activity/task whose manifest sets `android:excludeFromRecents="true"`; its window sets `FLAG_SECURE` before any phrase material is rendered and keeps it set for the complete phrase display/import lifetime. The release harness must verify that phrase material never appears in screenshots, screen recording/non-secure displays, or Recents/task snapshots on supported Android versions;
- phrase import uses an **in-app BIP-39 word-list picker** for each of the 24 positions. Standard v1 does not offer a free-text 24-word field and therefore does not require a third-party IME to observe the phrase;
- if an implementation temporarily invokes an IME for word filtering/accessibility, it requests no suggestions/autocorrect and `IME_FLAG_NO_PERSONALIZED_LEARNING`, but this is defense in depth rather than a guarantee that an arbitrary IME behaves correctly;
- there is **no clipboard copy/paste path** for the full phrase or individual phrase words in standard v1, even after an explicit gesture;
- accessibility announcements, analytics, crash reports, debug logs, saved-instance state, notification text, UI test snapshots, and application telemetry must never contain phrase words;
- phrase words and derived seed material are removed from the UI/view-model state immediately after successful verification/import or cancellation, subject to normal best-effort memory-zeroization limits.

Restoring a phrase on a second device is migration/disaster recovery, not permission to run the same PeerId concurrently on multiple devices.

### Android backup and device-transfer policy

Standard v1 does **not** use Android Auto Backup, cloud backup, or device-to-device transfer as an identity or message-history recovery mechanism. Packaging must set an explicit backup posture and explicit extraction rules rather than relying on platform defaults:

- the wrapped identity envelope, expected PeerId record, wrapping metadata, recovery-flow temporary state, transport configuration/trust material, and human SQLite database are excluded from both cloud backup and device-transfer extraction;
- standard-v1 packaging sets `android:allowBackup="false"` and also supplies explicit Android 12+ `android:dataExtractionRules` plus Android 11-and-lower `android:fullBackupContent` rules as defense in depth. The Android 12+ rules explicitly exclude the sensitive/app-history domains from both `cloud-backup` and `device-transfer`, because `allowBackup=false` alone is not treated as a portable guarantee that every manufacturer disables device-to-device transfer;
- restoring/reinstalling the APK without a valid local Keystore wrapping key and identity envelope never creates a partial identity restore or silently generates a replacement for an established profile; onboarding reports the profile as unconfigured/recovery-required;
- HumanChat history is not uploaded or transferred by Android system backup in standard v1. Any future user-selected cloud backup/synchronization is a separate application feature with its own encryption, privacy, retention and threat-model ADR; it is not enabled by relaxing these platform backup exclusions.

## Failure behavior

- Keystore key missing/invalidated -> fail identity unlock; do not generate a replacement silently for an established profile;
- ciphertext/authentication failure -> fail closed;
- expected PeerId mismatch after unwrap -> fail closed;
- user-presence denied/cancelled -> remain offline;
- hardware-backed capability absent -> use Android Keystore software/TEE availability per platform policy, report protection level diagnostically; do not change PeerId.

## Spike requirement

SPIKE-009 must verify the exact Android Keystore AES-GCM wrapping flow, key invalidation/device-lock behavior, hardware-backed capability reporting, lifecycle restart paths, the `user-presence + stay-reachable` diagnostic, secure recovery-screen/IME/clipboard behavior, backup/device-transfer exclusion, and that rust-libp2p receives exactly the same 32-byte seed used by desktop/recovery fixtures.
