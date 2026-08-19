# SPIKE-006 — identity-recovery portability

**Status: PASS**, with three findings that change how the production adapter must be written.

Ed25519 recovery portability across the selected rust-libp2p identity API. Authoritative objective, evidence requirements, and decision gate live in [`architecture/roadmap/SPIKES.md`](../../architecture/roadmap/SPIKES.md); this file records what was actually observed.

Do not treat the experiment in [`harness/`](./harness) as production implementation. It is deliberately outside the workspace — an empty `[workspace]` table in its manifest — so it cannot be built by `cargo xtask ci`, cannot enter `Cargo.lock`, and cannot become a production dependency by a stray `path =`.

## What was pinned

```text
libp2p-identity 0.3.0   features: ed25519, peerid, rand
```

## The question

Does this API expose and accept the **exact 32-byte Ed25519 secret seed** that `interweave-ed25519-bip39-entropy-v1` assumes — not a 64-byte expanded `secret || public`, and not an opaque protobuf blob?

It matters because the recovery format encodes 32 bytes of entropy as 24 BIP-39 words. If the library only round-tripped a larger or opaque representation, either the words would mean something other than the key, or a restore would reconstruct a different PeerId. The second is the worst failure available, because it looks exactly like success.

## Answer: yes, but not through the obvious accessor

| | |
|---|---|
| import | `ed25519::SecretKey::try_from_bytes(impl AsMut<[u8]>)` — takes exactly 32 bytes, refuses 31 and 33 |
| export | `<SecretKey as AsRef<[u8]>>::as_ref()` — 32 bytes, byte-identical to what was imported |

### Finding 1 — `SecretKey::to_bytes()` is `pub(crate)`

The obvious accessor, the one an implementer reaches for and the one that appears in older examples, is **not public** in 0.3.0. `AsRef<[u8]>` is the only public path to the raw seed.

An adapter written from memory would reach for `to_bytes()`, fail to compile, and the likely next move is `Keypair::to_bytes()` — which returns 64 bytes and is a different thing. That is the wrong turn this spike exists to prevent.

### Finding 2 — the 64-byte form is `seed || public`, not `expanded || public`

`Keypair::to_bytes()` returns 64 bytes documented as "the secret scalar and the compressed public point". "Secret scalar" reads like the SHA-512 expansion, which would make its first half **not** the seed. Measured, it is not the expansion:

```text
first 32  = 0000…0000   ← identical to the supplied seed
second 32 = 3b6a27bc…   ← identical to the public key
```

So taking the first half is safe. It is still not what the production adapter should do, because Finding 1 gives a direct accessor and a 64-byte intermediate is one refactor away from being mnemonic-encoded whole — which `SPIKES.md` forbids.

### Finding 3 — `try_from_bytes` zeroes the caller's buffer

The parameter is `impl AsMut<[u8]>` and the input is zeroed on success. Good hygiene, and a sharp edge: an adapter that passes its only copy loses it, and nothing at the call site suggests the argument is consumed destructively.

## Evidence

All eleven checks pass. Reproduce with `cargo run` in [`harness/`](./harness).

```text
Q1  try_from_bytes accepts 32 bytes and refuses 31 and 33
Q2a golden seed reproduces the frozen Ed25519 public key
Q2b golden seed reproduces the frozen PeerId
Q3a the public accessor is AsRef<[u8]>, and it yields 32 bytes
Q3b the exported secret is byte-identical to the seed supplied
Q3c the 64-byte keypair form is seed||public, not expanded||public
Q3d the 64-byte form's second half is the public key
Q4a export then re-import reproduces the same PeerId
Q4b try_from_bytes zeroes the caller's buffer on success
Q5  the protobuf private-key encoding is larger than the seed
Q6  random identities round-trip through the 32-byte seed
Q7  the BIP-39 wallet derivation yields a DIFFERENT identity from the same words
```

### The golden

The all-zero entropy from `fixtures/identity/ed25519-bip39-entropy-v1.json` produces, through libp2p:

```text
public key = 3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29
PeerId     = 12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN
```

Both match the frozen fixture exactly. This is the whole reconstruction path — entropy to seed to public key to PeerId — agreeing between the contract's Python verifier and rust-libp2p.

### The negative half

`Q7` is the one that would have been easy to fake. Asserting that a 64-byte array cannot be passed to a `[u8; 32]` parameter is a fact about Rust, not about this contract, and the first version of this harness asserted exactly that. The meaningful question is whether the **same 24 words** put through the wallet convention produce a different identity:

```text
BIP-39 wallet PBKDF2 path -> 12D3KooWBq33BJkcsZxNvhZwBwSBn1EHg5jMTh873Eg4eVLJqNLp
contract path             -> 12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN
```

Unrelated identities, as the fixture says. The two conventions are distinguishable, so a restore cannot silently take the wrong one and appear to work.

`Q5` records the shape of the private-key protobuf for completeness: 68 bytes, `0801 1240` followed by `seed || public`. It carries the seed but is not 32 bytes, so it is not mnemonic input.

## Decision unlocked

Production `identity backup/restore` implementation against the frozen recovery contract is **authorized**, subject to the three findings above:

1. export through `AsRef<[u8]>`, never `to_bytes()` — it is not public;
2. never mnemonic-encode the 64-byte or protobuf forms, even though the 64-byte first half is the seed;
3. treat `try_from_bytes` as consuming its input.

## Not covered here

Process-restart persistence and the read-only `verify` path are properties of the **production** adapter, not of the library boundary, so they are proved by permanent tests under `crates/` rather than by this harness. `SPIKES.md` lists them as spike evidence; they are discharged in the implementation commit that follows, which is the right place for them — a spike cannot prove a behaviour of code that does not exist yet.
