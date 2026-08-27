// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The `BroadcastMessageV1` envelope carried in a GossipSub message.
//!
//! Hand-written against frozen bytes, exactly as [`crate::direct_v2`] is.
//! The layout is pinned by `architecture/transport/libp2p/PUBSUB.md` and
//! by five vectors in `fixtures/gossipsub/broadcast-message-v1-frame.json`.
//!
//! ```text
//! version:u8 || message_id:16 || sent_at_ms:u64be ||
//! media_type_len:u8 || media_type || payload_len:u32be || payload
//! ```
//!
//! Every multi-byte integer is **big-endian**, matching the direct frame,
//! the IPC length prefix, and `DirectContentFingerprintV1`.
//!
//! # What this frame does not carry, and why
//!
//! **No endpoint.** ADR-0030 keeps EndpointId out of broadcast, so two
//! local endpoints sharing one PeerId are intentionally indistinguishable
//! as transport-level broadcast originators.
//!
//! **No channel.** The receiver learns the channel from the GossipSub
//! topic the message arrived on, which it can always map back because it
//! only receives on topics it derived from a ChannelId it holds. Carrying
//! it here as well would let a publisher assert one channel while
//! publishing on another, with nothing to say which wins.
//!
//! # What it does carry that the direct frame does not
//!
//! A **version byte**, in band. Direct takes its version from the
//! negotiated protocol name `/interweave/direct/2.0.0`; a GossipSub topic
//! negotiates nothing, so this is the only place a reader can learn what
//! it is holding. A frame declaring any other version is refused here
//! rather than guessed at.

use crate::ids::MessageId;
use crate::payload::{MAX_MEDIA_TYPE_BYTES, MAX_PAYLOAD_BYTES, MediaType, Payload, PayloadError};

/// The only version this build writes or accepts.
pub const VERSION: u8 = 1;

/// Bytes of everything that is not payload, when every field is at its
/// maximum: `version:1 + message_id:16 + sent_at_ms:8 + media_type_len:1
/// + media_type:128 + payload_len:4`.
///
/// The backend's maximum transmit size is sized from this plus the
/// payload ceiling. A transmit ceiling larger than that would let a peer
/// send a frame the transport buffers and this decoder must then refuse;
/// sized exactly, an oversized frame is refused before it is buffered.
pub const MAX_FRAME_OVERHEAD: usize = 1 + 16 + 8 + 1 + MAX_MEDIA_TYPE_BYTES + 4;

/// Why an envelope did not decode.
///
/// Every variant is a **local** diagnostic and they all mean the same
/// thing to the mesh: objectively invalid protocol data, reported as
/// ADR-0029 `Reject`. There is deliberately no `to_wire` here — the
/// direct frame has one because a direct sender receives a coarse
/// rejection code, and a GossipSub publisher receives nothing at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BroadcastFrameError {
    /// The buffer ended before a declared field did.
    Truncated {
        /// What was being read when the bytes ran out.
        field: &'static str,
        /// Bytes the field declared it needed.
        needed: usize,
        /// Bytes actually left.
        available: usize,
    },
    /// The version byte named a frame shape this build does not know.
    ///
    /// Refused rather than skipped: a decoder that ignored the version
    /// would read a future layout as though it were this one.
    UnsupportedVersion {
        /// The version the frame declared.
        got: u8,
    },
    /// A declared length exceeded its ceiling, caught before the read.
    DeclaredTooLong {
        /// Which length field.
        field: &'static str,
        /// What it declared.
        got: usize,
        /// The ceiling it broke.
        max: usize,
    },
    /// The declared payload exceeded the effective ceiling.
    PayloadTooLarge {
        /// What the frame declared.
        got: usize,
        /// The effective limit.
        max: usize,
    },
    /// The media type was present but not a legal one.
    Media(PayloadError),
    /// Bytes remained after a complete frame.
    ///
    /// Refused rather than ignored: trailing bytes mean the sender and
    /// this decoder disagree about the layout, and guessing which of them
    /// is right is how one implementation silently accepts another's bug.
    TrailingBytes {
        /// How many were left over.
        extra: usize,
    },
}

impl core::fmt::Display for BroadcastFrameError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Truncated {
                field,
                needed,
                available,
            } => write!(
                f,
                "the frame ended inside `{field}`: it needed {needed} bytes and {available} remained"
            ),
            Self::UnsupportedVersion { got } => {
                write!(f, "envelope version {got} is not {VERSION}")
            }
            Self::DeclaredTooLong { field, got, max } => {
                write!(f, "`{field}` declared {got} bytes; the ceiling is {max}")
            }
            Self::PayloadTooLarge { got, max } => {
                write!(f, "the payload declared {got} bytes; the ceiling is {max}")
            }
            Self::Media(error) => write!(f, "the media type is invalid: {error}"),
            Self::TrailingBytes { extra } => {
                write!(f, "{extra} byte(s) remained after a complete frame")
            }
        }
    }
}

impl core::error::Error for BroadcastFrameError {}

