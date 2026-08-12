# ADR-0033 — Recover software PeerId with an optional 24-word Ed25519 mnemonic backup

**Status:** Accepted

## Context

A persistent PeerId is a core transport identity. Losing the profile private key currently means losing that identity; silent regeneration is forbidden. Human-operated deployments benefit from a paper/offline recovery mechanism analogous in usability to cryptocurrency recovery phrases, but importing Bitcoin wallet key semantics into a transport identity would be unnecessary and error-prone.

BIP-39 provides a familiar entropy-to-word encoding for 128–256 bits, including a checksum and widely available English wordlist. Its mnemonic-to-wallet-seed PBKDF2 stage, optional passphrase semantics, short checksum, and lack of internal versioning are not required here. SLIP-0039 is relevant future work for threshold backup of an arbitrary master secret.

rust-libp2p intentionally treats fixed identity keys through its own identity API/portable key representation rather than exposing implementation-specific external key objects. The architecture therefore needs a stable secret-byte boundary and a deterministic restore rule.

## Decision

1. Initial software profile identities use **Ed25519**.
2. Define optional recovery format `cp2p-ed25519-bip39-entropy-v1`.
3. The format encodes the **exact 32-byte Ed25519 secret seed** as 256-bit BIP-39 entropy using the English wordlist/checksum, yielding exactly 24 words.
4. **Do not use BIP-39 PBKDF2 mnemonic-to-seed derivation and do not support a BIP-39 passphrase** for this format.
5. Backup records pair the secret 24 words with public `expected_peer_id` and format/algorithm labels. The PeerId is mandatory verification metadata when available because the 24-word BIP-39 checksum is only 8 bits.
6. Export/restore are offline local identity operations with daemon/profile identity lock exclusivity. Recovery material never crosses local daemon IPC, MCP, Channel, or the P2P network.
7. Recovery of an established profile must reproduce the expected PeerId exactly; mismatch fails closed and never silently becomes a new identity.
8. Future SLIP-0039 support may split the same exact 32-byte secret seed without rotating the PeerId, but is not v1 implementation scope.
9. Hardware-backed/non-exportable identities are a future alternative identity backend and may intentionally have no mnemonic export.

Normative details and fixtures are in `contracts/IDENTITY-RECOVERY.md`.

### Verify-only drill and recovery scope

`transportctl identity verify` is a read-only offline drill: decode phrase, derive PeerId, compare to expected public metadata, discard secret material, and perform no private-key write/profile mutation/network activity.

The phrase recovers **identity only**. Complete profile disaster recovery additionally requires a backup of `config.yaml` for trust allowlists, endpoints/default route, discovery/Kademlia/bootstrap settings, desired channels, and local policy. Runtime caches, leases, dedup state, messages, and human-application history are not restored by the transport phrase.

## Alternatives considered

No recovery; raw hex/base64 private-key backup only; use the complete BIP-39 mnemonic-to-seed PBKDF2 process; derive a new libp2p key from a separate mnemonic root with HKDF; invent a custom wordlist/checksum; make SLIP-0039 mandatory in v1; transmit recovery material through admin IPC; auto-store recovery words in profile config.

## Consequences

Existing Ed25519 profile keys can be represented after creation; backup does not need to become the source of key generation. Restore reproduces the exact same PeerId. Operators receive a familiar 24-word medium, but the phrase is private-key-equivalent and must be protected accordingly.

Choosing Ed25519 for initial software identity narrows v1 identity-key algorithm flexibility intentionally. A future algorithm change requires a new recovery-format identifier and explicit identity migration/rotation semantics.

## Security implications

Possession of the phrase is possession of the transport identity. Theft permits PeerId impersonation and all endpoint traffic allowed to that PeerId until remote trust is revoked/updated. BIP-39's checksum is typo detection, not authentication; expected-PeerId verification is required when backup metadata survives.

The format is deliberately not a brainwallet creation scheme: profile creation generates the Ed25519 seed with a CSPRNG and offers no user-selected-word path. Restore cannot prove historical entropy provenance; it validates the exact format/checksum and expected PeerId instead. Recovery phrases are never accepted from remote messages and never logged. No BIP-39 wallet passphrase is supported, avoiding an additional forgotten-secret failure mode and avoiding dependence on the wallet-specific PBKDF2 construction.

## Operational implications

Profile initialization/backup UX should strongly prompt the operator to record and verify recovery material, while allowing environments to choose raw/HSM backup instead. Recovery drills can verify a phrase against its expected PeerId without joining the network. Identity rotation requires creation of a new backup and retirement/destruction of old copies.

A future SLIP-0039 option can improve distributed custody but increases UX/implementation surface and must remain optional.

## Implementation implications

Phase 1 adds identity-algorithm/recovery data types and golden fixtures but no production backup implementation is required by the architecture-only phase. Future `transportctl` backup/restore code uses the libp2p Ed25519 secret binary boundary, BIP-39 English entropy encoding/decoding, atomic owner-only key writes, secret-buffer zeroization where practical, and strict expected-PeerId validation.

No daemon IPC method is added for export/import. Claude/human data-plane clients receive no recovery capability.

## Revisit conditions

Revisit when adding hardware-backed keys, a non-Ed25519 software identity algorithm, threshold recovery, encrypted recovery exports, signed identity rotation/continuity, or if a stronger standardized mnemonic format with materially better version/checksum properties becomes a deployment requirement.
