// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Media-type parsing and the bounded decode path (ADR-0050).
//!
//! # The cap aborts mid-stream, and that is the whole point
//!
//! Measured brotli expansion on hostile input exceeds 87,000×, so 48 KiB
//! of payload can name gigabytes of output. [`decode_envelope_bytes`]
//! therefore decompresses **incrementally** and stops the moment the
//! ceiling is passed. Decompressing first and checking the length after
//! would have already allocated whatever the attacker asked for — the
//! check would be a report, not a bound.
//!
//! There is deliberately no declared-length field to consult. A declared
//! length is peer-asserted metadata the cap must override anyway, so
//! honouring it would add an input to trust and no safety.

use std::io::Read;

/// The decompressed ceiling: 4 × the 49,152-byte transport payload limit.
pub const MAX_DECOMPRESSED_BYTES: usize = 196_608;

/// The v2 media type without a content encoding.
pub const MEDIA_TYPE_V2: &str = "application/vnd.interweave-human-chat+json;v=2";

/// The only defined content-encoding parameter value.
pub const CONTENT_ENCODING_BROTLI: &str = "br";

/// How the payload bytes are encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentEncoding {
    /// Raw UTF-8 JSON.
    Identity,
    /// The whole envelope, brotli-compressed.
    Brotli,
}

/// What a media type said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaTypeInfo {
    /// The declared envelope version.
    pub version: u32,
    /// How the bytes are encoded.
    pub encoding: ContentEncoding,
}

/// Why a media type or payload was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The media type is not a HumanChat envelope at all.
    NotHumanChat,
    /// The declared version is not 2.
    UnsupportedVersion {
        /// The version found.
        found: u32,
    },
    /// The version parameter was missing or unparseable.
    MalformedVersion,
    /// A `ce` parameter named something other than `br`.
    ///
    /// Rejected rather than ignored: an unknown encoding means the bytes
    /// are not what this decoder can read, and treating them as identity
    /// would hand a caller compressed data labelled as JSON.
    UnsupportedContentEncoding {
        /// The value found.
        found: String,
    },
    /// A duplicate parameter, which makes the media type ambiguous.
    DuplicateParameter {
        /// The repeated parameter name.
        name: String,
    },
    /// Decompressed output passed the ceiling.
    ///
    /// Reported as soon as the limit is crossed, before the rest of the
    /// stream is read.
    DecompressedTooLarge {
        /// The ceiling that was passed.
        limit: usize,
    },
    /// The compressed stream was malformed.
    MalformedCompressedStream,
    /// The decoded bytes were not UTF-8.
    NotUtf8,
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotHumanChat => write!(f, "not a HumanChat media type"),
            Self::UnsupportedVersion { found } => {
                write!(
                    f,
                    "envelope version {found} is not supported; this build implements 2"
                )
            }
            Self::MalformedVersion => write!(f, "the v parameter is missing or unparseable"),
            Self::UnsupportedContentEncoding { found } => {
                write!(
                    f,
                    "content encoding '{found}' is not defined; only 'br' exists"
                )
            }
            Self::DuplicateParameter { name } => {
                write!(f, "parameter '{name}' appears more than once")
            }
            Self::DecompressedTooLarge { limit } => {
                write!(f, "decompressed output passed the {limit}-byte ceiling")
            }
            Self::MalformedCompressedStream => write!(f, "the brotli stream is malformed"),
            Self::NotUtf8 => write!(f, "decoded bytes are not UTF-8"),
        }
    }
}

impl core::error::Error for DecodeError {}

/// Parse a HumanChatV2 media type.
///
/// Accepts `application/vnd.interweave-human-chat+json` with a `v`
/// parameter and an optional `ce`. Parameters are matched
/// case-insensitively on the name, as media-type parameters are, while
/// the values here are compared exactly — `BR` is not `br`, because the
/// contract names one spelling and accepting two would be a second way to
/// say the same thing.
///
/// # Errors
/// Returns [`DecodeError`] for a foreign type, a bad version, an unknown
/// encoding, or a duplicated parameter.
pub fn parse_media_type(media_type: &str) -> Result<MediaTypeInfo, DecodeError> {
    let mut parts = media_type.split(';');
    let base = parts.next().unwrap_or_default().trim();
    if !base.eq_ignore_ascii_case("application/vnd.interweave-human-chat+json") {
        return Err(DecodeError::NotHumanChat);
    }

    let mut version: Option<u32> = None;
    let mut encoding: Option<ContentEncoding> = None;

    for raw in parts {
        let param = raw.trim();
        let Some((name, value)) = param.split_once('=') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("v") {
            if version.is_some() {
                return Err(DecodeError::DuplicateParameter {
                    name: "v".to_owned(),
                });
            }
            version = Some(value.parse().map_err(|_| DecodeError::MalformedVersion)?);
        } else if name.eq_ignore_ascii_case("ce") {
            if encoding.is_some() {
                return Err(DecodeError::DuplicateParameter {
                    name: "ce".to_owned(),
                });
            }
            if value != CONTENT_ENCODING_BROTLI {
                return Err(DecodeError::UnsupportedContentEncoding {
                    found: value.to_owned(),
                });
            }
            encoding = Some(ContentEncoding::Brotli);
        }
    }

    let version = version.ok_or(DecodeError::MalformedVersion)?;
    if version != 2 {
        return Err(DecodeError::UnsupportedVersion { found: version });
    }
    Ok(MediaTypeInfo {
        version,
        encoding: encoding.unwrap_or(ContentEncoding::Identity),
    })
}