/// One broadcast message, as it appears inside a GossipSub payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastMessageV1 {
    /// The sender's APPLICATION identity for this message.
    ///
    /// Never an input to mesh duplicate suppression: that is
    /// `GossipSubMessageIdV1` over the authenticated source PeerId and the
    /// wire sequence number (ADR-0004). Two publishers may legitimately
    /// choose the same 128 bits, and a mesh that collapsed them would drop
    /// a message nobody sent twice.
    pub message_id: MessageId,
    /// When the sender says it sent this.
    ///
    /// **Diagnostic only.** Not authorization, ordering, freshness, a
    /// replay window, or dedup input. No admission path may read it.
    pub sent_at_ms: u64,
    /// The application bytes and their advisory media type.
    pub payload: Payload,
}

impl BroadcastMessageV1 {
    /// Encode to the frozen layout.
    ///
    /// Total size is computable in advance, so the buffer is allocated
    /// once. Every length written here was validated when its component
    /// type was constructed, which is why this cannot fail.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let media = self
            .payload
            .media_type()
            .map_or(&[][..], |m| m.as_str().as_bytes());
        let bytes = self.payload.bytes();

        let mut out = Vec::with_capacity(1 + 16 + 8 + 1 + media.len() + 4 + bytes.len());
        out.push(VERSION);
        out.extend_from_slice(self.message_id.as_bytes());
        out.extend_from_slice(&self.sent_at_ms.to_be_bytes());

        // A u8 by construction: MediaType caps at 128, checked when it
        // was parsed.
        out.push(u8::try_from(media.len()).unwrap_or(u8::MAX));
        out.extend_from_slice(media);

        // u32be even when zero: an empty payload still carries its length,
        // which is what lets a decoder tell "no payload" from "frame
        // ended".
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
    /// Returns [`BroadcastFrameError`] naming the field that failed.
    pub fn decode(buffer: &[u8], limit: usize) -> Result<Self, BroadcastFrameError> {
        let mut cursor = Cursor::new(buffer);

        // FIRST, so a frame of another shape is refused before any of its
        // fields are interpreted as though they were this one's.
        let version = cursor.take_u8("version")?;
        if version != VERSION {
            return Err(BroadcastFrameError::UnsupportedVersion { got: version });
        }

        let message_id = MessageId::from_bytes(cursor.take_array::<16>("message_id")?);
        let sent_at_ms = u64::from_be_bytes(cursor.take_array::<8>("sent_at_ms")?);

        let media_type = {
            let len = usize::from(cursor.take_u8("media_type_len")?);
            if len == 0 {
                // ABSENCE, never `Some("")`. An empty media type does not
                // exist on this wire, and the two hash differently in the
                // content fingerprint the dedup cache stores.
                None
            } else {
                ceiling("media_type", len, MAX_MEDIA_TYPE_BYTES)?;
                let raw = cursor.take("media_type", len)?;
                let text = core::str::from_utf8(raw).map_err(|_| {
                    BroadcastFrameError::Media(PayloadError::MediaTypeNotAscii { index: 0 })
                })?;
                Some(MediaType::parse(text).map_err(BroadcastFrameError::Media)?)
            }
        };

        let payload = {
            let declared = u32::from_be_bytes(cursor.take_array::<4>("payload_len")?);
            let len = usize::try_from(declared).unwrap_or(usize::MAX);
            // BEFORE the read, so an enormous declared length costs a
            // comparison rather than an allocation.
            let max = limit.min(MAX_PAYLOAD_BYTES);
            if len > max {
                return Err(BroadcastFrameError::PayloadTooLarge { got: len, max });
            }
            let bytes = cursor.take("payload", len)?.to_vec();
            Payload::new(media_type, bytes, limit).map_err(BroadcastFrameError::Media)?
        };

        let extra = cursor.remaining();
        if extra > 0 {
            return Err(BroadcastFrameError::TrailingBytes { extra });
        }

        Ok(Self {
            message_id,
            sent_at_ms,
            payload,
        })
    }
}

/// Refuse a declared length above its ceiling, before it is acted on.
fn ceiling(field: &'static str, got: usize, max: usize) -> Result<(), BroadcastFrameError> {
    if got > max {
        return Err(BroadcastFrameError::DeclaredTooLong { field, got, max });
    }
    Ok(())
}

