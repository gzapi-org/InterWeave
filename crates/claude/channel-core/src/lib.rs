// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Claude Code Channel bridge's pure half (plan §19 step 2): what a
//! session's events become on the channel, the reply-token table that
//! routes a reply back, and the tool surface's types and result wording.
//!
//! No I/O: `apps/claude-channel` is the MCP server and the IPC client's
//! caller. Nothing here names a libp2p type or a `crates/transport/*`
//! crate (§19 Rule; the layering guard), so the bridge sees the transport
//! only as `local-client-api` and `transport-api` describe it.

#![forbid(unsafe_code)]

pub mod content;
pub mod meta;
pub mod received_at;
pub mod reply_token;

pub use content::{ChannelContent, PayloadEncoding, Undecodable, channel_content};
pub use meta::{ChannelMeta, MAX_META_VALUE_BYTES, MetaError, MetaKey};
pub use received_at::{MAX_RECEIVED_AT_MS, rfc3339_utc};
pub use reply_token::{DuplicateToken, ReplyResolution, ReplyRoute, ReplyTokenTable};
