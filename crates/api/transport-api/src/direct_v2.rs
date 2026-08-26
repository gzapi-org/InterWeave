// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The `DirectMessageV2` wire frame and its two response shapes.
//!
//! Hand-written against frozen bytes, not derived. The layout is pinned
//! by `architecture/transport/libp2p/DIRECT.md` and by six vectors in
//! `fixtures/direct-v2/direct-message-v2-frame.json`; a derived codec
//! would be free to change its encoding when a field was reordered or a
//! type widened, and the fixtures exist precisely because that must be a
//! protocol decision rather than a refactor.
//!
//! ```text
//! message_id:16 || sent_at_ms:u64be || source_endpoint_len:u8 ||
//! source_endpoint || destination_endpoint_len:u8 || destination_endpoint ||
//! media_type_len:u8 || media_type || payload_len:u32be || payload
//! ```
//!
//! Every multi-byte integer is **big-endian**. That is not a preference:
//! the IPC frame's 4-byte length prefix and `DirectContentFingerprintV1`'s
//! u16be/u32be lengths are already big-endian, and this is the only choice
//! under which all three agree about one repository's byte order.
//!
//! Two absences are encoded as zero lengths and mean different things:
//! `destination_endpoint_len = 0` requests the receiver's configured
//! default endpoint and never fan-out, while `media_type_len = 0` encodes
//! an ABSENT media type — never an empty string, which does not exist on
//! this wire and which would hash differently in the content fingerprint.

use crate::ids::{EndpointId, IdError, MessageId};
use crate::payload::{MAX_MEDIA_TYPE_BYTES, MAX_PAYLOAD_BYTES, MediaType, Payload, PayloadError};
use crate::status::DirectRejectReason;

/// Bytes of the fixed-size prefix: `message_id` plus `sent_at_ms`.
const FIXED_PREFIX: usize = 16 + 8;

/// Why a frame did not decode.
///
/// Every variant is a **local** diagnostic. On the wire they all collapse
/// to [`DirectRejectReason::Malformed`] or, for an over-ceiling payload,
/// [`DirectRejectReason::TooLarge`] — a decoder that reported which field
/// was wrong would let a peer probe the parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// The buffer ended before a declared field did.
    Truncated {
        /// What was being read when the bytes ran out.
        field: &'static str,
        /// Bytes the field declared it needed.
        needed: usize,
        /// Bytes actually left.
        available: usize,
    },
    /// The declared payload exceeded the effective ceiling.
    ///
    /// Separate from [`Self::DeclaredTooLong`] because it is the one
    /// frame error with its own wire code, and deciding that by matching
    /// on a field NAME would mean renaming a string silently changed what
    /// a peer is told.
    PayloadTooLarge {
        /// The declared length.
        got: usize,
        /// The ceiling in force.
        max: usize,
    },
    /// A declared length exceeded its ceiling.
    ///
    /// Refused **before** the allocation it would have caused, which is
    /// the whole reason lengths are checked rather than trusted.
    DeclaredTooLong {
        /// Which field declared it.
        field: &'static str,
        /// The declared length.
        got: usize,
        /// The ceiling in force.
        max: usize,
    },
    /// A required endpoint label was declared zero-length.
    ///
    /// `source_endpoint` is always present; only the destination may be
    /// omitted, and only by the destination's own zero length.
    EmptySourceEndpoint,
    /// An endpoint label did not satisfy [`EndpointId`] grammar.
    Endpoint {
        /// Which field carried it.
        field: &'static str,
        /// The grammar failure.
        error: IdError,
    },
    /// The media type was present but not a legal one.
    Media(PayloadError),
    /// Bytes remained after the frame was fully decoded.
    ///
    /// A trailing-garbage frame is refused rather than ignored: accepting
    /// it would let two different byte strings decode to one message, and
    /// the dedup fingerprint could then disagree with the wire.
    TrailingBytes {
        /// How many bytes were left over.
        extra: usize,
    },
}

