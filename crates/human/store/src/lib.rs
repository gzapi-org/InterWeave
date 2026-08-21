// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Durable ADR-0044 retention storage for the human client.
//!
//! `interweave-human-core` decides whether content must be durable; this
//! crate is the thing that carries the decision out. Splitting them is
//! what lets the decision be tested by enumeration and the storage be
//! tested against a real file that gets closed and reopened.
//!
//! # Three content tables, and no fourth
//!
//! `pending_outbound`, `unread_inbound`, `kept_inbound`. There is no
//! `messages` table and no `conversation_history` table, because a
//! general archive is precisely the shape ADR-0044 exists to forbid.
//! That is not left to reviewer vigilance: [`schema::verify_shape`] runs
//! on every open and refuses a database containing one, so a migration
//! that added an archive "for the UI" fails at the next startup instead
//! of quietly becoming the thing.
//!
//! # What a remote sender cannot do
//!
//! Reach `kept_inbound`. The only route in is [`HumanStore::keep`],
//! which accepts a [`ReadEphemeral`] and nothing else, and the only way
//! to obtain one of those is [`HumanStore::mark_read`] — a local action
//! on a row the store already held. There is no field, parameter, or
//! envelope value through which remote content could ask to be retained.
//!
//! # What is deliberately absent
//!
//! No transport private key, trust allowlist, endpoint lease, Kademlia
//! bucket, relay reservation, AutoNAT evidence, direct dedup record, or
//! endpoint-directory cache. This database is application state: it is
//! safe to delete, and deleting it changes no PeerId and no trust
//! policy.
//!
//! # Degradation is not optional
//!
//! If the medium cannot hold new unread content, the client must stop
//! presenting itself as a durable receiver rather than accept a stream
//! it will lose. [`HumanStore::health`] reports that, and
//! [`StoreError::Degraded`] is returned by every content-committing
//! method while it holds.

#![forbid(unsafe_code)]

pub mod records;
pub mod schema;
pub mod store;

pub use records::{
    AppMessageId, InboundOrigin, NewInbound, NewOutbound, OutboundDestination, PendingOutbound,
    ReadEphemeral, RowId, StoredInbound,
};
pub use schema::{REQUIRED_TABLES, SCHEMA_VERSION};
pub use store::{HumanStore, StoreOptions};

// Re-exported so a caller acting on retention does not need a second
// dependency to name the event it is reporting.
pub use interweave_human_core::retention::{Durability, StorageHealth, TerminalCause};

/// Everything that can go wrong with the human store.
#[derive(Debug)]
pub enum StoreError {
    /// The storage medium cannot hold new content.
    ///
    /// The caller must release or disable the human endpoint and suspend
    /// local broadcast delivery until [`HumanStore::recheck_health`]
    /// reports recovery. Continuing to accept would mean claiming an
    /// unread durability the store cannot provide.
    Degraded,
    /// The row does not exist, or is not in the state the call requires.
    NoSuchRow,
    /// The retention state machine refused the transition.
    KeepRefused(interweave_human_core::retention::KeepRefused),
    /// An `app_message_id` outside the HumanChatV2 grammar.
    MalformedAppMessageId {
        /// What was supplied.
        got: String,
    },
    /// One peer used an `app_message_id` it had already used, for
    /// different content.
    ///
    /// Inbound identity is `(source_peer, app_message_id)`, and
    /// `app_message_id` is chosen by the sender. Repeating a keep for the
    /// SAME message is idempotent and succeeds; repeating the identity
    /// with a different body, endpoint, channel, media type, or receipt
    /// time is a collision, and answering it by silently selecting one of
    /// the two bodies would lose the other.
    IdentityConflict {
        /// The reused application id.
        app_message_id: String,
        /// The peer that reused it.
        source_peer: String,
    },
    /// A payload above the transport payload ceiling.
    ///
    /// The store holds the exact wire bytes so a retry can resend them
    /// unchanged, so anything transport could not have carried cannot be
    /// a pending row either.
    PayloadTooLarge {
        /// The supplied length.
        got: usize,
        /// The ceiling.
        max: usize,
    },
    /// A stored row could not be parsed by this build.
    Corrupt(String),
    /// A schema migration failed, or the schema is not one this build
    /// understands.
    Migration(String),
    /// An error from SQLite itself.
    Sql(rusqlite::Error),
    /// The store directory could not be created.
    Io(std::io::Error),
    /// Owner-only permissions cannot be enforced on this platform.
    ///
    /// Refusing beats creating a directory of message content this build
    /// cannot protect.
    UnsupportedPlatform,
}

impl From<rusqlite::Error> for StoreError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sql(value)
    }
}

impl core::fmt::Display for StoreError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Degraded => write!(
                f,
                "human store is degraded and cannot durably hold new content"
            ),
            Self::NoSuchRow => write!(f, "no such row, or it is not in the required state"),
            Self::KeepRefused(reason) => write!(f, "keep refused: {reason:?}"),
            Self::MalformedAppMessageId { got } => write!(
                f,
                "app_message_id must be 32 lowercase hex characters, got {got:?}"
            ),
            Self::IdentityConflict {
                app_message_id,
                source_peer,
            } => write!(
                f,
                "peer {source_peer} reused app_message_id {app_message_id} for different content"
            ),
            Self::PayloadTooLarge { got, max } => {
                write!(f, "payload is {got} bytes; the limit is {max}")
            }
            Self::Corrupt(detail) => write!(f, "stored row is unreadable: {detail}"),
            Self::Migration(detail) => write!(f, "schema migration failed: {detail}"),
            Self::Sql(e) => write!(f, "sqlite: {e}"),
            Self::Io(e) => write!(f, "human store directory: {e}"),
            Self::UnsupportedPlatform => write!(
                f,
                "owner-only directory permissions cannot be enforced on this platform"
            ),
        }
    }
}

impl core::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Sql(e) => Some(e),
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}
