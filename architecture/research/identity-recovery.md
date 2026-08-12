# Identity recovery research

Primary references reviewed for the architecture:

- Bitcoin BIP-39, `bip-0039.mediawiki` — 128..256-bit entropy-to-mnemonic construction, English wordlist guidance, NFKD handling, PBKDF2 wallet-seed step, and documented shortcomings including short checksum/no internal versioning.
- Trezor `python-mnemonic` vectors — includes the standard 256-bit all-zero entropy -> 24-word `abandon ... art` fixture.
- SLIP-0039 — threshold mnemonic sharing for an arbitrary master secret, explicitly positioned as successor work to BIP-39 and capable of 256-bit master secrets.
- rust-libp2p `libp2p-identity` — persistent fixed keys are loaded through the libp2p identity/portable binary representation boundary rather than by coupling callers to an external Ed25519 implementation type.

## Architectural conclusion

Use BIP-39 only as a well-known **human encoding of the exact Ed25519 secret entropy**. Do not use the wallet-specific PBKDF2 stage. This avoids creating a second derivation layer and permits an existing Ed25519 identity to be exported after creation.

The format is project-labelled/versioned outside the 24 words because BIP-39 itself does not carry application version semantics. The expected PeerId is retained as public verification metadata to compensate for the short mnemonic checksum.

SLIP-0039 is not implemented in v1, but it can later share the same exact 32-byte Ed25519 secret and therefore recover the same PeerId.

## Reference URLs

- https://github.com/bitcoin/bips/blob/master/bip-0039.mediawiki
- https://github.com/trezor/python-mnemonic/blob/master/vectors.json
- https://github.com/satoshilabs/slips/blob/master/slip-0039.md
- https://docs.rs/libp2p-identity/latest/libp2p_identity/
