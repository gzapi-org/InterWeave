// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Opaque application payloads and their advisory media type.

use core::fmt;

use serde::{Deserialize, Serialize};

/// The hard ceiling on application payload bytes (ADR-0026).
///
/// A profile may configure a lower effective limit, never a higher one.
pub const MAX_PAYLOAD_BYTES: usize = 49_152;

/// Maximum length of a present media type, in bytes.
pub const MAX_MEDIA_TYPE_BYTES: usize = 128;

/// Why a payload or media type was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadError {
    /// The payload exceeded the effective limit.
    TooLarge {
        /// Bytes supplied.
        got: usize,
        /// Effective limit in force.
        limit: usize,
    },
    /// A media type was present but empty. Absence is expressed by `None`.
    EmptyMediaType,
    /// A media type exceeded [`MAX_MEDIA_TYPE_BYTES`].
    MediaTypeTooLong {
        /// Bytes supplied.
        got: usize,
    },
    /// A media type contained a byte outside printable ASCII.
    MediaTypeNotAscii {
        /// Zero-based index of the offending byte.
        index: usize,
    },
}

impl fmt::Display for PayloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { got, limit } => {
                write!(f, "payload is {got} bytes; the effective limit is {limit}")
            }
            Self::EmptyMediaType => {
                write!(f, "an empty media type is invalid; use absence instead")
            }
            Self::MediaTypeTooLong { got } => {
                write!(f, "media type is {got} bytes; the limit is {MAX_MEDIA_TYPE_BYTES}")
            }
            Self::MediaTypeNotAscii { index } => {
                write!(f, "media type byte at index {index} is not printable ASCII")
            }
        }
    }
}

impl core::error::Error for PayloadError {}

/// A validated advisory media type: 1..=128 printable ASCII bytes.
///
/// Advisory means the transport neither interprets nor enforces it. It is
/// still validated, because it participates in the content fingerprint and
/// in wire framing where an over-long value would not fit its length field.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct MediaType(String);

impl MediaType {
    /// Parse a media type.
    ///
    /// # Errors
    /// Returns [`PayloadError`] when empty, longer than
    /// [`MAX_MEDIA_TYPE_BYTES`], or containing a non-printable-ASCII byte.
    pub fn parse(value: impl Into<String>) -> Result<Self, PayloadError> {
        let value = value.into();
        if value.is_empty() {
            return Err(PayloadError::EmptyMediaType);
        }
        if value.len() > MAX_MEDIA_TYPE_BYTES {
            return Err(PayloadError::MediaTypeTooLong { got: value.len() });
        }
        for (index, &b) in value.as_bytes().iter().enumerate() {
            if !(0x20..=0x7e).contains(&b) {
                return Err(PayloadError::MediaTypeNotAscii { index });
            }
        }
        Ok(Self(value))
    }

    /// The media type as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for MediaType {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(s).map_err(serde::de::Error::custom)
    }
}

/// Opaque application bytes plus an optional media type.
///
/// The bytes are not required to be UTF-8 (`contracts/TRANSPORT.md`), so
/// this holds `Vec<u8>` rather than `String`. A transport that quietly
/// required text would break the payload-agnostic guarantee every higher
/// layer is built on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payload {
    /// Present media type, or `None` for absence.
    ///
    /// `None` and `Some("")` are NOT the same thing, and the second cannot
    /// be constructed: the content fingerprint distinguishes absence from
    /// presence, so collapsing them would give two different messages one
    /// dedup identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<MediaType>,
    /// The opaque application bytes.
    pub bytes: Vec<u8>,
}

impl Payload {
    /// Build a payload, checking it against the effective limit.
    ///
    /// `limit` is the profile's effective `max_payload_bytes`, which may be
    /// lower than [`MAX_PAYLOAD_BYTES`] but is clamped so it can never be
    /// higher — a configuration that asked for more would otherwise widen a
    /// frozen protocol ceiling.
    ///
    /// # Errors
    /// Returns [`PayloadError::TooLarge`] when the bytes exceed the limit.
    pub fn new(
        media_type: Option<MediaType>,
        bytes: Vec<u8>,
        limit: usize,
    ) -> Result<Self, PayloadError> {
        let limit = limit.min(MAX_PAYLOAD_BYTES);
        if bytes.len() > limit {
            return Err(PayloadError::TooLarge { got: bytes.len(), limit });
        }
        Ok(Self { media_type, bytes })
    }

    /// Build a payload against the architecture ceiling.
    ///
    /// # Errors
    /// Returns [`PayloadError::TooLarge`] above [`MAX_PAYLOAD_BYTES`].
    pub fn at_ceiling(media_type: Option<MediaType>, bytes: Vec<u8>) -> Result<Self, PayloadError> {
        Self::new(media_type, bytes, MAX_PAYLOAD_BYTES)
    }

    /// Number of application bytes carried.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether the payload carries no bytes.
    ///
    /// An empty payload is legal: the wire framing writes its length field
    /// even when the length is zero.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_type_absence_and_emptiness_are_different() {
        assert_eq!(MediaType::parse(""), Err(PayloadError::EmptyMediaType));
        assert!(MediaType::parse("text/plain").is_ok());
        // Absence is representable only as None, which is the property the
        // content fingerprint depends on.
        let p = Payload::at_ceiling(None, b"hello".to_vec()).expect("valid");
        assert!(p.media_type.is_none());
    }

    #[test]
    fn media_type_is_bounded_and_printable_ascii() {
        assert!(MediaType::parse("m".repeat(128)).is_ok());
        assert_eq!(
            MediaType::parse("m".repeat(129)),
            Err(PayloadError::MediaTypeTooLong { got: 129 })
        );
        assert_eq!(
            MediaType::parse("text/\u{e9}"),
            Err(PayloadError::MediaTypeNotAscii { index: 5 })
        );
        assert_eq!(
            MediaType::parse("text/\tplain"),
            Err(PayloadError::MediaTypeNotAscii { index: 5 })
        );
    }

    #[test]
    fn payload_enforces_the_ceiling_and_allows_empty() {
        assert!(Payload::at_ceiling(None, vec![0; MAX_PAYLOAD_BYTES]).is_ok());
        assert_eq!(
            Payload::at_ceiling(None, vec![0; MAX_PAYLOAD_BYTES + 1]),
            Err(PayloadError::TooLarge { got: MAX_PAYLOAD_BYTES + 1, limit: MAX_PAYLOAD_BYTES })
        );
        let empty = Payload::at_ceiling(None, Vec::new()).expect("valid");
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
    }

    #[test]
    fn a_configured_limit_narrows_but_never_widens() {
        assert!(Payload::new(None, vec![0; 1024], 1024).is_ok());
        assert_eq!(
            Payload::new(None, vec![0; 1025], 1024),
            Err(PayloadError::TooLarge { got: 1025, limit: 1024 })
        );
        // A configuration asking for more than the frozen ceiling is
        // clamped down to it rather than honoured.
        assert_eq!(
            Payload::new(None, vec![0; MAX_PAYLOAD_BYTES + 1], usize::MAX),
            Err(PayloadError::TooLarge { got: MAX_PAYLOAD_BYTES + 1, limit: MAX_PAYLOAD_BYTES })
        );
    }

    #[test]
    fn payload_bytes_need_not_be_utf8() {
        let p = Payload::at_ceiling(None, vec![0xff, 0xfe, 0x00]).expect("valid");
        assert_eq!(p.len(), 3);
    }
}
