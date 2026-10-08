// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The desktop IPC v2 client (`LOCAL-IPC.md`; plan §16 (1)): the neutral
//! binding traits of `local-client-api` over the daemon's two sockets.
//!
//! [`IpcBinding`] opens an [`IpcSession`] on the data socket and an
//! [`IpcAdmin`] on the admin socket; a caller holds a `DataSessionPort`
//! and an `AdminPort` exactly as it would from the in-process binding,
//! and `tests/local-client-conformance` runs the same generic functions
//! against both. Nothing is decided here that the daemon does not decide:
//! every refusal is the server's answer, carried back as its code, except
//! the one read that never reaches the server -- [`IpcSession`]'s
//! `events`, which takes from what the server already pushed.
//!
//! Five things differ from the in-process binding, all of the wire and
//! all named in `LOCAL-IPC.md` (the first three A 2026-09-30, the last
//! two A 2026-10-08):
//!
//! - Events are pushed, so `events` returns what has ARRIVED; one the
//!   binding admitted may still be on its way.
//! - The receive buffer is bounded at the granted `event_queue`. A full
//!   buffer stops the client reading its socket, so a response queued
//!   behind undrained events waits for them -- and past the keepalive
//!   miss threshold the server closes the connection as wedged. Draining
//!   events is part of holding a lease.
//! - The grouped order of `events` (notices, then direct, then
//!   broadcast) holds within one server pump; across pumps batches are
//!   read as they arrive.
//! - What arrived before the end is still delivered: a positive `max`
//!   takes it, then answers the end, while `events(0)` answers the end
//!   at once. In process the queues are the runtime's and go with it.
//! - The connection can end `Timeout` on the client's own bound: once a
//!   `ping` has been read, `CLIENT_SILENCE_TIMEOUT` with no frame read
//!   ends it, and every waiting call with it.

#![cfg(unix)]
#![forbid(unsafe_code)]

mod admin;
mod connection;
mod session;

pub use admin::IpcAdmin;
pub use session::{IpcBinding, IpcSession, SocketPaths};
