# Identity recovery contract

Status: **architecture contract, v1 draft**.

This contract defines optional human-recoverable backup for a software transport identity. It does not change peer trust, endpoint identity, message encryption, or application identity semantics.

## Scope and selected identity algorithm

The initial software identity profile uses **Ed25519**. One profile owns one Ed25519 private identity key and one derived libp2p PeerId.

The recovery format exists to reconstruct the **same private key and therefore the same PeerId** after local key loss. It is not a password-reset mechanism and does not create a new identity.

Hardware-backed/non-exportable identity keys are a separate future identity backend and cannot be assumed to support mnemonic export.

## Recovery format identifier

```text
cp2p-ed25519-bip39-entropy-v1
```

The format uses only the **entropy-to-mnemonic** portion of BIP-39:

- exact entropy length: 256 bits / 32 bytes;
- English BIP-39 wordlist only;
- checksum: first `ENT/32 = 8` bits of `SHA-256(entropy)`;
- 264 bits are split into 24 11-bit word indexes;
- input/output text is normalized according to BIP-39's UTF-8/NFKD rules, although the selected English words are ASCII.

The 256-bit entropy is the **exact Ed25519 secret-key seed bytes** used to reconstruct the libp2p identity.

### Explicit non-use of BIP-39 wallet derivation

The following BIP-39 operation is **not used**:

```text
PBKDF2-HMAC-SHA512(mnemonic, "mnemonic" + passphrase, 2048)
```

There is no BIP-39 recovery passphrase in this transport format. Feeding these 24 words into a cryptocurrency wallet produces unrelated wallet material and is not a valid transport restore procedure.

The backup UI/CLI must label the phrase as a **P2P transport identity recovery phrase — not a cryptocurrency wallet seed**.

## Recovery record

A human backup should retain both the secret phrase and non-secret expected identity metadata:

```text
RecoveryRecordV1 {
  format: "cp2p-ed25519-bip39-entropy-v1",
  identity_algorithm: "ed25519",
  expected_peer_id: PeerId,
  words: exactly 24 English BIP-39 words,
}
```

`expected_peer_id` is public metadata. Its purpose is strong restore verification because the BIP-39 checksum for 256-bit entropy is only 8 bits.

The record is a bearer secret: anyone obtaining the 24 words can reconstruct the transport private key and impersonate that PeerId until peers revoke/replace trust.

## Export semantics

Recovery export is **not an IPC operation** and is never exposed to Claude Channel tools.

A future `transportctl identity backup`-equivalent operation must:

1. operate locally with the daemon stopped and the profile identity lock held exclusively;
2. load the configured profile identity key;
3. require the identity algorithm to be Ed25519;
4. extract the exact 32-byte Ed25519 secret seed through the supported libp2p identity API/portable binary representation boundary;
5. compute/confirm the current PeerId;
6. encode those 32 bytes using the format above;
7. render the 24 words only to an explicit interactive secret-output path;
8. never log, telemetry-report, crash-report, shell-history-inject, or place the phrase on clipboard without a separate explicit user gesture;
9. zeroize temporary mutable secret buffers where the implementation language/runtime permits.

Default export must not write a recovery file automatically. An explicit file-output option, if later provided, creates a new owner-only file and refuses broad permissions.

## Verify-only recovery drill

A future `transportctl identity verify` operation is the preferred routine recovery drill. It is offline/local and read-only:

1. accept a `RecoveryRecordV1` or phrase plus expected PeerId;
2. decode/NFKD-normalize and verify the 24-word BIP-39 entropy checksum;
3. recover the exact 32-byte Ed25519 secret seed in memory;
4. derive the public key and PeerId through the same identity adapter used by restore;
5. compare with `expected_peer_id` (or an explicitly supplied public PeerId);
6. report match/mismatch and discard secret buffers;
7. perform **no private-key write, no profile mutation, no daemon start, and no network activity**.

The verify operation does not require the live private-key file to be exclusively locked because it never reads or writes that key. If a profile name is supplied only to obtain surviving public expected-PeerId metadata, the tool must not enter the restore/replace path. Verification output never prints the recovered secret or normalized phrase.

## Restore semantics

Recovery import is likewise offline/local, never IPC/Channel.

A future `transportctl identity restore`-equivalent operation must:

