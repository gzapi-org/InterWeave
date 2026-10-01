// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The send side (ADR-0050, plan §17 (3)): an envelope's payload bytes and
//! the media type that names them.
//!
//! Compression is a FIT FALLBACK, never a default (`HUMAN-CHAT.md`
//! §Compression). The raw envelope goes out as itself whenever it fits
//! the transport payload limit; only above that limit, and only while the
//! raw envelope is within the receiver's decompressed ceiling, is the
//! whole envelope brotli-compressed as `;ce=br`. A raw envelope over the
//! ceiling is too large BEFORE compression is considered, and a
//! compressed form still over the payload limit is too large too: no
//! chunking, no reassembly, no downgrade.

use std::io::Read as _;

use crate::decode::{MAX_DECOMPRESSED_BYTES, MEDIA_TYPE_V2, sender_may_compress};
use crate::envelope::HumanChatV2;

/// Brotli quality: the densest, since only a document already over the
/// payload limit is compressed and every byte saved is a byte that fits.
const QUALITY: u32 = 11;

/// Brotli window: `lgwin` 22, a 4 MiB window -- wider than any input the
/// ceiling admits, so the whole envelope is one window.
const WINDOW: u32 = 22;

/// What a sender puts on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoded {
    /// `MEDIA_TYPE_V2`, with `;ce=br` when the bytes are compressed.
    pub media_type: String,
    /// The payload: the raw UTF-8 JSON, or that JSON brotli-compressed.
    pub bytes: Vec<u8>,
}

/// Why an envelope cannot be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// The raw envelope is over the decompressed ceiling every receiver
    /// enforces: too large before compression is considered.
    OverCeiling {
        /// The raw envelope's size.
        raw: usize,
        /// [`MAX_DECOMPRESSED_BYTES`].
        ceiling: usize,
    },
    /// Compressed, it is still over the payload limit.
    DoesNotFit {
        /// The compressed size.
        compressed: usize,
        /// The transport payload limit it had to fit.
        max_payload_bytes: usize,
    },
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OverCeiling { raw, ceiling } => write!(
                f,
                "the envelope is {raw} bytes, over the {ceiling}-byte decompressed ceiling"
            ),
            Self::DoesNotFit {
                compressed,
                max_payload_bytes,
            } => write!(
                f,
                "compressed, the envelope is {compressed} bytes, over the {max_payload_bytes}-byte payload limit"
            ),
        }
    }
}

impl std::error::Error for EncodeError {}

/// The payload and media type for `envelope` under a transport payload
/// limit of `max_payload_bytes`.
///
/// # Errors
/// [`EncodeError::OverCeiling`] for a raw envelope over the decompressed
/// ceiling, whatever it compresses to; [`EncodeError::DoesNotFit`] when
/// even compressed it is over the payload limit.
#[expect(
    clippy::missing_panics_doc,
    reason = "serializing a typed envelope to JSON cannot fail: every field is a string, an integer or an endpoint id"
)]
pub fn encode_outbound(
    envelope: &HumanChatV2,
    max_payload_bytes: usize,
) -> Result<Encoded, EncodeError> {
    #[expect(
        clippy::expect_used,
        reason = "serializing a typed envelope to JSON cannot fail: every field is a string, an integer or an endpoint id"
    )]
    let raw = serde_json::to_vec(envelope).expect("an envelope serializes");
    if raw.len() <= max_payload_bytes {
        return Ok(Encoded {
            media_type: MEDIA_TYPE_V2.to_owned(),
            bytes: raw,
        });
    }
    if !sender_may_compress(raw.len(), max_payload_bytes) {
        return Err(EncodeError::OverCeiling {
            raw: raw.len(),
            ceiling: MAX_DECOMPRESSED_BYTES,
        });
    }
    let compressed = compress(&raw);
    if compressed.len() > max_payload_bytes {
        return Err(EncodeError::DoesNotFit {
            compressed: compressed.len(),
            max_payload_bytes,
        });
    }
    Ok(Encoded {
        media_type: format!(
            "{MEDIA_TYPE_V2};ce={}",
            crate::decode::CONTENT_ENCODING_BROTLI
        ),
        bytes: compressed,
    })
}

