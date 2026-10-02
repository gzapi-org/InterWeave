// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The human client's UI model (plan §17 (6)): presentation state only,
//! pure and synchronous, between the transport facade and the views. Its
//! surface was agreed with the client's role before it was built (relay
//! seqs 10630, 10639, 10642): the composition root feeds it what the
//! facade and the store did, a view renders what it returns, and a
//! person's actions come back as [`Intent`]s -- a view never reaches the
//! facade, the store, or IPC.
//!
//! It names the client's vocabulary (`interweave-human-client-api`) and
//! nothing that reaches a store, a transport or a toolkit (plan §17 P2).

#![forbid(unsafe_code)]

mod labels;
mod model;

pub use labels::{
    ErrorClass, LabelKey, outbound_label, send_error_class, send_problem_class,
    session_problem_class,
};
pub use model::{
    Composer, ConversationKey, ConversationSummary, DEDUP_CAP, Direction, HELD_UPDATE_CAP, Intent,
    ItemDiagnostics, ItemKey, ItemStatus, ListedInbound, ListedOutbound, MessageItem, Reply,
    Retention, RouteLabel, SESSION_ITEM_CAP, SessionNotice, Table, Trust, UiModel,
};
