// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Unpadded base64url, the encoding application bytes use at JSON boundaries.
//!
//! Implemented here rather than taken as a dependency: this is a neutral
//! contract crate whose dependency list is itself part of the contract
//! (ADR-0021), the algorithm is fully specified by RFC 4648 §5, and it is
//! about forty lines that can be exhaustively tested.
//!
//! Strictness is the point. Decoding **rejects** padding, the standard
//! (`+/`) alphabet, and any length that unpadded base64 can never produce.
//! A lenient decoder would accept two spellings of the same payload, and
//! the content fingerprint hashes the decoded bytes, so two spellings
//! would mean one message with two dedup identities.

/// Why a base64url string could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Base64Error {
    /// A character outside the unpadded base64url alphabet.
    ///
    /// `=` lands here: padding is not part of the unpadded form, and
    /// silently tolerating it would accept a non-canonical spelling.
    IllegalCharacter {
        /// Zero-based index of the offending character.
        index: usize,
        /// The offending byte.
        byte: u8,
    },
    /// A length unpadded base64 cannot produce.
    ///
    /// `len % 4 == 1` is unreachable for any input, so such a string is
    /// malformed rather than merely truncated.
    InvalidLength {
        /// The length supplied.
        len: usize,
    },
    /// The final character carried bits that decode to nothing.
    ///
    /// Two spellings would otherwise decode to identical bytes, which is
    /// exactly the non-canonical encoding this module refuses.
    NonZeroTrailingBits,
}

impl core::fmt::Display for Base64Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::IllegalCharacter { index, byte } => {
                write!(
                    f,
                    "byte {byte:#04x} at index {index} is not unpadded base64url"
                )
            }
            Self::InvalidLength { len } => {
                write!(f, "length {len} is impossible for unpadded base64")
            }
            Self::NonZeroTrailingBits => {
                write!(f, "the final character carries bits that decode to nothing")
            }
        }
    }
}

impl core::error::Error for Base64Error {}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

const fn decode_char(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'-' => Some(62),
        b'_' => Some(63),
        _ => None,
    }
}

/// Encode bytes as unpadded base64url.
#[must_use]
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, u32::from);
        let b2 = chunk.get(2).copied().map_or(0, u32::from);
        let n = (b0 << 16) | (b1 << 8) | b2;
        let idx = |shift: u32| usize::try_from((n >> shift) & 0x3f).unwrap_or(0);
        out.push(char::from(ALPHABET[idx(18)]));
        out.push(char::from(ALPHABET[idx(12)]));
        if chunk.len() > 1 {
            out.push(char::from(ALPHABET[idx(6)]));
        }
        if chunk.len() > 2 {
            out.push(char::from(ALPHABET[idx(0)]));
        }
    }
    out
}

/// Decode unpadded base64url, rejecting every non-canonical spelling.
///
/// # Errors
/// Returns [`Base64Error`] for a character outside the alphabet (padding
/// included), an impossible length, or a final character whose unused low
/// bits are not zero.
pub fn decode(text: &str) -> Result<Vec<u8>, Base64Error> {
    let src = text.as_bytes();
    if src.len() % 4 == 1 {
        return Err(Base64Error::InvalidLength { len: src.len() });
    }
    let mut out = Vec::with_capacity(src.len() / 4 * 3);
    for chunk in src.chunks(4) {
        let mut n: u32 = 0;
        for (i, &b) in chunk.iter().enumerate() {
            let v = decode_char(b).ok_or(Base64Error::IllegalCharacter {
                index: (chunk.as_ptr() as usize - src.as_ptr() as usize) + i,
                byte: b,
            })?;
            n |= u32::from(v) << (18 - 6 * i);
        }
        match chunk.len() {
            4 => {
                out.push(((n >> 16) & 0xff) as u8);
                out.push(((n >> 8) & 0xff) as u8);
                out.push((n & 0xff) as u8);
            }
            3 => {
                // Six unused low bits must be zero, or two different
                // strings would decode to the same two bytes.
                if n & 0xff != 0 {
                    return Err(Base64Error::NonZeroTrailingBits);
                }
                out.push(((n >> 16) & 0xff) as u8);
                out.push(((n >> 8) & 0xff) as u8);
            }
            2 => {
                if n & 0xffff != 0 {
                    return Err(Base64Error::NonZeroTrailingBits);
                }
                out.push(((n >> 16) & 0xff) as u8);
            }
            _ => unreachable!("chunks(4) yields 1..=4 and length %4==1 was rejected"),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_tail_length() {
        for len in 0..=64usize {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 7 % 251) as u8).collect();
            let text = encode(&bytes);
            assert_eq!(decode(&text).expect("decodes"), bytes, "len {len}");
        }
    }

    #[test]
    fn encodes_the_rfc_alphabet_including_the_url_safe_pair() {
        // 0xfb 0xff exercises the two characters that differ from standard
        // base64: `-` and `_` where `+` and `/` would appear.
        assert_eq!(encode(&[0xfb, 0xff]), "-_8");
        assert_eq!(decode("-_8").expect("decodes"), vec![0xfb, 0xff]);
        assert_eq!(encode(b""), "");
        assert_eq!(decode("").expect("decodes"), Vec::<u8>::new());
    }

    #[test]
    fn rejects_padding_and_the_standard_alphabet() {
        // Padding is not part of the unpadded form; accepting it would
        // admit a second spelling of the same bytes.
        assert!(matches!(
            decode("AA=="),
            Err(Base64Error::IllegalCharacter { .. })
        ));
        assert!(matches!(
            decode("+_8"),
            Err(Base64Error::IllegalCharacter { index: 0, .. })
        ));
        assert!(matches!(
            decode("-/8"),
            Err(Base64Error::IllegalCharacter { index: 1, .. })
        ));
    }

    #[test]
    fn rejects_impossible_lengths() {
        assert_eq!(decode("A"), Err(Base64Error::InvalidLength { len: 1 }));
        assert_eq!(decode("AAAAA"), Err(Base64Error::InvalidLength { len: 5 }));
    }

    #[test]
    fn rejects_non_canonical_trailing_bits() {
        // "AB" and "AA" would both decode to [0x00] under a lenient
        // decoder; only the canonical spelling is accepted.
        assert_eq!(decode("AA").expect("decodes"), vec![0x00]);
        assert_eq!(decode("AB"), Err(Base64Error::NonZeroTrailingBits));
        assert_eq!(decode("AAA").expect("decodes"), vec![0x00, 0x00]);
        assert_eq!(decode("AAB"), Err(Base64Error::NonZeroTrailingBits));
    }

    #[test]
    fn the_encoded_length_matches_the_schema_bound() {
        // 49,152 payload bytes must encode to exactly 65,536 characters —
        // the maxLength the IPC schemas state.
        assert_eq!(encode(&vec![0u8; 49_152]).len(), 65_536);
    }
}
