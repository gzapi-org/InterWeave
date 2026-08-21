// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! What the three retention tables hold.
//!
//! The types here are the store's vocabulary, and two of them exist to
//! make a rule unstateable rather than merely checked:
//!
//! - [`ReadEphemeral`] is the only route into `kept_inbound`, and the
//!   only way to obtain one is to call `mark_read` locally. There is no
//!   `keep(content)` entry point for remote data to reach.
//! - [`OutboundDestination`] distinguishes direct from broadcast in the
//!   type, because their transport-terminal events differ and a UI that
//!   confused them would claim a delivery nobody made.

use interweave_transport_api::{
    ChannelId, DirectDestination, EndpointId, MediaType, TransportIdentity,
};

use crate::StoreError;

/// A row's local identity within one table.
///
/// Local and non-portable. It is not the `app_message_id`, is never sent
/// anywhere, and does not survive the row: a message that is read and
/// later kept gets a new one, because it is genuinely a new row in a
/// different table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowId(i64);

impl RowId {
    /// The underlying value, for logging and test assertions.
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }

    pub(crate) const fn new(value: i64) -> Self {
        Self(value)
    }
}

/// A HumanChatV2 `app_message_id`: 32 lowercase hex characters.
///
/// Validated on construction so a malformed id cannot reach a UNIQUE
/// column and turn into a constraint error at commit time — by which
/// point the store would already have decided it was healthy.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AppMessageId(String);

impl AppMessageId {
    /// Validate and wrap an application message id.
    ///
    /// # Errors
    /// Returns [`StoreError::MalformedAppMessageId`] for anything that is
    /// not exactly 32 lowercase hex characters — the grammar HumanChatV2
    /// states, restated here because the store must not depend on the
    /// envelope parser to hold its own columns valid.
    pub fn parse(value: impl Into<String>) -> Result<Self, StoreError> {
        let value = value.into();
        let canonical = value.len() == 32
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if canonical {
            Ok(Self(value))
        } else {
            Err(StoreError::MalformedAppMessageId { got: value })
        }
    }

    /// The id as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where an outbound message is going.
///
/// Two variants, not one destination string, because the retention
/// contract's terminal event is different for each: direct ends at
/// `AcceptedV2` from one endpoint, broadcast ends at local publication
/// with no recipient acknowledgement at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboundDestination {
    /// Exactly one remote endpoint, or the peer's configured default.
    Direct(DirectDestination),
    /// A broadcast channel. Publication is not delivery.
    Broadcast(ChannelId),
}

/// Where an inbound message came from.
///
/// `endpoint` is peer-asserted metadata and `channel` is set for
/// broadcast. Neither is authorization, and nothing in this crate reads
/// them for a decision — they exist so a UI can group a conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundOrigin {
    /// The authenticated publisher or sender.
    pub peer: TransportIdentity,
    /// The source endpoint the sender asserted, if any.
    pub endpoint: Option<EndpointId>,
    /// The broadcast channel, if this arrived over one.
    pub channel: Option<ChannelId>,
}

/// The content and metadata of a message being sent.
#[derive(Clone, PartialEq, Eq)]
pub struct NewOutbound {
    /// The HumanChatV2 application id. A retry reuses it.
    pub app_message_id: AppMessageId,
    /// Where it is going.
    pub destination: OutboundDestination,
    /// The media type of `payload`, if the application set one.
    pub media_type: Option<MediaType>,
    /// The exact wire bytes to send.
    ///
    /// Stored as sent, so a retry resends byte-identical content and its
    /// `DirectContentFingerprintV1` is unchanged (ADR-0050). Re-encoding
    /// on retry would move the fingerprint and defeat dedup.
    pub payload: Vec<u8>,
    /// Local millisecond timestamp of composition.
    pub created_at: u64,
}

/// A durable pending-outbound row.
#[derive(Clone, PartialEq, Eq)]
pub struct PendingOutbound {
    /// This row's local identity.
    pub row_id: RowId,
    /// The application id, reused verbatim by every retry.
    pub app_message_id: AppMessageId,
    /// Where it is going.
    pub destination: OutboundDestination,
    /// The media type of `payload`, if any.
    pub media_type: Option<MediaType>,
    /// The exact bytes to resend.
    pub payload: Vec<u8>,
    /// When it was composed.
    pub created_at: u64,
    /// When the last send attempt was made, if there was one.
    pub last_attempt_at: Option<u64>,
    /// How many attempts have been made.
    pub attempts: u32,
}

/// The content and metadata of a message just received.
#[derive(Clone, PartialEq, Eq)]
pub struct NewInbound {
    /// The HumanChatV2 application id.
    pub app_message_id: AppMessageId,
    /// Who sent it and over what.
    pub origin: InboundOrigin,
    /// The media type of `payload`, if the sender set one.
    pub media_type: Option<MediaType>,
    /// The received bytes.
    pub payload: Vec<u8>,
    /// Local millisecond timestamp of receipt.
    pub received_at: u64,
}

