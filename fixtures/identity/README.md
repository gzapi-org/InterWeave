# identity

Recovery-format vectors for `interweave-ed25519-bip39-entropy-v1` (ADR-0033).

## `ed25519-bip39-entropy-v1.json`

**TEST-ONLY KEY MATERIAL.** The all-zero entropy is the standard public BIP-39 vector with no secrecy whatsoever, and exists only to prove the reconstruction path. A real recovery phrase is private-key-equivalent: it must never enter this repository, a log, a diagnostic, a crash report, or a config fixture (`CLAUDE.md` §6, ADR-0033).

The golden proves what an implementation can silently get wrong:

| step | verified |
|---|---|
| entropy → 24 eleven-bit word indexes | yes, incl. the 8-bit SHA-256 checksum |
| entropy → Ed25519 public key | yes — the entropy **is** the seed |
| public key → libp2p PeerId | yes, identity-multihash + base58btc |

The last one matters most. The BIP-39 checksum is only 8 bits, so the PeerId — not the checksum — is what proves a restore reconstructed the intended identity. A checksum-valid phrase deriving a different PeerId is a hard failure with no "close enough" fallback.

`mnemonic` is carried as documentation from the contract; resolving indexes to words needs the 2048-word English list, and vendoring it for one vector would be a poor trade. Everything else in the file is recomputed.

Verify with:

```
python3 tools/checks/verify_fixture_vectors.py
```

It implements the derivation from `architecture/contracts/IDENTITY-RECOVERY.md`, and in particular uses the entropy directly as the Ed25519 seed. A wallet-style `PBKDF2` derivation would also produce a valid-looking PeerId — just the wrong one, recoverable by nobody — which is why that path is asserted rather than assumed.
