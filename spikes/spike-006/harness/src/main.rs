// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! SPIKE-006 — identity-recovery portability. Evidence only.
//!
//! Run with `cargo run` from this directory. Every assertion prints its
//! answer; the exit code is the verdict.

use libp2p_identity::{Keypair, PeerId, ed25519};

/// The contract's golden: an all-zero 32-byte entropy IS the Ed25519
/// secret seed, and must produce this PeerId.
const GOLDEN_ENTROPY: [u8; 32] = [0u8; 32];
const GOLDEN_PEER_ID: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
const GOLDEN_PUBLIC_HEX: &str = "3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn main() -> std::process::ExitCode {
    let mut failures = 0u32;
    let mut check = |name: &str, ok: bool, detail: String| {
        println!("{} {name}\n    {detail}", if ok { "PASS" } else { "FAIL" });
        if !ok {
            failures += 1;
        }
    };

    // ---------------------------------------------------------------
    // Q1. Which API takes exactly 32 bytes?
    // ---------------------------------------------------------------
    let secret = ed25519::SecretKey::try_from_bytes(GOLDEN_ENTROPY)
        .expect("ed25519::SecretKey::try_from_bytes accepts 32 bytes");
    let kp = ed25519::Keypair::from(secret);

    // EXACTLY 32: the neighbours must be refused, or "accepts 32 bytes"
    // is a sentence about a call that happened rather than a bound.
    let short = ed25519::SecretKey::try_from_bytes(&mut [0u8; 31][..]).is_err();
    let long = ed25519::SecretKey::try_from_bytes(&mut [0u8; 33][..]).is_err();
    check(
        "Q1 try_from_bytes accepts 32 bytes and refuses 31 and 33",
        short && long,
        format!("31 bytes refused: {short}, 33 bytes refused: {long}"),
    );

    // ---------------------------------------------------------------
    // Q2. Does the golden seed reproduce the frozen public key and PeerId?
    // ---------------------------------------------------------------
    let public = kp.public();
    let public_hex = hex(&public.to_bytes());
    check(
        "Q2a golden seed reproduces the frozen Ed25519 public key",
        public_hex == GOLDEN_PUBLIC_HEX,
        format!("got {public_hex}\n    want {GOLDEN_PUBLIC_HEX}"),
    );

    let peer_id = PeerId::from_public_key(&Keypair::from(kp.clone()).public());
    check(
        "Q2b golden seed reproduces the frozen PeerId",
        peer_id.to_base58() == GOLDEN_PEER_ID,
        format!("got {}\n    want {GOLDEN_PEER_ID}", peer_id.to_base58()),
    );

    // ---------------------------------------------------------------
    // Q3. Is the exported secret the SAME 32 bytes, or something larger?
    //
    // The dangerous answer is a 64-byte `secret || public`: mnemonic-
    // encoding that would produce 48 words, or silently truncate to a
    // half that is not the seed.
    // ---------------------------------------------------------------
    // `SecretKey::to_bytes` is pub(crate) in 0.3.0 — the obvious accessor
    // is NOT public. `AsRef<[u8]>` is, and is the only public path to the
    // raw seed. Finding that is half the point of this spike: a production
    // adapter written from the docs would have reached for to_bytes().
    // `secret()` returns an owned SecretKey, so it must be bound before
    // borrowing through AsRef.
    let secret_owned = kp.secret();
    let exported: &[u8] = secret_owned.as_ref();
    check(
        "Q3a the public accessor is AsRef<[u8]>, and it yields 32 bytes",
        exported.len() == 32,
        format!(
            "SecretKey::to_bytes() is pub(crate); <SecretKey as AsRef<[u8]>>::as_ref() \
             returned {} bytes",
            exported.len()
        ),
    );
    check(
        "Q3b the exported secret is byte-identical to the seed supplied",
        exported == &GOLDEN_ENTROPY[..],
        format!("got {}\n    want {}", hex(exported), hex(&GOLDEN_ENTROPY)),
    );

    // ---------------------------------------------------------------
    // Q3c. Is `Keypair::to_bytes()` seed||public, or EXPANDED||public?
    //
    // SPIKES.md forbids mnemonic-encoding "any 64-byte expanded
    // `secret || public` representation". The 64-byte form is only safe
    // to take a seed from if its first half IS the seed rather than the
    // SHA-512 expansion of it. Nothing in the documentation settles this;
    // the words "secret scalar" suggest the expansion, which would be the
    // dangerous answer.
    // ---------------------------------------------------------------
    let sixty_four = kp.to_bytes();
    let first_half_is_seed = sixty_four[..32] == GOLDEN_ENTROPY[..];
    check(
        "Q3c the 64-byte keypair form is seed||public, not expanded||public",
        first_half_is_seed,
        format!(
            "first 32 bytes = {}\n    seed          = {}\n    second 32 = public key = {}",
            hex(&sixty_four[..32]),
            hex(&GOLDEN_ENTROPY),
            hex(&sixty_four[32..])
        ),
    );
    check(
        "Q3d the 64-byte form's second half is the public key",
        sixty_four[32..] == public.to_bytes()[..],
        format!("{}", hex(&sixty_four[32..])),
    );

    // ---------------------------------------------------------------
    // Q4. Round trip through the portable representation.
    // ---------------------------------------------------------------
    // NOTE the signature: `try_from_bytes(impl AsMut<[u8]>)` ZEROES the
    // caller's buffer on success. A production adapter that passes its
    // only copy loses it — which is good hygiene and a sharp edge worth
    // recording, since nothing in the call site suggests the argument is
    // consumed destructively.
    let mut owned = [0u8; 32];
    owned.copy_from_slice(exported);
    let reimported =
        ed25519::SecretKey::try_from_bytes(&mut owned).expect("re-import of the exported bytes");
    let zeroed_on_import = owned == [0u8; 32];
    let rekp = ed25519::Keypair::from(reimported);
    let repid = PeerId::from_public_key(&Keypair::from(rekp).public());
    check(
        "Q4a export then re-import reproduces the same PeerId",
        repid == peer_id,
        format!("{repid}"),
    );
    check(
        "Q4b try_from_bytes zeroes the caller's buffer on success",
        zeroed_on_import,
        format!("caller buffer after import = {}", hex(&owned)),
    );

    // ---------------------------------------------------------------
    // Q5. The protobuf encoding is NOT the mnemonic input.
    //
    // `Keypair::to_protobuf_encoding` exists and is the tempting thing to
    // persist. It must never be what gets mnemonic-encoded: it is longer
    // than 32 bytes and carries a key-type tag, so the 24-word format
    // cannot represent it.
    // ---------------------------------------------------------------
    let full = Keypair::from(ed25519::Keypair::from(
        ed25519::SecretKey::try_from_bytes(GOLDEN_ENTROPY).expect("seed"),
    ));
    match full.to_protobuf_encoding() {
        Ok(pb) => check(
            "Q5 the protobuf private-key encoding is larger than the seed",
            pb.len() != 32,
            format!(
                "to_protobuf_encoding() = {} bytes ({}). NOT mnemonic input.",
                pb.len(),
                hex(&pb)
            ),
        ),
        Err(e) => check("Q5 protobuf encoding available", false, format!("{e}")),
    }

    // ---------------------------------------------------------------
    // Q6. Random identities round-trip too — the golden alone could pass
    // by coincidence for an all-zero input.
    // ---------------------------------------------------------------
    let mut random_ok = true;
    let mut detail = String::new();
    for i in 0..64 {
        let generated = ed25519::Keypair::generate();
        let mut seed = [0u8; 32];
        seed.copy_from_slice(generated.secret().as_ref());
        let seed_len = seed.len();
        let restored = ed25519::Keypair::from(
            ed25519::SecretKey::try_from_bytes(&mut seed).expect("re-import"),
        );
        let a = PeerId::from_public_key(&Keypair::from(generated).public());
        let b = PeerId::from_public_key(&Keypair::from(restored).public());
        if a != b || seed_len != 32 {
            random_ok = false;
            detail = format!("iteration {i}: {a} != {b}");
            break;
        }
    }
    if random_ok {
        detail = "64 CSPRNG identities: 32-byte seed out, same PeerId back".to_owned();
    }
    check("Q6 random identities round-trip through the 32-byte seed", random_ok, detail);

    // ---------------------------------------------------------------
    // Q7. A BIP-39 PBKDF2 seed must NEVER be accepted as the transport
    // secret. That output is 64 bytes; the API taking [u8; 32] refuses it
    // structurally, which is the answer the contract wants.
    // ---------------------------------------------------------------
    // The real question is not whether a 64-byte array fits a [u8; 32]
    // parameter — that is a fact about Rust, not about this contract.
    // It is whether feeding the SAME WORDS through the wallet convention
    // produces a different identity. If it did not, the two paths would be
    // interchangeable and the contract's warning would be decorative.
    const GOLDEN_MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon \
abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon \
abandon abandon abandon abandon abandon art";
    let mut wallet_seed = [0u8; 64];
    pbkdf2::pbkdf2::<hmac::Hmac<sha2::Sha512>>(
        GOLDEN_MNEMONIC.as_bytes(),
        b"mnemonic",
        2048,
        &mut wallet_seed,
    )
    .expect("pbkdf2");

    let mut wallet_first32 = [0u8; 32];
    wallet_first32.copy_from_slice(&wallet_seed[..32]);
    let wallet_kp = ed25519::Keypair::from(
        ed25519::SecretKey::try_from_bytes(&mut wallet_first32).expect("32 bytes"),
    );
    let wallet_peer = PeerId::from_public_key(&Keypair::from(wallet_kp).public());

    check(
        "Q7 the BIP-39 wallet derivation yields a DIFFERENT identity from the same words",
        wallet_peer.to_base58() != GOLDEN_PEER_ID,
        format!(
            "wallet PBKDF2 path -> {}\n    contract path      -> {GOLDEN_PEER_ID}\n    \
             feeding these words to a wallet yields unrelated material, as the fixture says",
            wallet_peer.to_base58()
        ),
    );

    println!("\n--- SPIKE-006 verdict ---");
    if failures == 0 {
        println!("PASS: the pinned libp2p identity API exposes and accepts the exact");
        println!("32-byte Ed25519 seed, and reproduces the frozen PeerId.");
        std::process::ExitCode::SUCCESS
    } else {
        println!("FAIL: {failures} check(s) failed. Recovery stays disabled (SPIKES.md).");
        std::process::ExitCode::FAILURE
    }
}
