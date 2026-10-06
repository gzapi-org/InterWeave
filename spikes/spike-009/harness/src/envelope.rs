// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! A PROPOSED v1 wrapped-identity envelope, for the spike to exercise --
//! not a contract. ADR-0042 says "a versioned authenticated ciphertext
//! wrapped by an AES-256-GCM key generated in AndroidKeyStore" and no
//! document fixes its bytes yet; this is the shape the spike measures,
//! chosen so the device can produce exactly these bytes:
//!
//! ```text
//! magic "IWK1" (4) | version 0x01 (1) | policy (1) | iv (12) | ciphertext (32) | tag (16)
//! associated data = magic | version | policy | expected PeerId (UTF-8)
//! ```
//!
//! - The IV is STORED: an AndroidKeyStore AES-GCM key generates its own
//!   IV on encryption (randomized encryption is required by default), so
//!   the caller cannot choose it and must keep the one it is given.
//! - The header and the expected PeerId are associated data (Android's
//!   `Cipher.updateAAD`). An unknown version or policy byte is refused by
//!   the header check before decryption (`Refused::Version`,
//!   `Refused::Policy`); a VALID policy swapped for the other, or a
//!   ciphertext moved to another profile, fails authentication.
//! - The PeerId is NOT in the envelope: it is the profile's, kept beside
//!   it, and unwrap also re-derives it from the seed and compares, so a
//!   seed that authenticates but derives another identity is refused.

use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};

pub const MAGIC: &[u8; 4] = b"IWK1";
pub const VERSION: u8 = 1;
pub const IV_LEN: usize = 12;
pub const SEED_LEN: usize = 32;
pub const TAG_LEN: usize = 16;
pub const LEN: usize = 4 + 1 + 1 + IV_LEN + SEED_LEN + TAG_LEN;

/// How the wrapping key may be used (ADR-0042's two explicit modes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    BackgroundCompatible = 0,
    UserPresence = 1,
}

/// Why an envelope did not give back a seed. Every one is final: there
/// is no fallback and no partial result (ADR-0042, fail-closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    Length,
    Magic,
    Version,
    Policy,
    Authentication,
    WrongIdentity,
}

fn aad(version: u8, policy: u8, peer: &str) -> Vec<u8> {
    let mut a = Vec::with_capacity(6 + peer.len());
    a.extend_from_slice(MAGIC);
    a.push(version);
    a.push(policy);
    a.extend_from_slice(peer.as_bytes());
    a
}

/// Wrap `seed` for the profile whose PeerId is `peer`. The IV is fresh
/// per wrap, as the Keystore makes it.
pub fn wrap(key: &[u8; 32], policy: Policy, peer: &str, seed: &[u8; SEED_LEN]) -> Vec<u8> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let iv = Aes256Gcm::generate_nonce(&mut OsRng);
    let sealed = cipher
        .encrypt(
            &iv,
            Payload {
                msg: seed,
                aad: &aad(VERSION, policy as u8, peer),
            },
        )
        .expect("AES-GCM encryption of 32 bytes cannot fail");
    let mut out = Vec::with_capacity(LEN);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.push(policy as u8);
    out.extend_from_slice(&iv);
    out.extend_from_slice(&sealed);
    out
}

/// Unwrap for the profile whose PeerId is `peer`; `derive` maps a seed
/// to its PeerId (the production derivation), and a seed deriving any
/// other is refused.
pub fn unwrap(
    key: &[u8; 32],
    peer: &str,
    envelope: &[u8],
    derive: impl Fn(&[u8; SEED_LEN]) -> Option<String>,
) -> Result<[u8; SEED_LEN], Refused> {
    if envelope.len() != LEN {
        return Err(Refused::Length);
    }
    if &envelope[..4] != MAGIC {
        return Err(Refused::Magic);
    }
    let (version, policy) = (envelope[4], envelope[5]);
    if version != VERSION {
        return Err(Refused::Version);
    }
    if policy > 1 {
        return Err(Refused::Policy);
    }
    let iv = Nonce::from_slice(&envelope[6..6 + IV_LEN]);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let plain = cipher
        .decrypt(
            iv,
            Payload {
                msg: &envelope[6 + IV_LEN..],
                aad: &aad(version, policy, peer),
            },
        )
        .map_err(|_| Refused::Authentication)?;
    let seed: [u8; SEED_LEN] = plain.try_into().map_err(|_| Refused::Length)?;
    if derive(&seed).as_deref() != Some(peer) {
        return Err(Refused::WrongIdentity);
    }
    Ok(seed)
}