/// Decode payload bytes into the raw envelope text.
///
/// For [`ContentEncoding::Identity`] this validates UTF-8. For
/// [`ContentEncoding::Brotli`] it decompresses under the ceiling,
/// **aborting as soon as the limit is passed** rather than after.
///
/// # Errors
/// Returns [`DecodeError::DecompressedTooLarge`] when the ceiling is
/// crossed, [`DecodeError::MalformedCompressedStream`] for a bad stream,
/// or [`DecodeError::NotUtf8`].
pub fn decode_envelope_bytes(
    bytes: &[u8],
    encoding: ContentEncoding,
) -> Result<String, DecodeError> {
    match encoding {
        ContentEncoding::Identity => {
            if bytes.len() > MAX_DECOMPRESSED_BYTES {
                return Err(DecodeError::DecompressedTooLarge {
                    limit: MAX_DECOMPRESSED_BYTES,
                });
            }
            String::from_utf8(bytes.to_vec()).map_err(|_| DecodeError::NotUtf8)
        }
        ContentEncoding::Brotli => {
            let decoded = decompress_bounded(bytes, MAX_DECOMPRESSED_BYTES)?;
            String::from_utf8(decoded).map_err(|_| DecodeError::NotUtf8)
        }
    }
}

/// Decompress with a hard output ceiling, stopping the moment it is passed.
///
/// Reads in fixed chunks and checks after each, so peak memory is bounded
/// by `limit` plus one chunk regardless of what the stream claims to
/// contain. `Read::take` is what makes the bound real: the decompressor is
/// never asked for more than `limit + 1` bytes, and the extra byte is how
/// "exactly at the limit" is told apart from "over it".
fn decompress_bounded(input: &[u8], limit: usize) -> Result<Vec<u8>, DecodeError> {
    let mut reader = brotli::Decompressor::new(input, 4096)
        .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1));
    let mut out = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                out.extend_from_slice(&chunk[..n]);
                if out.len() > limit {
                    // Passed the ceiling: stop now, with at most one chunk
                    // of overshoot held, rather than finishing the stream.
                    return Err(DecodeError::DecompressedTooLarge { limit });
                }
            }
            Err(_) => return Err(DecodeError::MalformedCompressedStream),
        }
    }
    Ok(out)
}