/// A bounds-checked reader over the frame.
///
/// Deliberately a private twin of [`crate::direct_v2`]'s rather than a
/// shared generic: the two differ only in the error type they build, and
/// threading a conversion trait through forty lines to spare twenty is a
/// worse trade than the duplication. If a third frame appears, share it.
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

    fn take(&mut self, field: &'static str, len: usize) -> Result<&'a [u8], BroadcastFrameError> {
        let available = self.remaining();
        if len > available {
            return Err(BroadcastFrameError::Truncated {
                field,
                needed: len,
                available,
            });
        }
        let slice = &self.buffer[self.at..self.at + len];
        self.at += len;
        Ok(slice)
    }

    fn take_u8(&mut self, field: &'static str) -> Result<u8, BroadcastFrameError> {
        Ok(self.take(field, 1)?[0])
    }

    fn take_array<const N: usize>(
        &mut self,
        field: &'static str,
    ) -> Result<[u8; N], BroadcastFrameError> {
        let slice = self.take(field, N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(slice);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MID: &str = "3ac9f1027b6e45d8ba10c4e93f5d7a26";

    fn frame(media: Option<&str>, payload: &[u8]) -> BroadcastMessageV1 {
        BroadcastMessageV1 {
            message_id: MessageId::parse_hex(MID).expect("valid message id"),
            sent_at_ms: 1_786_600_000_000,
            payload: Payload::at_ceiling(
                media.map(|m| MediaType::parse(m).expect("valid media type")),
                payload.to_vec(),
            )
            .expect("within the ceiling"),
        }
    }

    #[test]
    fn a_frame_round_trips_through_encode_and_decode() {
        for original in [
            frame(None, b"hello"),
            frame(Some("text/plain"), b"hello"),
            frame(Some("application/octet-stream"), b""),
            frame(Some(&"a".repeat(MAX_MEDIA_TYPE_BYTES)), b"\x00"),
        ] {
            let bytes = original.encode();
            let decoded = BroadcastMessageV1::decode(&bytes, MAX_PAYLOAD_BYTES)
                .expect("a frame this build wrote decodes");
            assert_eq!(decoded, original);
        }
    }

    #[test]
    fn media_type_len_zero_is_absent_never_empty() {
        // The two are different messages with different content
        // fingerprints, so a decoder that turned absence into `Some("")`
        // would give them one identity in the dedup cache.
        let bytes = frame(None, b"hello").encode();
        let decoded = BroadcastMessageV1::decode(&bytes, MAX_PAYLOAD_BYTES).expect("decodes");
        assert!(
            decoded.payload.media_type().is_none(),
            "a zero length is absence, not an empty media type"
        );
        assert_eq!(bytes[25], 0, "the media_type_len byte itself is zero");
    }

    #[test]
    fn an_unknown_version_is_refused_rather_than_read_as_this_one() {
        // Every later field would decode "successfully" as garbage if the
        // version byte were skipped, which is the failure this catches.
        let mut bytes = frame(Some("text/plain"), b"hello").encode();
        bytes[0] = 2;
        assert_eq!(
            BroadcastMessageV1::decode(&bytes, MAX_PAYLOAD_BYTES),
            Err(BroadcastFrameError::UnsupportedVersion { got: 2 })
        );
    }

    #[test]
    fn a_truncated_frame_names_the_field_the_bytes_ran_out_in() {
        let bytes = frame(Some("text/plain"), b"hello").encode();
        for cut in [0, 1, 10, 25, 26, bytes.len() - 1] {
            let error = BroadcastMessageV1::decode(&bytes[..cut], MAX_PAYLOAD_BYTES)
                .expect_err("a short buffer cannot decode");
            assert!(
                matches!(error, BroadcastFrameError::Truncated { .. }),
                "cut at {cut} gave {error:?}"
            );
        }
    }

    #[test]
    fn a_payload_past_the_effective_limit_is_refused_at_the_length_field() {
        // The declared length is a lie: only five bytes follow. A decoder
        // that allocated before comparing would try for 4 GiB.
        let mut bytes = frame(Some("text/plain"), b"hello").encode();
        let tail = bytes.len() - 5 - 4;
        bytes[tail..tail + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(
            BroadcastMessageV1::decode(&bytes, MAX_PAYLOAD_BYTES),
            Err(BroadcastFrameError::PayloadTooLarge {
                got: u32::MAX as usize,
                max: MAX_PAYLOAD_BYTES
            })
        );
    }

    #[test]
    fn a_profile_limit_narrows_the_ceiling_but_cannot_widen_it() {
        let bytes = frame(None, b"hello").encode();
        assert!(
            matches!(
                BroadcastMessageV1::decode(&bytes, 4),
                Err(BroadcastFrameError::PayloadTooLarge { got: 5, max: 4 })
            ),
            "a narrower profile limit binds"
        );
        // And a limit above the frozen ceiling is clamped to it, so a
        // configuration cannot buy itself a wider wire.
        let over =
            BroadcastMessageV1::decode(&bytes, MAX_PAYLOAD_BYTES * 4).expect("still decodes");
        assert_eq!(over.payload.len(), 5);
    }

    #[test]
    fn trailing_bytes_are_refused_rather_than_ignored() {
        let mut bytes = frame(None, b"hello").encode();
        bytes.push(0);
        assert_eq!(
            BroadcastMessageV1::decode(&bytes, MAX_PAYLOAD_BYTES),
            Err(BroadcastFrameError::TrailingBytes { extra: 1 })
        );
    }

    #[test]
    fn the_frame_overhead_constant_is_the_real_maximum() {
        // `MAX_FRAME_OVERHEAD` sizes the backend's transmit ceiling. If it
        // understated the true overhead, a legal maximum-size message
        // would be refused by the transport before this decoder ever saw
        // it — a failure that looks like a network fault.
        let widest = frame(Some(&"a".repeat(MAX_MEDIA_TYPE_BYTES)), b"").encode();
        assert_eq!(widest.len(), MAX_FRAME_OVERHEAD);
    }
}