1. require format `cp2p-ed25519-bip39-entropy-v1`;
2. accept exactly 24 English BIP-39 words after whitespace/NFKD normalization;
3. verify word membership and the BIP-39 8-bit checksum;
4. recover exactly 32 entropy bytes;
5. interpret those bytes directly as the Ed25519 secret seed — **no PBKDF2 or extra KDF**;
6. reconstruct the Ed25519 public key and libp2p PeerId;
7. if an `expected_peer_id` is available from the recovery record or surviving public profile metadata, require an exact match;
8. for an established profile, refuse replacement unless the expected old PeerId matches and the operator explicitly chooses the restore/replace path;
9. atomically write the private-key file with owner-only permissions using the implementation's current portable libp2p key serialization;
10. start no network service until the restored key has been reloaded and its PeerId revalidated.

A checksum-valid phrase that derives a different PeerId from the expected record is a hard failure. There is no "close enough" or silent new-identity fallback.

If only a phrase survives and no expected PeerId exists, it may initialize a **new empty local profile record** after displaying the derived PeerId for explicit operator verification. It must not overwrite an established profile automatically.

## Rotation and compromise

- A recovery phrase is bound to one Ed25519 key/PeerId.
- Intentional identity rotation creates a new private key and therefore a new recovery phrase.
- The old phrase continues to recover the old compromised/retired PeerId; it does not follow rotation.
- Suspected phrase disclosure is private-key compromise and requires the same out-of-band trust revocation/replacement procedure as key-file theft.
- EndpointIds are unaffected as configuration labels, but all endpoints move under the new PeerId after intentional rotation.

## Complete profile disaster-recovery bundle

The mnemonic restores **transport identity only**. Complete recovery of an operational profile requires both:

1. the 24-word recovery phrase (or future threshold shares) for the exact Ed25519 identity; and
2. a separate backup of the profile's non-secret `config.yaml`, especially `trust.allowed_peers`, endpoint definitions/default route, discovery/bootstrap/Kademlia settings, desired channels, and policy/limit choices.

The phrase alone intentionally restores a **bare identity**: the same PeerId with no reconstructed trust allowlist, EndpointIds, bootstrap policy, application contacts, or message history. Runtime cache, endpoint leases, directory cache, dedup state, and messages are never part of the disaster-recovery bundle. Applications such as a human client back up their own contacts/history separately.

## Backup redundancy and future threshold recovery

Copying the same 24-word phrase to several locations improves availability but increases theft exposure.

A future threshold-backup format may use **SLIP-0039** with the same exact 32-byte Ed25519 secret seed as the master secret. That can add `T-of-N` recovery without changing the recovered PeerId. Such support is a separate implementation/UX decision and must not reinterpret existing 24-word records.

The architecture does not implement custom Shamir secret sharing.

## Golden fixture

Test-only secret; never use as a production key:

```text
ed25519_secret_hex =
0000000000000000000000000000000000000000000000000000000000000000

mnemonic =
abandon abandon abandon abandon abandon abandon abandon abandon
abandon abandon abandon abandon abandon abandon abandon abandon
abandon abandon abandon abandon abandon abandon abandon art

expected_peer_id =
12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN
```

The phrase is the standard 256-bit-zero BIP-39 entropy encoding. The expected PeerId fixture additionally verifies the chosen Ed25519/libp2p reconstruction path.

## Security invariants

- phrase == private-key-equivalent secret;
- phrase never crosses MCP, Channel, transport network, endpoint directory, discovery, Kademlia, or daemon IPC;
- remote messages can never request/export/restore/rotate identity;
- no recovery phrase is stored in normal config, peer cache, runtime state, logs, or diagnostics;
- profile creation generates the Ed25519 seed with a CSPRNG and never offers a user-selected-word/"brainwallet" creation mode; restore cannot prove how historical entropy was generated, so it validates only this format, checksum, and expected PeerId;
- recovery does not grant trust to peers or restore application/human identity bindings automatically.

## Required conformance tests

- golden zero-secret phrase decodes to 32 zero bytes and expected PeerId;
- random generated Ed25519 secret -> 24 words -> exact same secret -> exact same PeerId;
- one-word mutation failing checksum is rejected;
- checksum-valid phrase with wrong expected PeerId is rejected;
- 12/15/18/21-word BIP-39 phrases are rejected for this format;
- non-English wordlist is rejected;
- BIP-39 PBKDF2 seed output is never used as the Ed25519 secret;
- export/import unavailable through IPC and Claude tools;
- recovery phrase absent from logs/crash reports/config fixtures;
- established-key overwrite is fail-closed without explicit matching restore flow;
- rotation produces a distinct PeerId and distinct phrase;
- verify-only drill derives/compares the expected PeerId without key-file write, profile mutation, IPC, or network activity;
- a restored phrase without backed-up config yields only a bare identity, never reconstructed trust/endpoints.
