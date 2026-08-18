// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The length-prefixed frame codec.
//!
//! Four big-endian bytes of length, then that many bytes of UTF-8 JSON.
//! The prefix is outside the count, zero length is invalid, and the body
//! ceiling is 131,072 bytes (`contracts/LOCAL-IPC.md` §Framing).
//!
//! # The decoder never allocates on a declared length
//!
//! The length prefix arrives from the other side of the socket, so it is
//! untrusted input even on a local connection: an owner-protected socket
//! bounds *who* may connect, not what a buggy or hostile client sends once
//! connected. [`decode_frame`] therefore checks the declared length
//! against the ceiling **before** looking at the buffer, and reports how
//! many more bytes are needed rather than reserving them. A decoder that
//! reserved 4 GiB because a peer said so would have conceded the resource
//! the bound exists to protect.

/// Bytes of length prefix, outside the counted body.
pub const LENGTH_PREFIX_BYTES: usize = 4;

/// Maximum JSON body in one frame (ADR-0026).
///
/// Sized so every legal 48 KiB payload fits in both an IPC command and a
/// `MessageReceived` event after base64url expansion and envelope
/// overhead — the payload-fit invariant, whose worst case is measured in
/// `fixtures/ipc-v2/ipc-v2-payload-fit.json`.
pub const MAX_BODY_BYTES: usize = 131_072;

/// Why a frame could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// The declared body length exceeds the ceiling.
    ///
    /// Reported before any allocation: the length is a claim, not a fact.
    BodyTooLarge {
        /// Length the prefix declared.
        declared: usize,
        /// Maximum permitted.
        max: usize,
    },
    /// The prefix declared a zero-length body, which is invalid.
    ZeroLength,
    /// Not enough bytes yet. Not an error condition for a stream reader —
    /// it says exactly how many more are needed.
    Incomplete {
        /// Additional bytes required.
        needed: usize,
    },
    /// The body was not valid UTF-8.
    NotUtf8,
    /// The body did not parse as a JSON object.
    NotJson {
        /// The parser's message.
        detail: String,
    },
}

impl core::fmt::Display for FrameError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BodyTooLarge { declared, max } => {
                write!(f, "frame declares {declared} bytes; the ceiling is {max}")
            }
            Self::ZeroLength => write!(f, "a zero-length frame body is invalid"),
            Self::Incomplete { needed } => write!(f, "{needed} more bytes needed"),
            Self::NotUtf8 => write!(f, "frame body is not valid UTF-8"),
            Self::NotJson { detail } => write!(f, "frame body is not JSON: {detail}"),
        }
    }
}

impl core::error::Error for FrameError {}

/// Encode a JSON body into a complete frame.
///
/// # Errors
/// Returns [`FrameError::BodyTooLarge`] above [`MAX_BODY_BYTES`] or
/// [`FrameError::ZeroLength`] for an empty body — checked on the way out
/// as well as in, so a local bug cannot emit a frame no conforming peer
/// would accept.
pub fn encode_frame(body: &str) -> Result<Vec<u8>, FrameError> {
    let bytes = body.as_bytes();
    if bytes.is_empty() {
        return Err(FrameError::ZeroLength);
    }
    if bytes.len() > MAX_BODY_BYTES {
        return Err(FrameError::BodyTooLarge {
            declared: bytes.len(),
            max: MAX_BODY_BYTES,
        });
    }
    let mut out = Vec::with_capacity(LENGTH_PREFIX_BYTES + bytes.len());
    // Big-endian, matching DirectMessageV2 and the content fingerprint.
    // The three agreeing is the only choice under which one repository
    // has one byte order.
    let len = u32::try_from(bytes.len()).map_err(|_| FrameError::BodyTooLarge {
        declared: bytes.len(),
        max: MAX_BODY_BYTES,
    })?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(out)
}

/// What a successful decode produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFrame {
    /// The JSON body.
    pub body: String,
    /// Total bytes consumed, prefix included.
    pub consumed: usize,
}

