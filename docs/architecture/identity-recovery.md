# Identity recovery architecture

## Objective

Recover a lost software profile key **without changing the profile PeerId** and without widening the daemon/Channel secret boundary.

## Layers

```text
24-word offline recovery record
        |
        | BIP-39 entropy decode only
        v
32-byte Ed25519 secret seed
        |
        | libp2p identity API / portable key representation
        v
Ed25519 public key -> PeerId
        |
        v
profile identity.key
```

There is no wallet/BIP-32 layer and no BIP-39 PBKDF2 seed step.

## Why encode the actual Ed25519 seed

Encoding the actual 32-byte secret rather than deriving a key from a separate mnemonic root has three useful properties:

1. an already-created Ed25519 profile can be backed up later without rotating identity;
2. restore is one-to-one and independent of a custom KDF;
3. a future SLIP-0039 threshold backup can split the same 32-byte secret and recover the same PeerId.

The tradeoff is intentional: v1 software identity algorithm is fixed to Ed25519.

## Recovery operations stay outside daemon IPC

```text
Claude bridge -----X---- recovery secret
Human data IPC ----X---- recovery secret
Admin IPC ---------X---- recovery secret
P2P network -------X---- recovery secret

transportctl identity backup/verify/restore
        |
        +-- local profile files, daemon stopped, exclusive identity lock
```

The phrase is key material, not an admin API value. This preserves the existing rule that private identity secrets never cross IPC.

## Backup UX

A first-party human client may guide the operator to the local recovery command, but should not proxy the phrase through its daemon IPC session.

The display must include:

- `P2P transport identity recovery phrase — not a cryptocurrency wallet seed`;
- format identifier;
- PeerId;
- 24 numbered words;
- instruction to verify the words and PeerId offline;
- warning that anyone with the words can impersonate the PeerId.

Clipboard/copy requires an explicit user gesture and is not the default backup path.

## Verify-only drill

Routine recovery exercises should use `transportctl identity verify`, not restore. The command decodes the phrase, reconstructs the Ed25519 public key/PeerId, compares it with the expected public PeerId, reports match/mismatch, then discards secret buffers. It performs no key-file write, no profile mutation, no daemon start, and no network activity.

## Restore UX

The preferred restore record includes both words and expected PeerId. Restore decodes the exact 32-byte key and verifies the resulting PeerId before writing any identity file.

If a full-machine loss leaves only the words, a new empty local profile may be reconstructed and the derived PeerId displayed for manual verification. This path is never allowed to overwrite an established profile automatically.

## Failure and compromise

A phrase typo normally fails BIP-39 checksum; because the checksum is only 8 bits for 24 words, expected-PeerId comparison is the stronger identity check.

A stolen phrase is equivalent to a stolen private key. Recovery itself cannot revoke that copy. Peers must update trust after intentional rotation/compromise.

## Complete disaster-recovery bundle

The 24 words recover the transport identity only. Complete profile recovery requires a **separate backup of `config.yaml`** containing trust allowlists, configured EndpointIds/default route, discovery/bootstrap/Kademlia configuration, desired channels, and policy/limit choices.

Phrase without config = same PeerId but a bare profile. Config without phrase = policy/topology without the private identity. Runtime peer cache, leases, directory cache, dedup state, undelivered messages, and human-client contacts/history are outside transport recovery and must not be reconstructed implicitly.

## Future threshold backup

SLIP-0039 is a plausible optional second format because it can split a 256-bit master secret into threshold mnemonic shares. If adopted, its master secret is the same exact 32-byte Ed25519 seed; it must not derive a new identity or reinterpret the existing 24-word record.
