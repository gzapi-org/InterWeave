# Android protects the portable Ed25519 seed with Android Keystore wrapping

**Status:** Accepted for Android first-party implementation

## Context

ADR-0033 requires exact recovery of the existing 32-byte Ed25519 secret. Android Keystore provides strong non-exportable key storage for supported algorithms, but the libp2p Ed25519 seed still must be supplied to the Rust identity implementation and remain recoverable by the portable mnemonic.

## Decision

Store the transport Ed25519 secret on Android only as a versioned authenticated ciphertext wrapped by an AES-256-GCM key generated in `AndroidKeyStore`. Prefer hardware-backed protection when available. Support explicit `background-compatible` and `user-presence` unlock policies. Never silently substitute a different Android-native identity key algorithm.

## Alternatives considered

Plain app-private seed file; generate unrelated Android Keystore EC identity; store the mnemonic; require biometric on every network operation; cloud custody.

## Consequences

The same PeerId/recovery phrase works across platforms. Android gains materially better at-rest protection, but once unwrapped the Ed25519 secret is present in the app process and is not claimed to be hardware-nonextractable.

## Security implications

Keystore/ciphertext failure is fail-closed. User-presence mode intentionally sacrifices unattended restart. Phrase theft remains full identity compromise.

## Operational implications

Device restore/Keystore invalidation may require mnemonic recovery. Protection level should be visible in local diagnostics without exposing secrets.

## Implementation implications

SPIKE-009 validates Android Keystore AES-GCM wrapping, lifecycle and exact 32-byte seed import. Recovery tooling remains stopped-runtime/offline.

## Revisit conditions

Revisit if Android provides a portable/hardware Ed25519 primitive compatible with exact libp2p PeerId recovery or if a future identity-version ADR intentionally changes the transport key format.
