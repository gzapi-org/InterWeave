// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Shared fixtures for the ADR-0044 conformance suite.
//!
//! The library exists so the test binaries and the `crash_writer` bin
//! build the same rows. A crash test that wrote different content from
//! the one that reads it back would prove nothing.

use interweave_human_store::{
    AppMessageId, InboundOrigin, NewInbound, NewOutbound, OutboundDestination,
};
use interweave_transport_api::{DirectDestination, MediaType, TransportIdentity};

/// A canonical test PeerId. Test-only; no private key exists for it.
pub const PEER: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

/// The `app_message_id` of the pending-outbound row the crash writer commits.
pub const OUTBOUND_ID: &str = "0123456789abcdef0123456789abcdef";
/// The `app_message_id` of the unread-inbound row the crash writer commits.
pub const INBOUND_ID: &str = "fedcba9876543210fedcba9876543210";

/// The body of the pending-outbound row.
pub const OUTBOUND_BODY: &[u8] = b"a message the human believes they sent";
/// The body of the unread-inbound row.
pub const INBOUND_BODY: &[u8] = b"a message the human was told about";

/// The test peer identity.
///
/// # Panics
/// If the constant above stops being a canonical PeerId.
#[must_use]
pub fn peer() -> TransportIdentity {
    #[allow(clippy::expect_used)]
    TransportIdentity::parse(PEER).expect("the test peer id is canonical")
}

/// The pending-outbound row every case in this suite starts from.
///
/// # Panics
/// If [`OUTBOUND_ID`] stops matching the HumanChatV2 id grammar.
#[must_use]
pub fn pending_outbound() -> NewOutbound {
    #[allow(clippy::expect_used)]
    NewOutbound {
        app_message_id: AppMessageId::parse(OUTBOUND_ID).expect("canonical id"),
        destination: OutboundDestination::Direct(DirectDestination::to_default(peer())),
        media_type: Some(
            MediaType::parse("application/vnd.interweave-human-chat+json;v=2")
                .expect("a valid test media type"),
        ),
        payload: OUTBOUND_BODY.to_vec(),
        created_at: 1_700_000_000_000,
    }
}

/// The unread-inbound row every case in this suite starts from.
///
/// # Panics
/// If [`INBOUND_ID`] stops matching the HumanChatV2 id grammar.
#[must_use]
pub fn unread_inbound() -> NewInbound {
    #[allow(clippy::expect_used)]
    NewInbound {
        app_message_id: AppMessageId::parse(INBOUND_ID).expect("canonical id"),
        origin: InboundOrigin {
            peer: peer(),
            endpoint: None,
            channel: None,
        },
        media_type: Some(
            MediaType::parse("application/vnd.interweave-human-chat+json;v=2")
                .expect("a valid test media type"),
        ),
        payload: INBOUND_BODY.to_vec(),
        received_at: 1_700_000_001_000,
    }
}
