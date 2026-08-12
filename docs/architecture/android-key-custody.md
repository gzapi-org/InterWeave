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

The UI must show this tradeoff rather than silently weakening user-presence policy after a restart.

## Recovery

The portable 24-word recovery phrase remains the disaster-recovery path and represents the same 32-byte secret. It is never stored by the app after display/import. Phrase display/import requires explicit local flow with the transport service stopped/profile exclusively locked; `expected_peer_id` verification remains mandatory under ADR-0033.

Restoring a phrase on a second device is migration/disaster recovery, not permission to run the same PeerId concurrently on multiple devices.

## Failure behavior

- Keystore key missing/invalidated -> fail identity unlock; do not generate a replacement silently for an established profile;
- ciphertext/authentication failure -> fail closed;
- expected PeerId mismatch after unwrap -> fail closed;
- user-presence denied/cancelled -> remain offline;
- hardware-backed capability absent -> use Android Keystore software/TEE availability per platform policy, report protection level diagnostically; do not change PeerId.

## Spike requirement

SPIKE-009 must verify the exact Android Keystore AES-GCM wrapping flow, key invalidation/device-lock behavior, hardware-backed capability reporting, lifecycle restart paths, and that rust-libp2p receives exactly the same 32-byte seed used by desktop/recovery fixtures.
