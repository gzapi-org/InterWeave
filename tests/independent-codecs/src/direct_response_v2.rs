// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! `AcceptedV2` / `RejectedV2`, from `DIRECT.md` §Response byte layout:
//!
//! ```text
//! AcceptedV2: tag:u8 = 1 || message_id:16 || resolved_endpoint_len:u8 || resolved_destination_endpoint
//! RejectedV2: tag:u8 = 2 || message_id:16 || reason:u8
//! ```
//!
//! The reason numbers `schemas/direct/reject-reason`'s enum in order from
//! 1, and the order is written out below as that schema lists it. Any other
//! tag, an unassigned code, a short field or a trailing byte is malformed
//! response metadata.

use crate::wire::Reader;
use crate::{DecodeError, is_endpoint_id};

/// The coarse reasons, in the schema's enum order: code = index + 1.
pub const REASONS: [&str; 7] = [
    "no_route",
    "unauthorized_peer",
    "overloaded",
    "malformed",
    "too_large",
    "shutting_down",
    "unsupported",
];

/// The longest legal response: 1 + 16 + 1 + 64.
pub const MAX_RESPONSE_BYTES: usize = 82;

const TAG_ACCEPTED: u8 = 1;
const TAG_REJECTED: u8 = 2;

/// One response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectResponseV2 {
    /// The resolved endpoint's queue took it.
    Accepted {
        /// The request's id, echoed.
        message_id: [u8; 16],
        /// 1..64 bytes of `EndpointId` grammar, never empty.
        resolved_destination_endpoint: String,
    },
    /// Refused, coarsely.
    Rejected {
        /// The request's id, echoed.
        message_id: [u8; 16],
        /// One of [`REASONS`].
        reason: &'static str,
    },
}

impl DirectResponseV2 {
    /// Decode one response, the whole substream.
    ///
    /// # Errors
    /// An unknown tag, an empty or ungrammatical label, an unassigned
    /// reason, a short field or a byte after the last field.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(bytes);
        let tag = r.u8("tag")?;
        let message_id = r.id16()?;
        let response = match tag {
            TAG_ACCEPTED => {
                let len = usize::from(r.u8("resolved_endpoint_len")?);
                if len == 0 {
                    return Err(DecodeError(
                        "resolved_endpoint_len 0: an acceptance names its endpoint".to_owned(),
                    ));
                }
                let label = r.take(len, "resolved_destination_endpoint")?;
                let text = core::str::from_utf8(label)
                    .ok()
                    .filter(|t| is_endpoint_id(t))
                    .ok_or_else(|| {
                        DecodeError("resolved endpoint outside the EndpointId grammar".to_owned())
                    })?;
                Self::Accepted {
                    message_id,
                    resolved_destination_endpoint: text.to_owned(),
                }
            }
            TAG_REJECTED => {
                let code = r.u8("reason")?;
                let reason = usize::from(code)
                    .checked_sub(1)
                    .and_then(|i| REASONS.get(i))
                    .ok_or_else(|| DecodeError(format!("an unassigned reason code {code}")))?;
                Self::Rejected { message_id, reason }
            }
            other => return Err(DecodeError(format!("an unknown response tag {other}"))),
        };
        r.finish()?;
        Ok(response)
    }

    /// Encode.
    ///
    /// # Errors
    /// A label outside the grammar, or a reason not in [`REASONS`].
    pub fn encode(&self) -> Result<Vec<u8>, DecodeError> {
        let mut out = Vec::with_capacity(MAX_RESPONSE_BYTES);
        match self {
            Self::Accepted {
                message_id,
                resolved_destination_endpoint,
            } => {
                if !is_endpoint_id(resolved_destination_endpoint) {
                    return Err(DecodeError(
                        "resolved endpoint outside the grammar".to_owned(),
                    ));
                }
                out.push(TAG_ACCEPTED);
                out.extend_from_slice(message_id);
                out.push(u8::try_from(resolved_destination_endpoint.len()).unwrap_or(u8::MAX));
                out.extend_from_slice(resolved_destination_endpoint.as_bytes());
            }
            Self::Rejected { message_id, reason } => {
                let index = REASONS
                    .iter()
                    .position(|r| r == reason)
                    .ok_or_else(|| DecodeError(format!("{reason:?} is not a coarse reason")))?;
                out.push(TAG_REJECTED);
                out.extend_from_slice(message_id);
                out.push(u8::try_from(index + 1).unwrap_or(u8::MAX));
            }
        }
        Ok(out)
    }
}
