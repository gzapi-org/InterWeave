// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Neutral transport contract types for InterWeave.
//!
//! This crate is the boundary every higher layer consumes and no backend
//! may cross. It contains identifiers, payloads, capabilities, status, and
//! the error vocabulary — and deliberately **no** libp2p, UI, platform, or
//! storage types (ADR-0021, ADR-0045). Translating a backend concept into
//! these types is the backend's job; letting one leak upward is what makes
//! a "replaceable" backend unreplaceable.
//!
//! Two properties are load-bearing throughout:
//!
//! - **Identifiers are parsed, never merely held.** Every newtype validates
//!   at construction and deserializes through the same parser, so a value
//!   that exists satisfies its grammar. JSON arriving over IPC is untrusted
//!   input, and a derived `Deserialize` would build values the grammar
//!   never admitted.
//! - **Absence is distinct from emptiness.** An absent media type is not an
//!   empty one, and an omitted destination endpoint is the receiver's
//!   configured default rather than fan-out. Both distinctions are encoded
//!   in the types because both are observable on the wire.
//!
//! Nothing here performs I/O, and no type in this crate implies that a
//! message was delivered, stored, or read.

#![forbid(unsafe_code)]

pub mod ids;
pub mod payload;
pub mod status;

pub use ids::{ChannelId, DirectDestination, EndpointId, IdError, MessageId, TransportIdentity};
pub use payload::{
    MediaType, Payload, PayloadError, MAX_MEDIA_TYPE_BYTES, MAX_PAYLOAD_BYTES,
};
pub use status::{
    ConnectivitySummary, Health, PathReadiness, PreferredPathPolicy, TransportCapabilities,
    TransportError,
};
