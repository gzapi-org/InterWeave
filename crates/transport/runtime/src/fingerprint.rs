// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `DirectContentFingerprintV1`, the content identity a dedup entry stores.
//!
//! From `contracts/ENDPOINTS.md`. A pure function over the message content
//! and nothing else: no endpoint, no message ID, and **no timestamp**.
//! Excluding `sent_at_ms` is what lets a retry of the same message match
//! its own earlier attempt — a fingerprint that covered the clock would
//! make every retry look like different content.
//!
//! The frozen vectors in `fixtures/direct-v2/` are recomputed against this
//! implementation by its own tests, so the vectors become executable here
//! rather than only in the Python verifier.

use sha2::{Digest, Sha256};

/// The domain prefix, ASCII followed by one `0x00`.
///
/// The terminator is load-bearing: without it a domain could be a prefix
/// of another, and two different constructions could hash the same bytes.
pub const DOMAIN: &[u8] = b"interweave/direct-content-fingerprint/v1\x00";

/// A 32-byte content fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentFingerprint([u8; 32]);

impl ContentFingerprint {
    /// The raw digest.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lower-case hex, the form the fixtures and prose use.
    #[must_use]
    pub fn to_hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut s = String::with_capacity(64);
        for b in self.0 {
            s.push(char::from(HEX[usize::from(b >> 4)]));
            s.push(char::from(HEX[usize::from(b & 0x0f)]));
        }
        s
    }
}

/// Why a fingerprint could not be computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FingerprintError {
    /// A media type was present but empty.
    ///
    /// Empty is INVALID rather than an alias for absence: the framing
    /// distinguishes the two, so collapsing them would give two different
    /// messages one content identity.
    EmptyMediaType,
    /// A present media type exceeded 128 bytes.
    MediaTypeTooLong {
        /// Bytes supplied.
        got: usize,
    },
    /// A present media type was not ASCII.
    MediaTypeNotAscii,
}

impl core::fmt::Display for FingerprintError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::EmptyMediaType => {
                write!(
                    f,
                    "an empty media type is invalid; absence is encoded separately"
                )
            }
            Self::MediaTypeTooLong { got } => {
                write!(f, "media type is {got} bytes; the limit is 128")
            }
            Self::MediaTypeNotAscii => write!(f, "media type is not ASCII"),
        }
    }
}

impl core::error::Error for FingerprintError {}

/// Compute `DirectContentFingerprintV1`.
///
/// `domain || media_present:u8 || [media_len:u16be || media] || payload_len:u32be || payload`
///
/// Absence uses `media_present = 0` and carries **no length field at all**,
/// which is what keeps an absent media type distinct from any present one
/// including a zero-length string — and why the empty string is rejected
/// rather than encoded.
///
/// # Errors
/// Returns [`FingerprintError`] for an empty, over-long, or non-ASCII
/// media type.
pub fn direct_content_fingerprint_v1(
    media_type: Option<&str>,
    payload: &[u8],
) -> Result<ContentFingerprint, FingerprintError> {
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN);
    match media_type {
        None => hasher.update([0u8]),
        Some(media) => {
            if media.is_empty() {
                return Err(FingerprintError::EmptyMediaType);
            }
            if !media.is_ascii() {
                return Err(FingerprintError::MediaTypeNotAscii);
            }
            let bytes = media.as_bytes();
            if bytes.len() > 128 {
                return Err(FingerprintError::MediaTypeTooLong { got: bytes.len() });
            }
            hasher.update([1u8]);
            // u16be for the media length, u32be for the payload: the
            // widths differ because their ceilings do, and both are
            // big-endian like every other length in this repository.
            let len = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
            hasher.update(len.to_be_bytes());
            hasher.update(bytes);
        }
    }
    let payload_len = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    hasher.update(payload_len.to_be_bytes());
    hasher.update(payload);
    Ok(ContentFingerprint(hasher.finalize().into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_adr_0047_golden_reproduces() {
        let fp = direct_content_fingerprint_v1(Some("text/plain"), b"hello").expect("valid");
        assert_eq!(
            fp.to_hex(),
            "d73342f033f00fca9c4ffcced6f9e6debaeb53e3743049ee9aaf227a55f9bf15"
        );
    }

    #[test]
    fn absence_and_presence_are_distinct_for_the_same_payload() {
        // The property the media_present byte exists for.
        let absent = direct_content_fingerprint_v1(None, b"hello").expect("valid");
        let present = direct_content_fingerprint_v1(Some("text/plain"), b"hello").expect("valid");
        assert_ne!(absent, present);
    }

    #[test]
    fn an_empty_media_type_is_invalid_rather_than_absent() {
        // If it were encoded as a zero-length present value, it would
        // collide with nothing — but accepting it would create a second
        // spelling of "no media type", and two spellings mean one message
        // with two content identities.
        assert_eq!(
            direct_content_fingerprint_v1(Some(""), b"hello"),
            Err(FingerprintError::EmptyMediaType)
        );
    }

    #[test]
    fn media_bounds_are_enforced() {
        assert!(direct_content_fingerprint_v1(Some(&"a".repeat(128)), b"").is_ok());
        assert_eq!(
            direct_content_fingerprint_v1(Some(&"a".repeat(129)), b""),
            Err(FingerprintError::MediaTypeTooLong { got: 129 })
        );
        assert_eq!(
            direct_content_fingerprint_v1(Some("text/\u{e9}"), b""),
            Err(FingerprintError::MediaTypeNotAscii)
        );
    }

    #[test]
    fn length_prefixes_stop_a_boundary_ambiguity() {
        // Without the payload length, ("ab", "") and ("a", "b") could be
        // made to hash identically by moving the boundary. They must not.
        let a = direct_content_fingerprint_v1(Some("ab"), b"").expect("valid");
        let b = direct_content_fingerprint_v1(Some("a"), b"b").expect("valid");
        assert_ne!(a, b);
    }

    #[test]
    fn an_empty_payload_is_legal_and_distinct() {
        let empty = direct_content_fingerprint_v1(None, b"").expect("valid");
        let one = direct_content_fingerprint_v1(None, b"\0").expect("valid");
        assert_ne!(empty, one);
    }
}
