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
mod problem;
mod queue;

pub use client::{ClientConfig, TransportClient, WallClock};
// The caller-facing vocabulary lives in `interweave-human-client-api`,
// where the UI model can name it without reaching this crate's store
// (plan §17 P2); re-exported so a caller of the facade needs one crate.
pub use interweave_human_client_api::{
    ClientEvent, Connectivity, Destination, Diagnostics, Origin, OutboundStatus, OutboundUpdate,
    Received, RowError, SendError, SendProblem, SessionProblem, SessionState, TrustList,
    TrustProblem, TrustSetFailure,
};
