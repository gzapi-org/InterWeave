// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! HumanChatV2: the envelope, the media type, and the bounded decode path.
//!
//! Above transport and independent of it (ADR-0050). The same library
//! serves the desktop client, the Android client, and the Claude bridge,
//! which is what makes "decompression happens once, above transport"
//! true rather than aspirational.
//!
//! The markdown SUBSET is not enforced here. An out-of-subset construct
//! falls back to plain-text display rather than rejecting the message, so
//! subset conformance is a rendering contract and belongs with whatever
//! pins a CommonMark parser. What this crate does provide is the policy
//! primitives that rendering needs — the allowlisted link schemes and the
//! bounds — so every consumer applies one rule rather than its own
//! reading of the contract.

#![forbid(unsafe_code)]

pub mod decode;
pub mod envelope;

pub use decode::{
    CONTENT_ENCODING_BROTLI, ContentEncoding, DecodeError, MAX_DECOMPRESSED_BYTES, MEDIA_TYPE_V2,
    MediaTypeInfo, decode_envelope_bytes, parse_media_type, sender_may_compress,
};
pub use envelope::{
    ALLOWED_LINK_SCHEMES, COMMONMARK_VERSION, EnvelopeError, GFM_VERSION, HumanChatV2,
    MAX_BLOCK_NESTING, MAX_SENT_AT_MS, MAX_TABLE_COLUMNS, MAX_TABLE_ROWS, MessageKind,
    is_allowed_link_scheme,
};