/// Whether a sender may compress an envelope of this size.
///
/// The legal range is `max_payload_bytes < raw <= MAX_DECOMPRESSED_BYTES`.
/// Above the ceiling the message is too large **before** compression is
/// considered: compressibility does not extend the ceiling, and a
/// repetitive 300 KB document that compresses under the payload limit
/// would be sender-conforming and refused by every conforming receiver.
#[must_use]
pub const fn sender_may_compress(raw_len: usize, max_payload_bytes: usize) -> bool {
    raw_len > max_payload_bytes && raw_len <= MAX_DECOMPRESSED_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compress(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut input = bytes;
        std::io::copy(
            &mut brotli::CompressorReader::new(&mut input, 4096, 5, 22),
            &mut out,
        )
        .expect("compresses");
        out
    }

    #[test]
    fn the_plain_and_compressed_media_types_parse() {
        assert_eq!(
            parse_media_type(MEDIA_TYPE_V2).expect("parses"),
            MediaTypeInfo {
                version: 2,
                encoding: ContentEncoding::Identity
            }
        );
        assert_eq!(
            parse_media_type(&format!("{MEDIA_TYPE_V2};ce=br")).expect("parses"),
            MediaTypeInfo {
                version: 2,
                encoding: ContentEncoding::Brotli
            }
        );
        // Whitespace around parameters is normal in media types.
        assert!(parse_media_type("application/vnd.interweave-human-chat+json; v=2; ce=br").is_ok());
    }

    #[test]
    fn a_v1_media_type_is_refused_rather_than_upgraded() {
        assert_eq!(
            parse_media_type("application/vnd.interweave-human-chat+json;v=1"),
            Err(DecodeError::UnsupportedVersion { found: 1 })
        );
        assert_eq!(
            parse_media_type("text/plain"),
            Err(DecodeError::NotHumanChat)
        );
        assert_eq!(
            parse_media_type("application/vnd.interweave-human-chat+json"),
            Err(DecodeError::MalformedVersion)
        );
    }

    #[test]
    fn an_unknown_content_encoding_is_refused_not_ignored() {
        // Ignoring it would hand a caller compressed bytes labelled JSON.
        assert_eq!(
            parse_media_type(&format!("{MEDIA_TYPE_V2};ce=gzip")),
            Err(DecodeError::UnsupportedContentEncoding {
                found: "gzip".to_owned()
            })
        );
        // One spelling only: BR is not br.
        assert!(parse_media_type(&format!("{MEDIA_TYPE_V2};ce=BR")).is_err());
    }

    #[test]
    fn a_duplicated_parameter_is_ambiguous_and_refused() {
        assert_eq!(
            parse_media_type(&format!("{MEDIA_TYPE_V2};v=3")),
            Err(DecodeError::DuplicateParameter {
                name: "v".to_owned()
            })
        );
        assert!(parse_media_type(&format!("{MEDIA_TYPE_V2};ce=br;ce=br")).is_err());
    }

    #[test]
    fn a_compressed_envelope_round_trips() {
        let body = r#"{"v":2,"kind":"text","app_message_id":"00000000000000000000000000000000","text":"hello"}"#;
        let packed = compress(body.as_bytes());
        assert_eq!(
            decode_envelope_bytes(&packed, ContentEncoding::Brotli).expect("decodes"),
            body
        );
    }

    #[test]
    fn a_decompression_bomb_is_stopped_at_the_ceiling() {
        // 4 MiB of zeros compresses to a few hundred bytes. A decoder that
        // decompressed first and measured after would have allocated all
        // of it before noticing.
        let bomb = compress(&vec![0u8; 4 * 1024 * 1024]);
        assert!(
            bomb.len() < 4096,
            "the test input should be small, got {} bytes",
            bomb.len()
        );
        assert_eq!(
            decode_envelope_bytes(&bomb, ContentEncoding::Brotli),
            Err(DecodeError::DecompressedTooLarge {
                limit: MAX_DECOMPRESSED_BYTES
            })
        );
    }

    #[test]
    fn exactly_the_ceiling_is_accepted_and_one_byte_over_is_not() {
        // The boundary is the interesting case: the ceiling is a legal
        // size, not the first illegal one.
        let at = compress(&vec![b'a'; MAX_DECOMPRESSED_BYTES]);
        let decoded = decode_envelope_bytes(&at, ContentEncoding::Brotli).expect("at ceiling");
        assert_eq!(decoded.len(), MAX_DECOMPRESSED_BYTES);

        let over = compress(&vec![b'a'; MAX_DECOMPRESSED_BYTES + 1]);
        assert_eq!(
            decode_envelope_bytes(&over, ContentEncoding::Brotli),
            Err(DecodeError::DecompressedTooLarge {
                limit: MAX_DECOMPRESSED_BYTES
            })
        );
    }

    #[test]
    fn a_malformed_stream_is_reported_as_such() {
        assert_eq!(
            decode_envelope_bytes(&[0xff; 64], ContentEncoding::Brotli),
            Err(DecodeError::MalformedCompressedStream)
        );
    }

    #[test]
    fn identity_bytes_are_bounded_too() {
        // A raw envelope has no expansion step, but the ceiling still
        // applies: the receiver's bound is on content, not on encoding.
        let big = vec![b'a'; MAX_DECOMPRESSED_BYTES + 1];
        assert_eq!(
            decode_envelope_bytes(&big, ContentEncoding::Identity),
            Err(DecodeError::DecompressedTooLarge {
                limit: MAX_DECOMPRESSED_BYTES
            })
        );
        assert!(decode_envelope_bytes(b"{}", ContentEncoding::Identity).is_ok());
        assert_eq!(
            decode_envelope_bytes(&[0xff, 0xfe], ContentEncoding::Identity),
            Err(DecodeError::NotUtf8)
        );
    }

    #[test]
    fn the_senders_compression_range_is_bounded_at_both_ends() {
        let limit = 49_152;
        // Fits raw: must not compress.
        assert!(!sender_may_compress(1_000, limit));
        assert!(!sender_may_compress(limit, limit));
        // Between the payload limit and the ceiling: may compress.
        assert!(sender_may_compress(limit + 1, limit));
        assert!(sender_may_compress(MAX_DECOMPRESSED_BYTES, limit));
        // Above the ceiling: too large BEFORE compression is considered,
        // however well it would have compressed.
        assert!(!sender_may_compress(MAX_DECOMPRESSED_BYTES + 1, limit));
        assert!(!sender_may_compress(300_000, limit));
    }
}
