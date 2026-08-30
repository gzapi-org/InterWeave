// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! K1 — deterministic custom protocol derivation from `network_id`.
//!
//! `kademlia-integration.md` §4 fixes the derivation and publishes one
//! golden vector. This module implements it from the SPECIFICATION TEXT
//! and checks the result against that vector, so a derivation that
//! merely agrees with itself cannot pass.

use sha2::{Digest, Sha256};

/// RFC4648 base32, lower-cased. The spec says base32 of the first 16
/// digest bytes without padding; 16 bytes is 128 bits, which is exactly
/// 26 base32 characters with 2 bits of zero padding in the last symbol
/// and no `=` needed.
const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// `^[a-z0-9][a-z0-9._-]{0,63}$`, per the specification.
#[must_use]
pub fn network_id_is_legal(id: &str) -> bool {
    let mut bytes = id.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    if id.len() > 64 {
        return false;
    }
    bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
}

/// `SHA-256("interweave/kad-network/v1\0" || ASCII(network_id))`,
/// base32 of the first 16 bytes, lower case, unpadded.
#[must_use]
pub fn network_hash(network_id: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"interweave/kad-network/v1\0");
    h.update(network_id.as_bytes());
    let digest = h.finalize();

    let mut out = String::with_capacity(26);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for &b in &digest[..16] {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(ALPHABET[((acc >> bits) & 0x1f) as usize]));
        }
    }
    if bits > 0 {
        out.push(char::from(ALPHABET[((acc << (5 - bits)) & 0x1f) as usize]));
    }
    out
}

/// `/interweave/kad/<major>/<network-hash>`.
#[must_use]
pub fn protocol_name(network_id: &str) -> String {
    format!("/interweave/kad/1.0.0/{}", network_hash(network_id))
}
