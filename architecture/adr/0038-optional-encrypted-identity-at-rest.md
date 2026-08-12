# Optional encrypted software identity at rest

**Status:** Accepted direction for v2.x; standard v1 remains filesystem-only pending SPIKE-007

## Context

The exportable Ed25519 software identity is currently protected at rest by owner-only filesystem permissions. That is simple and interoperable but offers no extra protection when a laptop disk/profile directory is copied or the same OS account is compromised. Mnemonic recovery solves loss, not local key-file confidentiality.

## Decision

Keep standard v1 `identity.key_protection=filesystem-only`. Define an explicit v2.x option for a **passphrase-encrypted, versioned key envelope** that decrypts to the exact same portable Ed25519 identity and therefore preserves the same PeerId. Do not invent a bespoke cipher/KDF format. SPIKE-007 must select and pin a maintained audited format/library providing a memory-hard password KDF and authenticated encryption, along with unlock UX and migration semantics, before the option becomes selectable.

The passphrase is separate from the 24-word recovery phrase and is never stored in normal config, logs, network messages, Claude/MCP, endpoint directory, or ordinary daemon IPC.

## Alternatives considered

Owner-only plaintext forever; use the recovery phrase itself as encryption password; custom Argon2id+AEAD container immediately; OS keychain only; HSM-only identities.

## Consequences

v1 remains operationally simple and unattended. v2.x can improve laptop/disk-at-rest protection at the cost of an unlock path and another secret. Recovery remains exact-key backup and does not depend on remembering the encryption passphrase if the recovery phrase is available.

## Security implications

Encrypted-at-rest storage raises the cost of offline key-file theft but does not protect an unlocked daemon, a malicious same-user process that can obtain the passphrase/secret from memory, or a stolen mnemonic. KDF/AEAD misuse is avoided by requiring an audited existing format/library rather than project-designed cryptography.

## Operational implications

Interactive/headless unlock, restart automation, passphrase loss, parameter upgrades, and atomic re-encryption require explicit operator UX. Backups must distinguish encrypted live key file from mnemonic recovery material.

## Implementation implications

SPIKE-007 is mandatory before introducing a selectable encrypted mode. The envelope must version format/KDF parameters, fail closed on authentication error, decrypt to the exact portable Ed25519 secret boundary, use atomic owner-only writes, and avoid passphrase/plaintext leakage to config/logs/crash reports. Hardware-backed/non-exportable identities remain a separate future backend.

## Revisit conditions

Revisit after SPIKE-007 evidence or when an OS-native keychain/HSM backend becomes a concrete target. Any change that alters PeerId derivation requires a separate identity migration ADR.