/// The whole of `bytes`, brotli-compressed. Bounded by its input: only an
/// envelope within the ceiling reaches it.
#[expect(
    clippy::expect_used,
    reason = "a compressor error would leave a TRUNCATED stream, which shipped as `;ce=br` \
              every receiver refuses: panicking here is the loud failure, and reading from \
              memory into memory leaves only the encoder's own invalid-state error"
)]
pub(crate) fn compress(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    brotli::CompressorReader::new(bytes, 4096, QUALITY, WINDOW)
        .read_to_end(&mut out)
        .expect("in-memory brotli compression completes");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::{ContentEncoding, decode_envelope_bytes, parse_media_type};
    use crate::envelope::MessageKind;

    const LIMIT: usize = 49_152;

    fn envelope(text: String) -> HumanChatV2 {
        HumanChatV2 {
            v: 2,
            kind: MessageKind::Text,
            app_message_id: "0".repeat(32),
            text,
            reply_to: None,
            sent_at_ms: None,
            from_endpoint: None,
        }
    }

    /// Text that does not compress: a deterministic pseudo-random stream
    /// over 64 symbols, so a test does not depend on a seed it cannot see.
    fn noise(len: usize) -> String {
        const SYMBOLS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                char::from(SYMBOLS[usize::try_from(state % 64).unwrap_or(0)])
            })
            .collect()
    }

    fn raw_len(envelope: &HumanChatV2) -> usize {
        serde_json::to_vec(envelope).expect("serializes").len()
    }

    /// What fits goes out raw, whatever it would compress to: compression
    /// is a fit fallback, never a default. Exactly at the limit is raw.
    #[test]
    fn what_fits_goes_out_raw() {
        let small = envelope("hello".to_owned());
        let encoded = encode_outbound(&small, LIMIT).expect("fits");
        assert_eq!(encoded.media_type, MEDIA_TYPE_V2);
        assert_eq!(encoded.bytes, serde_json::to_vec(&small).expect("ser"));

        let overhead = raw_len(&envelope(String::new()));
        let at_limit = envelope("a".repeat(LIMIT - overhead));
        assert_eq!(raw_len(&at_limit), LIMIT);
        let encoded = encode_outbound(&at_limit, LIMIT).expect("fits exactly");
        assert_eq!(encoded.media_type, MEDIA_TYPE_V2, "at the limit, raw");
    }

    /// One byte over the limit, a compressible envelope goes as `;ce=br`,
    /// and the receiver's own decode path gives back the raw envelope.
    #[test]
    fn over_the_limit_a_compressible_envelope_is_compressed_and_decodes_back() {
        let overhead = raw_len(&envelope(String::new()));
        let over = envelope(
            "compressible "
                .repeat(LIMIT)
                .chars()
                .take(LIMIT + 1 - overhead)
                .collect(),
        );
        assert_eq!(raw_len(&over), LIMIT + 1);
        let encoded = encode_outbound(&over, LIMIT).expect("compresses to fit");
        assert_eq!(encoded.media_type, format!("{MEDIA_TYPE_V2};ce=br"));
        assert!(encoded.bytes.len() <= LIMIT);
        let info = parse_media_type(&encoded.media_type).expect("the receiver parses it");
        assert_eq!(info.encoding, ContentEncoding::Brotli);
        let decoded = decode_envelope_bytes(&encoded.bytes, info.encoding).expect("decodes");
        assert_eq!(HumanChatV2::parse(&decoded).expect("valid"), over);
    }

    /// Exactly at the ceiling compresses; one byte over is too large,
    /// however well it would compress.
    #[test]
    fn the_ceiling_bounds_the_raw_envelope_before_compression() {
        let overhead = raw_len(&envelope(String::new()));
        let at = envelope("z".repeat(MAX_DECOMPRESSED_BYTES - overhead));
        assert_eq!(raw_len(&at), MAX_DECOMPRESSED_BYTES);
        assert!(encode_outbound(&at, LIMIT).is_ok(), "at the ceiling");
        let over = envelope("z".repeat(MAX_DECOMPRESSED_BYTES + 1 - overhead));
        assert_eq!(
            encode_outbound(&over, LIMIT),
            Err(EncodeError::OverCeiling {
                raw: MAX_DECOMPRESSED_BYTES + 1,
                ceiling: MAX_DECOMPRESSED_BYTES
            })
        );
        // A repetitive 300 KB document: it would compress far under the
        // limit, and every receiver would abort it -- refused here.
        assert!(matches!(
            encode_outbound(&envelope("z".repeat(300_000)), LIMIT),
            Err(EncodeError::OverCeiling { .. })
        ));
    }

    /// Within the ceiling but incompressible: still over the limit, so too
    /// large, never chunked.
    #[test]
    fn an_envelope_that_does_not_compress_under_the_limit_is_refused() {
        let noisy = envelope(noise(LIMIT * 2));
        match encode_outbound(&noisy, LIMIT) {
            Err(EncodeError::DoesNotFit {
                compressed,
                max_payload_bytes,
            }) => {
                assert!(compressed > LIMIT);
                assert_eq!(max_payload_bytes, LIMIT);
            }
            other => panic!("too large compressed, got {other:?}"),
        }
    }

    /// `compress` returns the WHOLE stream, never a truncated one: the
    /// worst case for the encoder -- incompressible input at the ceiling --
    /// decodes back to every byte it was given.
    #[test]
    fn compress_emits_a_complete_stream_for_incompressible_input_at_the_ceiling() {
        let raw = noise(MAX_DECOMPRESSED_BYTES).into_bytes();
        let decoded =
            decode_envelope_bytes(&compress(&raw), ContentEncoding::Brotli).expect("decodes");
        assert_eq!(decoded.as_bytes(), raw.as_slice());
    }
}
