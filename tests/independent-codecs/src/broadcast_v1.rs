// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! `BroadcastMessageV1`, from `PUBSUB.md` §Envelope:
//!
//! ```text
//! version:u8 || message_id:16 || sent_at_ms:u64be
//!   || media_type_len:u8 || media_type || payload_len:u32be || payload
//! ```
//!
//! The version is in band because a topic negotiates nothing; any value
//! but 1 is a frame this codec cannot read.

use crate::wire::{self, Reader};
use crate::{DecodeError, MAX_PAYLOAD_BYTES, is_media_type};

/// The one envelope version that exists.
pub const VERSION: u8 = 1;

/// One envelope. No endpoint and no channel: ADR-0030 keeps `EndpointId` out
/// of broadcast, and the topic names the channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastMessageV1 {
    /// The application identity, never the mesh duplicate key.
    pub message_id: [u8; 16],
    /// Diagnostic only.
    pub sent_at_ms: u64,
    /// `None` is length zero.
    pub media_type: Option<String>,
    /// At most [`MAX_PAYLOAD_BYTES`].
    pub payload: Vec<u8>,
}

impl BroadcastMessageV1 {
    /// Decode one envelope, the whole GossipSub data field.
    ///
    /// # Errors
    /// A version other than [`VERSION`], or any field outside its bound.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(bytes);
        let version = r.u8("version")?;
        if version != VERSION {
            return Err(DecodeError(format!(
                "unsupported envelope version {version}"
            )));
        }
        let message_id = r.id16()?;
        let sent_at_ms = r.u64be("sent_at_ms")?;
        let media_type = wire::media_type(&mut r)?;
        let payload = wire::payload(&mut r)?;
        r.finish()?;
        Ok(Self {
            message_id,
            sent_at_ms,
            media_type,
            payload,
        })
    }

    /// Encode at [`VERSION`].
    ///
    /// # Errors
    /// An empty or over-long media type, or a payload above the ceiling.
    pub fn encode(&self) -> Result<Vec<u8>, DecodeError> {
        if let Some(m) = &self.media_type
            && !is_media_type(m)
        {
            return Err(DecodeError(
                "media_type not 1..128 printable ASCII bytes".to_owned(),
            ));
        }
        if self.payload.len() > MAX_PAYLOAD_BYTES {
            return Err(DecodeError("payload above the ceiling".to_owned()));
        }
        let mut out = Vec::with_capacity(1 + 16 + 8 + 1 + 128 + 4 + self.payload.len());
        out.push(VERSION);
        out.extend_from_slice(&self.message_id);
        out.extend_from_slice(&self.sent_at_ms.to_be_bytes());
        wire::put_media_type(&mut out, self.media_type.as_deref());
        wire::put_payload(&mut out, &self.payload);
        Ok(out)
    }
}
