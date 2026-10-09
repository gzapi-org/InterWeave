// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! `DirectMessageV2`, from `DIRECT.md` §Request:
//!
//! ```text
//! message_id:16 || sent_at_ms:u64be || source_endpoint_len:u8 || source_endpoint
//!   || destination_endpoint_len:u8 || destination_endpoint
//!   || media_type_len:u8 || media_type || payload_len:u32be || payload
//! ```
//!
//! No version byte: the version is the negotiated protocol id
//! `/interweave/direct/2.0.0`.

use crate::wire::{self, Reader};
use crate::{DecodeError, MAX_PAYLOAD_BYTES, is_endpoint_id, is_media_type};

/// The protocol id the frame travels under.
pub const PROTOCOL: &str = "/interweave/direct/2.0.0";

/// One request frame, every field as the contract names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectMessageV2 {
    /// Exactly 128 bits.
    pub message_id: [u8; 16],
    /// Diagnostic only.
    pub sent_at_ms: u64,
    /// 1..64 bytes of `EndpointId` grammar: a source is always present.
    pub source_endpoint: String,
    /// `None` is length zero: the receiver's configured default, never
    /// fan-out.
    pub destination_endpoint: Option<String>,
    /// `None` is length zero: absence, never an empty string.
    pub media_type: Option<String>,
    /// At most [`MAX_PAYLOAD_BYTES`].
    pub payload: Vec<u8>,
}

impl DirectMessageV2 {
    /// Decode one frame, the whole substream.
    ///
    /// # Errors
    /// A field outside its contract bound, a declared length past the end,
    /// or a byte after the payload.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(bytes);
        let message_id = r.id16()?;
        let sent_at_ms = r.u64be("sent_at_ms")?;

        let source_len = usize::from(r.u8("source_endpoint_len")?);
        if source_len == 0 {
            return Err(DecodeError(
                "source_endpoint_len 0: a source is always present".to_owned(),
            ));
        }
        let source_endpoint = endpoint(r.take(source_len, "source_endpoint")?, "source_endpoint")?;

        let dest_len = usize::from(r.u8("destination_endpoint_len")?);
        let destination_endpoint = if dest_len == 0 {
            None
        } else {
            Some(endpoint(
                r.take(dest_len, "destination_endpoint")?,
                "destination_endpoint",
            )?)
        };

        let media_type = wire::media_type(&mut r)?;
        let payload = wire::payload(&mut r)?;
        r.finish()?;
        Ok(Self {
            message_id,
            sent_at_ms,
            source_endpoint,
            destination_endpoint,
            media_type,
            payload,
        })
    }

    /// Encode, refusing what the contract says no frame may carry.
    ///
    /// # Errors
    /// An endpoint outside the grammar, an empty or over-long media type,
    /// or a payload above [`MAX_PAYLOAD_BYTES`].
    pub fn encode(&self) -> Result<Vec<u8>, DecodeError> {
        if !is_endpoint_id(&self.source_endpoint) {
            return Err(DecodeError(
                "source_endpoint outside the grammar".to_owned(),
            ));
        }
        if let Some(d) = &self.destination_endpoint
            && !is_endpoint_id(d)
        {
            return Err(DecodeError(
                "destination_endpoint outside the grammar".to_owned(),
            ));
        }
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
        let mut out = Vec::with_capacity(16 + 8 + 3 + 4 + 64 * 2 + 128 + self.payload.len());
        out.extend_from_slice(&self.message_id);
        out.extend_from_slice(&self.sent_at_ms.to_be_bytes());
        put_label(&mut out, Some(&self.source_endpoint));
        put_label(&mut out, self.destination_endpoint.as_deref());
        wire::put_media_type(&mut out, self.media_type.as_deref());
        wire::put_payload(&mut out, &self.payload);
        Ok(out)
    }
}

fn endpoint(bytes: &[u8], what: &str) -> Result<String, DecodeError> {
    let text =
        core::str::from_utf8(bytes).map_err(|_| DecodeError(format!("{what} is not ASCII")))?;
    if !is_endpoint_id(text) {
        return Err(DecodeError(format!(
            "{what} {text:?} outside the EndpointId grammar"
        )));
    }
    Ok(text.to_owned())
}

fn put_label(out: &mut Vec<u8>, label: Option<&str>) {
    match label {
        None => out.push(0),
        Some(l) => {
            out.push(u8::try_from(l.len()).unwrap_or(u8::MAX));
            out.extend_from_slice(l.as_bytes());
        }
    }
}
