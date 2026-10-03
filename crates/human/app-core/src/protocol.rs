// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! What the model side asks of the facade side, and what the facade side
//! reports back. The two sides may live on different threads -- on the
//! desktop the facade runs on its own runtime, because `ipc-client` loses
//! its lease when a session's events go undrained -- so everything that
//! crosses is a plain value.

use interweave_human_chat_protocol::HumanChatV2;
use interweave_human_client_api::{ClientEvent, Destination, Diagnostics, Received, SendError};
use interweave_human_store::RowId;
use interweave_human_ui_model::{ConversationKey, ItemKey, ListedInbound, ListedOutbound, Table};

/// A person's intent, resolved by the model, for the facade side to carry
/// out against the facade and the store.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Command {
    /// Send `draft` to the conversation `key`.
    Send {
        /// The conversation.
        key: ConversationKey,
        /// The text as the person wrote it.
        draft: String,
    },
    /// Mark an unread row read.
    MarkRead(RowId),
    /// Keep the read message `item`, from the copy `from`'s read or
    /// unkeep handed back.
    Keep {
        /// The item.
        item: ItemKey,
        /// The row whose read or unkeep handed back the content.
        from: (Table, RowId),
    },
    /// Remove Keep from a kept row.
    Unkeep(RowId),
    /// Retry a pending row now.
    Retry(RowId),
    /// Cancel a pending row.
    Cancel(RowId),
    /// Try opening the session again.
    Reopen,
    /// Re-check storage now.
    RecheckStorage,
}

/// The store's rows at start, decoded for the model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    /// Pending outbound rows.
    pub pending: Vec<ListedOutbound>,
    /// Unread inbound rows.
    pub unread: Vec<ListedInbound>,
    /// Kept inbound rows.
    pub kept: Vec<ListedInbound>,
    /// Rows the store holds that could not be decoded for display: still
    /// held, never shown, counted so the root can say so.
    pub undecodable: usize,
}

/// What the facade side did, for the model side to apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    /// The store's rows at start. Applied before anything else, since the
    /// model treats the pending listing as authoritative.
    Listed(Listing),
    /// The store's unread rows again, after the facade reported unread
    /// content it holds but did not hand over (`ClientEvent::UnreadInStore`).
    UnreadListed(Vec<ListedInbound>),
    /// A message committed unread and handed over.
    Received(Received),
    /// An event from the facade.
    Client(ClientEvent),
    /// The facade's counters changed.
    Diagnostics(Diagnostics),
    /// A send was committed as `row`.
    Sent {
        /// The conversation it was sent from.
        key: ConversationKey,
        /// Its pending row.
        row: RowId,
        /// Where it went.
        destination: Destination,
        /// What was sent.
        envelope: HumanChatV2,
        /// When, on the wall clock.
        at: u64,
    },
    /// A send was refused with no row: the composer keeps its text.
    SendRefused {
        /// The conversation.
        key: ConversationKey,
        /// The text that was refused.
        draft: String,
        /// Why.
        error: SendError,
    },
    /// An unread row was marked read.
    Read(RowId),
    /// `item` was kept as `row`.
    Kept {
        /// The item.
        item: ItemKey,
        /// Its kept row.
        row: RowId,
    },
    /// A kept row was unkept.
    Unkept(RowId),
    /// A command finished. Every command ends with this or `Failed`, so
    /// the model side can offer the same action again.
    Done(Command),
    /// A command could not be carried out; nothing changed.
    Failed(Command),
}