impl FrameError {
    /// The coarse code a peer may receive.
    ///
    /// Only an over-ceiling payload is distinguishable, and only because
    /// `too_large` is already a documented reason code that tells an
    /// honest sender something actionable. Everything else is
    /// `malformed`.
    #[must_use]
    pub const fn to_wire(&self) -> DirectRejectReason {
        match self {
            Self::PayloadTooLarge { .. } => DirectRejectReason::TooLarge,
            _ => DirectRejectReason::Malformed,
        }
    }
}

impl core::fmt::Display for FrameError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Truncated {
                field,
                needed,
                available,
            } => write!(
                f,
                "frame ended inside `{field}`: needed {needed} bytes, {available} left"
            ),
            Self::PayloadTooLarge { got, max } => {
                write!(f, "payload declared {got} bytes; the limit is {max}")
            }
            Self::DeclaredTooLong { field, got, max } => {
                write!(f, "`{field}` declared {got} bytes; the limit is {max}")
            }
            Self::EmptySourceEndpoint => {
                write!(f, "source_endpoint_len is 0; the source is never omitted")
            }
            Self::Endpoint { field, error } => write!(f, "`{field}`: {error}"),
            Self::Media(e) => write!(f, "media type: {e}"),
            Self::TrailingBytes { extra } => {
                write!(f, "{extra} byte(s) remained after the frame")
            }
        }
    }
}

impl core::error::Error for FrameError {}

/// One directed message, as it appears on `/interweave/direct/2.0.0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectMessageV2 {
    /// The sender's idempotency key. Any 128-bit value.
    pub message_id: MessageId,
    /// When the sender says it sent this.
    ///
    /// **Diagnostic only.** Not authorization, ordering, freshness, a
    /// replay window, or dedup input, and excluded from the content
    /// fingerprint — a retry may carry a different one.
    pub sent_at_ms: u64,
    /// Which local endpoint of the SENDER produced this.
    ///
    /// Peer-asserted metadata on arrival. The sender's own runtime
    /// derives it from an endpoint lease and never from caller input,
    /// but a receiver cannot verify that and must not treat it as
    /// authorization.
    pub source_endpoint: EndpointId,
    /// Which endpoint of the RECEIVER this is for.
    ///
    /// `None` requests the receiver's configured `default_direct_endpoint`
    /// and never every local client.
    pub destination_endpoint: Option<EndpointId>,
    /// The application bytes and their advisory media type.
    pub payload: Payload,
}

impl DirectMessageV2 {
    /// Encode to the frozen layout.
    ///
    /// Total size is computable in advance, so the buffer is allocated
    /// once. Every length written here was validated when its component
    /// type was constructed, which is why this cannot fail.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let source = self.source_endpoint.as_str().as_bytes();
        let destination = self
            .destination_endpoint
            .as_ref()
            .map_or(&[][..], |e| e.as_str().as_bytes());
        let media = self
            .payload
            .media_type()
            .map_or(&[][..], |m| m.as_str().as_bytes());
        let bytes = self.payload.bytes();

        let mut out = Vec::with_capacity(
            FIXED_PREFIX
                + 1
                + source.len()
                + 1
                + destination.len()
                + 1
                + media.len()
                + 4
                + bytes.len(),
        );
        out.extend_from_slice(self.message_id.as_bytes());
        out.extend_from_slice(&self.sent_at_ms.to_be_bytes());

        // Each length is a u8 by construction: EndpointId caps at 64 and
        // MediaType at 128, both below 256, and both were checked when the
        // value was parsed.
        out.push(u8::try_from(source.len()).unwrap_or(u8::MAX));
        out.extend_from_slice(source);
        out.push(u8::try_from(destination.len()).unwrap_or(u8::MAX));
        out.extend_from_slice(destination);
        out.push(u8::try_from(media.len()).unwrap_or(u8::MAX));
        out.extend_from_slice(media);

