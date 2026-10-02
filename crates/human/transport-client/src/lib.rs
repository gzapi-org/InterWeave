// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The human client's transport facade (plan §17 (1)): generic over the
//! neutral [`DataSessionBinding`](interweave_local_client_api::DataSessionBinding),
//! it owns the client's half of retention -- commit-pending, send,
//! transport-terminal; drain, commit-unread, present; re-open with
//! backoff; the degraded-storage reaction; the retry that resends the
//! stored bytes (ADR-0050 rule 7) under the stored transport id
//! (ADR-0019, schema v6).
//!
//! The caller-facing surface is the contract agreed with the client's
//! role (relay seqs 10522, 10534, 10540): [`OutboundStatus`],
//! [`SendProblem`], [`SessionState`], [`SessionProblem`],
//! [`Connectivity`], [`Received`] and [`ClientEvent`]. Nothing here names
//! libp2p, a daemon or IPC, and nothing claims more than the transport
//! proved: `Accepted` is remote queue admission, never read or seen.

mod backoff;
mod client;
mod model;
mod problem;
mod queue;

pub use client::{ClientConfig, Destination, RowError, SendError, TransportClient};
pub use model::{
    ClientEvent, Connectivity, Diagnostics, Origin, OutboundStatus, OutboundUpdate, Received,
    SessionState,
};
pub use problem::{SendProblem, SessionProblem};
