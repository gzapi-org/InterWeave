// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! Lower-case hexadecimal, which is how every fixture states its bytes.
//!
//! Fixtures carry inputs as hex on purpose: no encoding of the JSON file can
//! change what gets hashed, and `payload_utf8` beside it is a reader
//! convenience that is never the input (`fixtures/README.md`).
//!
//! Strict on the way in. Upper-case, odd length, and `0x` prefixes are errors
//! rather than accepted variants, because a fixture that hashes the same bytes
//! whether or not its notation is canonical stops pinning the notation — and
//! several contracts here (`MessageId`, `app_message_id`) make the canonical
//! form part of the rule.

/// Why a hex string could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HexError {
    /// The string has an odd number of characters, so a byte is incomplete.
    OddLength(usize),
    /// A character outside `0-9a-f` at this zero-based index.
    NotLowerHex {
        /// Index of the offending character.
        index: usize,
        /// The character found there.
        found: char,
    },
}

impl std::fmt::Display for HexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OddLength(len) => {
                write!(f, "hex string has odd length {len}")
            }
            Self::NotLowerHex { index, found } => {
                write!(
                    f,
                    "character {found:?} at index {index} is not lower-case hexadecimal"
                )
            }
        }
    }
}

impl std::error::Error for HexError {}

/// Decode canonical lower-case hex into bytes.
///
/// # Errors
///
/// [`HexError`] when the string is not canonical lower-case hex of even
/// length.
pub fn decode(text: &str) -> Result<Vec<u8>, HexError> {
    if !text.len().is_multiple_of(2) {
        return Err(HexError::OddLength(text.len()));
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = nibble(bytes[i], i)?;
        let lo = nibble(bytes[i + 1], i + 1)?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Ok(out)
}

fn nibble(byte: u8, index: usize) -> Result<u8, HexError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(HexError::NotLowerHex {
            index,
            found: char::from(byte),
        }),
    }
}

/// Encode bytes as canonical lower-case hex.
#[must_use]
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(digit(byte >> 4)));
        out.push(char::from(digit(byte & 0x0f)));
    }
    out
}

fn digit(nibble: u8) -> u8 {
    match nibble {
        0..=9 => b'0' + nibble,
        _ => b'a' + nibble - 10,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let bytes = vec![0x00, 0x0f, 0x68, 0xff];
        assert_eq!(encode(&bytes), "000f68ff");
        assert_eq!(decode("000f68ff"), Ok(bytes));
    }

    #[test]
    fn empty_is_empty_not_an_error() {
        assert_eq!(decode(""), Ok(Vec::new()));
        assert_eq!(encode(&[]), "");
    }

    #[test]
    fn upper_case_is_rejected() {
        // Not a style objection: the MessageId and app_message_id grammars
        // both specify lower case, so accepting `FF` here would let a fixture
        // exercise a form the contract calls invalid.
        assert_eq!(
            decode("FF"),
            Err(HexError::NotLowerHex {
                index: 0,
                found: 'F'
            })
        );
    }

    #[test]
    fn odd_length_is_rejected() {
        assert_eq!(decode("abc"), Err(HexError::OddLength(3)));
    }

    #[test]
    fn prefix_is_not_stripped() {
        assert_eq!(
            decode("0xff"),
            Err(HexError::NotLowerHex {
                index: 1,
                found: 'x'
            })
        );
    }
}