        // u32be even when zero: an empty payload still carries its length,
        // which is what lets a decoder tell "no payload" from "frame ended".
        out.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(u32::MAX).to_be_bytes());
        out.extend_from_slice(bytes);
        out
    }

    /// Decode from the frozen layout.
    ///
    /// **Every declared length is checked against its ceiling before the
    /// bytes it describes are read**, so a hostile frame claiming a 4 GiB
    /// payload is refused at the length field rather than at the
    /// allocation. `limit` is the profile's effective
    /// `max_payload_bytes`, clamped to [`MAX_PAYLOAD_BYTES`] so a
    /// configuration cannot widen a frozen ceiling.
    ///
    /// # Errors
    /// Returns [`FrameError`] naming the field that failed; all of them
    /// collapse to a coarse code via [`FrameError::to_wire`].
    pub fn decode(buffer: &[u8], limit: usize) -> Result<Self, FrameError> {
        let mut cursor = Cursor::new(buffer);

        let message_id = MessageId::from_bytes(cursor.take_array::<16>("message_id")?);
        let sent_at_ms = u64::from_be_bytes(cursor.take_array::<8>("sent_at_ms")?);

        let source_endpoint = {
            let len = usize::from(cursor.take_u8("source_endpoint_len")?);
            if len == 0 {
                return Err(FrameError::EmptySourceEndpoint);
            }
            ceiling("source_endpoint", len, EndpointId::MAX_BYTES)?;
            endpoint("source_endpoint", cursor.take("source_endpoint", len)?)?
        };

        let destination_endpoint = {
            let len = usize::from(cursor.take_u8("destination_endpoint_len")?);
            if len == 0 {
                // ABSENCE, and it means the receiver's default endpoint.
                None
            } else {
                ceiling("destination_endpoint", len, EndpointId::MAX_BYTES)?;
                Some(endpoint(
                    "destination_endpoint",
                    cursor.take("destination_endpoint", len)?,
                )?)
            }
        };

        let media_type = {
            let len = usize::from(cursor.take_u8("media_type_len")?);
            if len == 0 {
                // ABSENCE, never `Some("")`. An empty media type does not
                // exist on this wire, and the two hash differently in
                // DirectContentFingerprintV1.
                None
            } else {
                ceiling("media_type", len, MAX_MEDIA_TYPE_BYTES)?;
                let raw = cursor.take("media_type", len)?;
                let text = core::str::from_utf8(raw)
                    .map_err(|_| FrameError::Media(PayloadError::MediaTypeNotAscii { index: 0 }))?;
                Some(MediaType::parse(text).map_err(FrameError::Media)?)
            }
        };

        let payload = {
            let declared = u32::from_be_bytes(cursor.take_array::<4>("payload_len")?);
            let len = usize::try_from(declared).unwrap_or(usize::MAX);
            // BEFORE the read, so an enormous declared length costs a
            // comparison rather than an allocation.
            let max = limit.min(MAX_PAYLOAD_BYTES);
            if len > max {
                return Err(FrameError::PayloadTooLarge { got: len, max });
            }
            let bytes = cursor.take("payload", len)?.to_vec();
            Payload::new(media_type, bytes, limit).map_err(FrameError::Media)?
        };

        let extra = cursor.remaining();
        if extra > 0 {
            return Err(FrameError::TrailingBytes { extra });
        }

        Ok(Self {
            message_id,
            sent_at_ms,
            source_endpoint,
            destination_endpoint,
            payload,
        })
    }
}

/// Refuse a declared length above its ceiling, before it is acted on.
fn ceiling(field: &'static str, got: usize, max: usize) -> Result<(), FrameError> {
    if got > max {
        return Err(FrameError::DeclaredTooLong { field, got, max });
    }
    Ok(())
}

fn endpoint(field: &'static str, raw: &[u8]) -> Result<EndpointId, FrameError> {
    let text = core::str::from_utf8(raw).map_err(|_| FrameError::Endpoint {
        field,
        error: IdError::IllegalByte {
            index: 0,
            byte: raw.first().copied().unwrap_or(0),
        },
    })?;
    EndpointId::parse(text).map_err(|error| FrameError::Endpoint { field, error })
}

