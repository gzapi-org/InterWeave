// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! IPC v2: the frame codec and the handshake that decides authority.
//!
//! Language-neutral by contract (ADR-0039). Nothing here is a socket, a
//! stream, an async runtime, or a platform type — this crate decides what
//! the *bytes* mean, and carrying them is someone else's job. That split
//! is what lets the desktop daemon and an independent third-party client
//! agree without sharing a transport implementation.
//!
//! Two things are worth reading before using it:
//!
//! - [`framing`] never allocates on a declared length. The prefix arrives
//!   from the other side of a socket, so it is untrusted even locally: an
//!   owner-protected socket bounds who may connect, not what they send
//!   afterwards.
//! - [`handshake`] keeps *requested* and *granted* as different types, and
//!   takes the authority domain from the accepting code rather than the
//!   frame. A client claiming `client.kind = "admin"` on the data socket
//!   is still on the data socket (ADR-0037).

#![forbid(unsafe_code)]

pub mod framing;
pub mod handshake;

pub use framing::{
    DecodedFrame, FrameError, LENGTH_PREFIX_BYTES, MAX_BODY_BYTES, decode_frame, encode_frame,
};
pub use handshake::{
    AuthorityDomain, ClientInfo, EndpointClaim, FEATURE_KEEPALIVE, HandshakeOutcome, Hello,
    HelloTag, IPC_MAJOR, IpcVersion, MAX_REQUESTED, RequestedCapability,
};