/// A durable inbound row, unread or kept.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredInbound {
    /// This row's local identity.
    pub row_id: RowId,
    /// The application id.
    pub app_message_id: AppMessageId,
    /// Who sent it and over what.
    pub origin: InboundOrigin,
    /// The media type of `payload`, if any.
    pub media_type: Option<MediaType>,
    /// The content.
    pub payload: Vec<u8>,
    /// When it was received.
    pub received_at: u64,
    /// When it was read, for a kept row.
    pub read_at: Option<u64>,
    /// When it was kept, for a kept row.
    pub kept_at: Option<u64>,
}

/// A message that has been read and whose durable copy is already gone.
///
/// # This type is the enforcement, not a convenience
///
/// It is the ONLY thing [`crate::HumanStore::keep`] accepts, and the only
/// way to obtain one is [`crate::HumanStore::mark_read`] — a local UI
/// action on a row the store already held. So:
///
/// - a remote sender cannot construct one (no public constructor, no
///   `Deserialize`, private fields);
/// - a notification action on an unopened message cannot produce one,
///   because reading is what mints it;
/// - it cannot survive the process, because it is not serializable and
///   nothing writes it anywhere. After a restart, a read-unkept message
///   is gone by design and `keep` has nothing to be called with.
///
/// The last point is why this is a type rather than a row-id lookup: a
/// row id would still exist after a restart and would need a runtime
/// check to refuse. Holding the content in memory makes the restart case
/// disappear instead of being defended.
#[derive(Clone, PartialEq, Eq)]
pub struct ReadEphemeral {
    pub(crate) app_message_id: AppMessageId,
    pub(crate) origin: InboundOrigin,
    pub(crate) media_type: Option<MediaType>,
    pub(crate) payload: Vec<u8>,
    pub(crate) received_at: u64,
    pub(crate) read_at: u64,
}

impl ReadEphemeral {
    /// The application id.
    #[must_use]
    pub const fn app_message_id(&self) -> &AppMessageId {
        &self.app_message_id
    }

    /// Who sent it.
    #[must_use]
    pub const fn origin(&self) -> &InboundOrigin {
        &self.origin
    }

    /// The content, still in memory for this session only.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// The media type, if any.
    #[must_use]
    pub fn media_type(&self) -> Option<&MediaType> {
        self.media_type.as_ref()
    }

    /// When it was received.
    #[must_use]
    pub const fn received_at(&self) -> u64 {
        self.received_at
    }

    /// When it was read.
    #[must_use]
    pub const fn read_at(&self) -> u64 {
        self.read_at
    }
}

// ---------------------------------------------------------------------
// Debug that cannot become a shadow message archive
// ---------------------------------------------------------------------
//
// `RETENTION.md` §8: logs, analytics, crash reports, notification
// databases, OS backup, and search indexes must not become shadow
// message archives. A derived `Debug` on any of these types puts the
// message BODY into whatever printed it — a panic message, a tracing
// span, a crash report — where the retention state machine has no reach
// at all. A message deleted at read would still be sitting in a log.
//
// So the five types carrying content implement `Debug` by hand and print
// the payload's LENGTH. Everything a debugger actually wants — which
// message, from whom, how big, what state — survives; the one thing that
// must not leave the store does not. Writing these out rather than
// deriving is also what makes the omission visible: adding a field to
// one of these structs will not silently start logging it.

fn redacted(f: &mut core::fmt::Formatter<'_>, name: &str, len: usize) -> core::fmt::Result {
    write!(f, "{name} {{ payload: <{len} bytes redacted>, ")
}

impl core::fmt::Debug for NewOutbound {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        redacted(f, "NewOutbound", self.payload.len())?;
        write!(
            f,
            "app_message_id: {:?}, destination: {:?}, media_type: {:?}, created_at: {} }}",
            self.app_message_id, self.destination, self.media_type, self.created_at
        )
    }
}

impl core::fmt::Debug for PendingOutbound {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        redacted(f, "PendingOutbound", self.payload.len())?;
        write!(
            f,
            "row_id: {:?}, app_message_id: {:?}, destination: {:?}, media_type: {:?}, \
             created_at: {}, last_attempt_at: {:?}, attempts: {} }}",
            self.row_id,
            self.app_message_id,
            self.destination,
            self.media_type,
            self.created_at,
            self.last_attempt_at,
            self.attempts
        )
    }
}

impl core::fmt::Debug for NewInbound {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        redacted(f, "NewInbound", self.payload.len())?;
        write!(
            f,
            "app_message_id: {:?}, origin: {:?}, media_type: {:?}, received_at: {} }}",
            self.app_message_id, self.origin, self.media_type, self.received_at
        )
    }
}

impl core::fmt::Debug for StoredInbound {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        redacted(f, "StoredInbound", self.payload.len())?;
        write!(
            f,
            "row_id: {:?}, app_message_id: {:?}, origin: {:?}, media_type: {:?}, \
             received_at: {}, read_at: {:?}, kept_at: {:?} }}",
            self.row_id,
            self.app_message_id,
            self.origin,
            self.media_type,
            self.received_at,
            self.read_at,
            self.kept_at
        )
    }
}

impl core::fmt::Debug for ReadEphemeral {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        redacted(f, "ReadEphemeral", self.payload.len())?;
        write!(
            f,
            "app_message_id: {:?}, origin: {:?}, media_type: {:?}, received_at: {}, \
             read_at: {} }}",
            self.app_message_id, self.origin, self.media_type, self.received_at, self.read_at
        )
    }
}