/// A bounds-checked reader over the frame.
struct Cursor<'a> {
    buffer: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    const fn new(buffer: &'a [u8]) -> Self {
        Self { buffer, at: 0 }
    }

    fn remaining(&self) -> usize {
        self.buffer.len().saturating_sub(self.at)
    }

    fn take(&mut self, field: &'static str, len: usize) -> Result<&'a [u8], FrameError> {
        let available = self.remaining();
        if len > available {
            return Err(FrameError::Truncated {
                field,
                needed: len,
                available,
            });
        }
        let slice = &self.buffer[self.at..self.at + len];
        self.at += len;
        Ok(slice)
    }

    fn take_u8(&mut self, field: &'static str) -> Result<u8, FrameError> {
        Ok(self.take(field, 1)?[0])
    }

    fn take_array<const N: usize>(&mut self, field: &'static str) -> Result<[u8; N], FrameError> {
        let slice = self.take(field, N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(slice);
        Ok(out)
    }
}

/// The affirmative response.
///
/// Means the resolved endpoint's bounded local queue ACCEPTED the event.
/// It does not mean the human, Claude, or any application processed it,
/// and it is never evidence that a person read anything (ADR-0018).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedV2 {
    /// Echoes the request's ID. A sender must check it matches.
    pub message_id: MessageId,
    /// Which endpoint actually accepted it.
    ///
    /// Reported so a caller can show exact routing in diagnostics. For an
    /// explicit destination a sender must require this to equal what it
    /// asked for; for an omitted one this is how the default is learned.
    pub resolved_destination_endpoint: EndpointId,
}

