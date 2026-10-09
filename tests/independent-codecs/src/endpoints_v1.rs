// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! `/interweave/endpoints/1.0.0`, from `transport/libp2p/ENDPOINTS.md`
//! §Endpoint directory protocol:
//!
//! ```text
//! ListEndpointsV1:     tag:u8 = 0x01                         (exactly one byte)
//! EndpointDirectoryV1: tag:u8 = 0x01 || generated_at_ms:u64be || ttl_ms:u32be
//!                      || count:u8 (0..=32) || count × (len:u8 (1..=64) || endpoint)
//! DirectoryRefusedV1:  tag:u8 = 0x02 || reason:u8   (1 overloaded, 2 unauthorized, 3 unavailable)
//! ```
//!
//! More than 32 entries, an endpoint outside the grammar, or a duplicate is a
//! protocol violation, never something to truncate. `ttl_ms` is carried as
//! sent, because clamping is the receiver's cache rule and not the codec's.
//! An unsorted unique list decodes as sent: sorting it is also the
//! receiver's. A refusal reason outside 1..=3 is refused. The text lists
//! three hand-assigned reasons and names no fallback for any other, so this
//! codec reads "the reasons are these three" as closed, as `DIRECT.md` does
//! for its own codes.

use crate::wire::Reader;
use crate::{DecodeError, is_endpoint_id};

/// The protocol id.
pub const PROTOCOL: &str = "/interweave/endpoints/1.0.0";

/// The one request, exactly one byte.
pub const REQUEST: [u8; 1] = [0x01];

/// At most this many entries.
pub const MAX_ENTRIES: usize = 32;

/// The largest response: 1 + 8 + 4 + 1 + 32 × 65.
pub const MAX_RESPONSE_BYTES: usize = 2094;

/// The refusal reasons, hand-assigned: code = index + 1.
pub const REASONS: [&str; 3] = ["overloaded", "unauthorized", "unavailable"];

/// One response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectoryResponseV1 {
    /// The list.
    Directory {
        /// Diagnostic only; never extends freshness.
        generated_at_ms: u64,
        /// As sent; the receiver clamps it.
        ttl_ms: u32,
        /// 0..=32 unique endpoints, in the order sent.
        endpoints: Vec<String>,
    },
    /// A refusal, which carries no list.
    Refused {
        /// One of [`REASONS`].
        reason: &'static str,
    },
}

/// Decode a request.
///
/// # Errors
/// Anything but the single byte `0x01`, an empty stream included.
pub fn decode_request(bytes: &[u8]) -> Result<(), DecodeError> {
    if bytes == REQUEST {
        Ok(())
    } else {
        Err(DecodeError(format!(
            "a request of {} byte(s) that is not 0x01",
            bytes.len()
        )))
    }
}

impl DirectoryResponseV1 {
    /// Decode one response, the whole substream.
    ///
    /// # Errors
    /// A frame over [`MAX_RESPONSE_BYTES`], an unknown tag, a count over 32,
    /// an entry outside the grammar, a duplicate, an unassigned reason, a
    /// short field or a trailing byte.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(DecodeError(format!(
                "a response of {} bytes, above {MAX_RESPONSE_BYTES}",
                bytes.len()
            )));
        }
        let mut r = Reader::new(bytes);
        let response = match r.u8("tag")? {
            0x01 => {
                let generated_at_ms = r.u64be("generated_at_ms")?;
                let ttl_ms = r.u32be("ttl_ms")?;
                let count = usize::from(r.u8("count")?);
                if count > MAX_ENTRIES {
                    return Err(DecodeError(format!("{count} entries, above {MAX_ENTRIES}")));
                }
                let mut endpoints: Vec<String> = Vec::with_capacity(count);
                for _ in 0..count {
                    let len = usize::from(r.u8("len")?);
                    if len == 0 {
                        return Err(DecodeError("an entry of length 0".to_owned()));
                    }
                    let label = core::str::from_utf8(r.take(len, "endpoint")?)
                        .ok()
                        .filter(|t| is_endpoint_id(t))
                        .ok_or_else(|| {
                            DecodeError("an entry outside the EndpointId grammar".to_owned())
                        })?;
                    if endpoints.iter().any(|e| e == label) {
                        return Err(DecodeError(format!("a duplicate entry {label:?}")));
                    }
                    endpoints.push(label.to_owned());
                }
                Self::Directory {
                    generated_at_ms,
                    ttl_ms,
                    endpoints,
                }
            }
            0x02 => {
                let code = r.u8("reason")?;
                let reason = usize::from(code)
                    .checked_sub(1)
                    .and_then(|i| REASONS.get(i))
                    .ok_or_else(|| DecodeError(format!("an unassigned refusal reason {code}")))?;
                Self::Refused { reason }
            }
            other => return Err(DecodeError(format!("an unknown response tag {other}"))),
        };
        r.finish()?;
        Ok(response)
    }

    /// Encode.
    ///
    /// # Errors
    /// More than 32 entries, an entry outside the grammar, a duplicate, or
    /// a reason not in [`REASONS`].
    pub fn encode(&self) -> Result<Vec<u8>, DecodeError> {
        let mut out = Vec::with_capacity(MAX_RESPONSE_BYTES);
        match self {
            Self::Directory {
                generated_at_ms,
                ttl_ms,
                endpoints,
            } => {
                if endpoints.len() > MAX_ENTRIES {
                    return Err(DecodeError("more than 32 entries".to_owned()));
                }
                out.push(0x01);
                out.extend_from_slice(&generated_at_ms.to_be_bytes());
                out.extend_from_slice(&ttl_ms.to_be_bytes());
                out.push(u8::try_from(endpoints.len()).unwrap_or(u8::MAX));
                for (n, e) in endpoints.iter().enumerate() {
                    if !is_endpoint_id(e) || endpoints[..n].contains(e) {
                        return Err(DecodeError(format!("entry {e:?} illegal or repeated")));
                    }
                    out.push(u8::try_from(e.len()).unwrap_or(u8::MAX));
                    out.extend_from_slice(e.as_bytes());
                }
            }
            Self::Refused { reason } => {
                let index = REASONS
                    .iter()
                    .position(|r| r == reason)
                    .ok_or_else(|| DecodeError(format!("{reason:?} is not a refusal reason")))?;
                out.push(0x02);
                out.push(u8::try_from(index + 1).unwrap_or(u8::MAX));
            }
        }
        Ok(out)
    }
}
