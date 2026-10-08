// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! Independent codecs for InterWeave's frozen wire shapes.
//!
//! **Test-only**, and independent by construction: every rule here is read
//! from the contract text — `architecture/transport/libp2p/DIRECT.md`,
//! `PUBSUB.md`, `architecture/contracts/ENDPOINTS.md`, `TRANSPORT.md`,
//! `LOCAL-IPC.md` and the schemas under `architecture/contracts/schemas/` —
//! and nothing from a production crate. `tools/checks/check_independent_codecs.sh`
//! refuses any workspace package in this crate's dependency graph,
//! dev-dependencies included.
//!
//! What follows from that: when a codec here and production disagree, the
//! contract text is the oracle and the disagreement is a contract finding
//! (testing.md §Compatibility fixtures), never a reason to edit one side
//! until the tests agree.
//!
//! The shapes, one module each:
//!
//! - [`direct_v2`] — the `DirectMessageV2` request frame;
//! - [`broadcast_v1`] — the GossipSub `BroadcastMessageV1` envelope;
//! - [`fingerprint`] — `DirectContentFingerprintV1`;
//! - [`ipc_v2`] — the IPC v2 length-prefixed frame and its envelope classes.
//!
//! `AcceptedV2`/`RejectedV2` have no codec here yet: their byte layout is
//! stated in no contract, so a codec for them could only transcribe
//! production. The question is with architect-cto.

pub mod broadcast_v1;
pub mod direct_v2;
pub mod fingerprint;
pub mod ipc_v2;
pub mod json;
mod wire;

/// Why a frame was refused. The message names the rule, for a test's
/// failure output; nothing branches on its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError(pub String);

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DecodeError {}

/// `DIRECT.md` and `PUBSUB.md`: the payload ceiling both wires enforce at
/// decode, the profile's `max_payload_bytes` never above it.
pub const MAX_PAYLOAD_BYTES: usize = 49_152;

/// `TRANSPORT.md` §Direct send: a present media type is 1..128 bytes.
pub const MAX_MEDIA_TYPE_BYTES: usize = 128;

/// `ENDPOINTS.md`: `^[a-z][a-z0-9._-]{0,63}$`.
#[must_use]
pub fn is_endpoint_id(s: &str) -> bool {
    let b = s.as_bytes();
    match b.split_first() {
        Some((first, rest)) => {
            first.is_ascii_lowercase()
                && rest.len() <= 63
                && rest.iter().all(|c| {
                    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
                })
        }
        None => false,
    }
}

/// A present media type: 1..128 bytes, each in 0x20..=0x7E.
///
/// PRINTABLE, from the two schemas that carry the field
/// (`ipc/payload`, `endpoints/message-received`: `^[\x20-\x7E]+$`).
/// The wire and fingerprint prose says only "ASCII". That gap is raised
/// with architect-cto (01a11c8b), and until it is ruled this follows the
/// schemas, the one reading under which a wire frame round-trips through
/// IPC.
#[must_use]
pub fn is_media_type(s: &str) -> bool {
    (1..=MAX_MEDIA_TYPE_BYTES).contains(&s.len()) && s.bytes().all(|b| (0x20..=0x7e).contains(&b))
}