/// The refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedV2 {
    /// Echoes the request's ID.
    pub message_id: MessageId,
    /// The coarse reason. Deliberately not a local diagnostic.
    pub reason: DirectRejectReason,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message_id(hex: &str) -> MessageId {
        MessageId::parse_hex(hex).expect("valid message id")
    }

    fn endpoint_id(name: &str) -> EndpointId {
        EndpointId::parse(name).expect("valid endpoint id")
    }

    fn frame(destination: Option<&str>, media: Option<&str>, payload: &[u8]) -> DirectMessageV2 {
        DirectMessageV2 {
            message_id: message_id("9f2c1d4e7a0b48c3915ed6f0a72b3c58"),
            sent_at_ms: 1_786_600_000_000,
            source_endpoint: endpoint_id("human"),
            destination_endpoint: destination.map(endpoint_id),
            payload: Payload::at_ceiling(
                media.map(|m| MediaType::parse(m).expect("valid media type")),
                payload.to_vec(),
            )
            .expect("within the ceiling"),
        }
    }

    #[test]
    fn a_frame_round_trips_through_its_own_encoding() {
        let original = frame(Some("claude"), Some("text/plain"), b"hello");
        let decoded = DirectMessageV2::decode(&original.encode(), MAX_PAYLOAD_BYTES)
            .expect("its own encoding decodes");
        assert_eq!(decoded, original);
    }

    /// The two zero-length fields mean different things, and neither is
    /// an empty string.
    #[test]
    fn an_omitted_destination_is_none_and_not_an_empty_label() {
        let decoded = DirectMessageV2::decode(
            &frame(None, Some("text/plain"), b"hi").encode(),
            MAX_PAYLOAD_BYTES,
        )
        .expect("decodes");
        assert_eq!(decoded.destination_endpoint, None);
    }

    #[test]
    fn an_absent_media_type_is_none_and_not_an_empty_string() {
        // The distinction the content fingerprint depends on: absence maps
        // to media_present = 0, and `Some("")` would hash differently if it
        // could exist at all.
        let decoded = DirectMessageV2::decode(
            &frame(Some("claude"), None, b"hi").encode(),
            MAX_PAYLOAD_BYTES,
        )
        .expect("decodes");
        assert_eq!(decoded.payload.media_type(), None);
    }

    #[test]
    fn an_empty_payload_still_carries_its_length() {
        let encoded = frame(Some("claude"), Some("text/plain"), b"").encode();
        let decoded =
            DirectMessageV2::decode(&encoded, MAX_PAYLOAD_BYTES).expect("an empty payload decodes");
        assert!(decoded.payload.is_empty());
        // The last four bytes are the u32be zero, present even so.
        assert_eq!(&encoded[encoded.len() - 4..], &[0, 0, 0, 0]);
    }

    #[test]
    fn a_source_endpoint_is_never_omitted() {
        let mut encoded = frame(Some("claude"), None, b"hi").encode();
        encoded[FIXED_PREFIX] = 0;
        assert_eq!(
            DirectMessageV2::decode(&encoded, MAX_PAYLOAD_BYTES),
            Err(FrameError::EmptySourceEndpoint)
        );
    }

    /// A declared length is refused BEFORE the bytes it describes are
    /// read. The buffer here is far shorter than the declared payload, so
    /// a decoder that allocated first would ask for 4 GiB.
    #[test]
    fn an_enormous_declared_payload_is_refused_at_the_length_field() {
        let mut encoded = frame(Some("claude"), None, b"hi").encode();
        let at = encoded.len() - 4 - 2;
        encoded[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        let error = DirectMessageV2::decode(&encoded, MAX_PAYLOAD_BYTES)
            .expect_err("an over-ceiling payload is refused");
        assert!(
            matches!(error, FrameError::PayloadTooLarge { .. }),
            "refused at the length, not the read: {error:?}"
        );
        assert_eq!(error.to_wire(), DirectRejectReason::TooLarge);
    }

    #[test]
    fn a_truncated_frame_is_refused_at_every_boundary() {
        let encoded = frame(Some("claude"), Some("text/plain"), b"hello").encode();
        for cut in 0..encoded.len() {
            assert!(
                DirectMessageV2::decode(&encoded[..cut], MAX_PAYLOAD_BYTES).is_err(),
                "a frame truncated to {cut} bytes must not decode"
            );
        }
        assert!(DirectMessageV2::decode(&encoded, MAX_PAYLOAD_BYTES).is_ok());
    }

    #[test]
    fn trailing_bytes_are_refused_rather_than_ignored() {
        let mut encoded = frame(Some("claude"), None, b"hi").encode();
        encoded.push(0);
        assert_eq!(
            DirectMessageV2::decode(&encoded, MAX_PAYLOAD_BYTES),
            Err(FrameError::TrailingBytes { extra: 1 })
        );
    }

    /// A profile may narrow the ceiling but the frozen limit still wins:
    /// asking for more than `MAX_PAYLOAD_BYTES` cannot widen the wire.
    #[test]
    fn an_effective_limit_narrows_but_never_widens() {
        let encoded = frame(Some("claude"), None, b"hello").encode();
        assert!(DirectMessageV2::decode(&encoded, 4).is_err(), "5 > 4");
        assert!(DirectMessageV2::decode(&encoded, 5).is_ok());
        assert!(DirectMessageV2::decode(&encoded, usize::MAX).is_ok());
    }

    #[test]
    fn every_frame_error_collapses_to_a_coarse_wire_code() {
        assert_eq!(
            FrameError::EmptySourceEndpoint.to_wire(),
            DirectRejectReason::Malformed
        );
        assert_eq!(
            FrameError::TrailingBytes { extra: 1 }.to_wire(),
            DirectRejectReason::Malformed
        );
        assert_eq!(
            FrameError::DeclaredTooLong {
                field: "media_type",
                got: 999,
                max: MAX_MEDIA_TYPE_BYTES
            }
            .to_wire(),
            DirectRejectReason::Malformed,
            "only the payload ceiling is `too_large`"
        );
    }
}
