// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The desktop IPC v2 server (`LOCAL-IPC.md`; plan §16 (1), (5)-(9)).
//!
//! A library, generic over the neutral binding traits of
//! `interweave-local-client-api`: it serializes `DataSessionPort` and
//! `AdminPort` calls onto two Unix sockets and adds no behaviour model of
//! its own -- no lease table, no event queue. Stage 13's exit gate is that
//! IPC is only a serialization of those semantics, and the missing
//! dependency on `crates/transport/*` is the structural half of it.
//!
//! Unix only (plan §16 (9)): the Windows named pipe is carried to Stage 15.

#![cfg(unix)]
#![forbid(unsafe_code)]

mod admission;
mod counters;
#[cfg(test)]
mod fake;
mod frames;
mod hello;
pub mod listen;

pub use admission::Limits;
pub use counters::Counters;
pub use hello::{KeepalivePolicy, ServerConfig};
pub use listen::{BindError, Listeners, SocketPaths, bind};

/// Requests one connection may have handed to its session at once
/// (`LOCAL-IPC.md` §Cancellation mapping and request concurrency: a
/// protocol constant, not a profile value).
pub const MAX_IN_FLIGHT: usize = 16;

/// Requests one connection may have waiting behind those in flight; one
/// more is answered `Overloaded`. 16 + 48 = the 64 outstanding commands
/// per client of `TRANSPORT.md` §Backpressure.
pub const MAX_PENDING: usize = 48;