/// Decode one frame from the front of a buffer.
///
/// Returns [`FrameError::Incomplete`] when more bytes are needed, so a
/// stream reader can loop without the codec holding any state of its own.
///
/// # Errors
/// Returns [`FrameError`] for an over-ceiling or zero declared length, a
/// non-UTF-8 body, or a body that is not JSON.
pub fn decode_frame(buffer: &[u8]) -> Result<DecodedFrame, FrameError> {
    let Some(prefix) = buffer.get(..LENGTH_PREFIX_BYTES) else {
        return Err(FrameError::Incomplete {
            needed: LENGTH_PREFIX_BYTES - buffer.len(),
        });
    };
    let mut raw = [0u8; LENGTH_PREFIX_BYTES];
    raw.copy_from_slice(prefix);
    let declared = u32::from_be_bytes(raw) as usize;

    // ORDER MATTERS. The ceiling is checked against the DECLARED length
    // before the buffer is consulted, so a hostile prefix is refused
    // without reserving anything.
    if declared == 0 {
        return Err(FrameError::ZeroLength);
    }
    if declared > MAX_BODY_BYTES {
        return Err(FrameError::BodyTooLarge {
            declared,
            max: MAX_BODY_BYTES,
        });
    }

    let total = LENGTH_PREFIX_BYTES + declared;
    let Some(body) = buffer.get(LENGTH_PREFIX_BYTES..total) else {
        return Err(FrameError::Incomplete {
            needed: total - buffer.len(),
        });
    };
    let text = core::str::from_utf8(body).map_err(|_| FrameError::NotUtf8)?;
    serde_json::from_str::<serde_json::Value>(text).map_err(|e| FrameError::NotJson {
        detail: e.to_string(),
    })?;
    Ok(DecodedFrame {
        body: text.to_owned(),
        consumed: total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_round_trips() {
        let body = r#"{"type":"hello"}"#;
        let framed = encode_frame(body).expect("encodes");
        assert_eq!(&framed[..4], &(body.len() as u32).to_be_bytes());
        let decoded = decode_frame(&framed).expect("decodes");
        assert_eq!(decoded.body, body);
        assert_eq!(decoded.consumed, framed.len());
    }

    #[test]
    fn the_prefix_is_big_endian() {
        // 258 = 0x0102. Little-endian would put 0x02 first, and the frame
        // would disagree with DirectMessageV2 and the fingerprint.
        let body = format!("{:258}", 1);
        let framed = encode_frame(&body).expect("encodes");
        assert_eq!(&framed[..4], &[0x00, 0x00, 0x01, 0x02]);
    }

    #[test]
    fn an_over_ceiling_declaration_is_refused_without_allocating() {
        // Only the four prefix bytes exist. A decoder that trusted the
        // declared length would try to reserve 4 GiB here.
        let hostile = u32::MAX.to_be_bytes();
        assert_eq!(
            decode_frame(&hostile),
            Err(FrameError::BodyTooLarge {
                declared: u32::MAX as usize,
                max: MAX_BODY_BYTES,
            })
        );
        // And one byte over the ceiling is refused just as early.
        let over = ((MAX_BODY_BYTES + 1) as u32).to_be_bytes();
        assert!(matches!(
            decode_frame(&over),
            Err(FrameError::BodyTooLarge { .. })
        ));
    }

    #[test]
    fn zero_length_is_invalid_in_both_directions() {
        assert_eq!(encode_frame(""), Err(FrameError::ZeroLength));
        assert_eq!(
            decode_frame(&0u32.to_be_bytes()),
            Err(FrameError::ZeroLength)
        );
    }

    #[test]
    fn an_incomplete_frame_says_how_much_more_is_needed() {
        assert_eq!(
            decode_frame(&[0x00, 0x00]),
            Err(FrameError::Incomplete { needed: 2 })
        );
        let framed = encode_frame(r#"{"a":1}"#).expect("encodes");
        assert_eq!(
            decode_frame(&framed[..framed.len() - 3]),
            Err(FrameError::Incomplete { needed: 3 })
        );
    }

    #[test]
    fn a_body_at_the_ceiling_is_accepted_and_one_over_is_not() {
        // The ceiling is a legal size, not the first illegal one.
        // The JSON wrapper {"a":"…"} is exactly 8 bytes.
        let filler = "x".repeat(MAX_BODY_BYTES - 8);
        let body = format!(r#"{{"a":"{filler}"}}"#);
        assert_eq!(body.len(), MAX_BODY_BYTES);
        assert!(encode_frame(&body).is_ok());
        assert!(encode_frame(&format!("{body} ")).is_err());
    }

    #[test]
    fn malformed_bodies_are_reported_by_kind() {
        let mut bad = 4u32.to_be_bytes().to_vec();
        bad.extend_from_slice(&[0xff, 0xfe, 0xfd, 0xfc]);
        assert_eq!(decode_frame(&bad), Err(FrameError::NotUtf8));

        let framed = encode_frame("not json at all").expect("encodes");
        assert!(matches!(
            decode_frame(&framed),
            Err(FrameError::NotJson { .. })
        ));
    }

    #[test]
    fn trailing_bytes_belong_to_the_next_frame() {
        // `consumed` is what lets a reader advance without the codec
        // holding stream state.
        let first = encode_frame(r#"{"n":1}"#).expect("encodes");
        let second = encode_frame(r#"{"n":2}"#).expect("encodes");
        let mut stream = first.clone();
        stream.extend_from_slice(&second);
        let decoded = decode_frame(&stream).expect("decodes");
        assert_eq!(decoded.consumed, first.len());
        let next = decode_frame(&stream[decoded.consumed..]).expect("decodes");
        assert_eq!(next.body, r#"{"n":2}"#);
    }
}
