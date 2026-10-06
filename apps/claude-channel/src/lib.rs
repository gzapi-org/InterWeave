// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Claude Code Channel bridge (plan §19 step 3): a stdio MCP server
//! that turns a data session's inbound messages into
//! `notifications/claude/channel` and the seven tools into session calls.
//! `channel-core` holds the rules; this crate is the composition root.

#![forbid(unsafe_code)]

pub mod mcp;
