// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! `DirectContentFingerprintV1`, from `ENDPOINTS.md`:
//!
//! ```text
//! SHA-256( "interweave/direct-content-fingerprint/v1" || 0x00
//!          || media_present:u8 || [media_len:u16be || media_ascii]
//!          || payload_len:u32be || payload )
//! ```

use sha2::{Digest, Sha256};

use crate::{DecodeError, MAX_PAYLOAD_BYTES, is_media_type};

/// The domain prefix, its terminating zero included.
pub const DOMAIN: &[u8] = b"interweave/direct-content-fingerprint/v1\0";

/// The canonical byte string the hash is taken over.
///
/// # Errors
/// An empty or over-long media type — invalid, never an alias for absence —
/// or a payload above the ceiling.
pub fn canonical_bytes(media_type: Option<&str>, payload: &[u8]) -> Result<Vec<u8>, DecodeError> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(DecodeError("payload above the ceiling".to_owned()));
    }
    let mut out = Vec::with_capacity(DOMAIN.len() + 1 + 2 + 128 + 4 + payload.len());
    out.extend_from_slice(DOMAIN);
    match media_type {
        None => out.push(0),
        Some(m) => {
            if !is_media_type(m) {
                return Err(DecodeError(
                    "media_type not 1..128 printable ASCII bytes".to_owned(),
                ));
            }
            out.push(1);
            out.extend_from_slice(&u16::try_from(m.len()).unwrap_or(u16::MAX).to_be_bytes());
            out.extend_from_slice(m.as_bytes());
        }
    }
    out.extend_from_slice(
        &u32::try_from(payload.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    out.extend_from_slice(payload);
    Ok(out)
}

/// The fingerprint.
///
/// # Errors
/// As [`canonical_bytes`].
pub fn fingerprint(media_type: Option<&str>, payload: &[u8]) -> Result<[u8; 32], DecodeError> {
    Ok(Sha256::digest(canonical_bytes(media_type, payload)?).into())
}
