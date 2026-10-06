// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The envelope's bytes, without the cipher.
//!
//! ```text
//! magic "IWK1" (4) | version 0x01 (1) | policy (1) | iv (12) | ciphertext (32) | tag (16)
//! associated data = magic | version | policy | the profile's PeerId (UTF-8)
//! ```

use interweave_profile_identity::{ProfileIdentity, RecoveryPhrase};

pub const MAGIC: &[u8; 4] = b"IWK1";
pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 6;
pub const IV_LEN: usize = 12;
pub const SEED_LEN: usize = 32;
pub const TAG_LEN: usize = 16;
pub const SEALED_LEN: usize = SEED_LEN + TAG_LEN;
pub const LEN: usize = HEADER_LEN + IV_LEN + SEALED_LEN;

/// Why a stored envelope is refused before or after the cipher. The
/// numbering is the JNI surface's (`Core.java` names each), and the
/// cipher's own refusal, authentication, is the Keystore's
/// `AEADBadTagException`, so it has no code here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Refused {
    Length = 1,
    Magic = 2,
    Version = 3,
    Policy = 4,
    WrongIdentity = 6,
}

/// The associated data for `policy` and the profile `peer`, exactly as
/// the host half builds it.
#[must_use]
pub fn aad(policy: u8, peer: &str) -> Vec<u8> {
    let mut a = Vec::with_capacity(HEADER_LEN + peer.len());
    a.extend_from_slice(MAGIC);
    a.push(VERSION);
    a.push(policy);
    a.extend_from_slice(peer.as_bytes());
    a
}

/// Frame what the Keystore cipher returned: its IV and its output
/// (ciphertext with the tag appended). `None` when either length is not
/// the layout's, so a Keystore that returned another IV or tag size is
/// seen, never framed.
#[must_use]
pub fn frame(policy: u8, iv: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
    if iv.len() != IV_LEN || sealed.len() != SEALED_LEN || policy > 1 {
        return None;
    }
    let mut out = Vec::with_capacity(LEN);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.push(policy);
    out.extend_from_slice(iv);
    out.extend_from_slice(sealed);
    Some(out)
}

/// The header check that runs before any decryption: length, magic,
/// version, then policy, in the host half's order.
///
/// # Errors
/// The first of those that fails.
pub fn check(envelope: &[u8]) -> Result<(), Refused> {
    if envelope.len() != LEN {
        return Err(Refused::Length);
    }
    if &envelope[..4] != MAGIC {
        return Err(Refused::Magic);
    }
    if envelope[4] != VERSION {
        return Err(Refused::Version);
    }
    if envelope[5] > 1 {
        return Err(Refused::Policy);
    }
    Ok(())
}

/// The policy byte, IV and sealed bytes of an envelope `check` passed.
#[must_use]
pub fn split(envelope: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    check(envelope).ok()?;
    Some((
        envelope[5],
        &envelope[HEADER_LEN..HEADER_LEN + IV_LEN],
        &envelope[HEADER_LEN + IV_LEN..],
    ))
}

/// The PeerId a seed derives through the production derivation (the
/// entropy IS the seed, ADR-0033).
#[must_use]
pub fn peer_of(seed: &[u8]) -> Option<String> {
    let seed: &[u8; SEED_LEN] = seed.try_into().ok()?;
    let phrase = RecoveryPhrase::from_entropy(seed).ok()?;
    let identity = ProfileIdentity::from_phrase(&phrase).ok()?;
    Some(identity.transport_identity().ok()?.as_str().to_owned())
}

/// After the Keystore authenticated and decrypted: the seed must derive
/// the profile's own PeerId, or it is refused.
///
/// # Errors
/// `WrongIdentity` when it derives another, or none.
pub fn verify(seed: &[u8], peer: &str) -> Result<(), Refused> {
    if peer_of(seed).as_deref() == Some(peer) {
        Ok(())
    } else {
        Err(Refused::WrongIdentity)
    }
}
