// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The endpoint-directory exchange, `/interweave/endpoints/1.0.0` (ADR-0031).
//!
//! Neutral shapes only. The byte layout belongs to the codec
//! (`architecture/transport/libp2p/ENDPOINTS.md`); what lives here is what
//! a directory MAY carry and the two ceilings every implementation shares,
//! so the config crate, the runtime and the backend read one number each
//! rather than three copies of it.

use crate::EndpointId;

/// The most entries one directory response may carry (ADR-0031).
///
/// The profile's `directory.max_advertised` ceiling is this same value,
/// read from here rather than restated.
pub const MAX_DIRECTORY_ENTRIES: usize = 32;

/// The hard ceiling on a directory entry's freshness, in milliseconds.
///
/// A remote may send any `ttl_ms` at all; the receiver clamps it to
/// `min(remote, local, MAX_DIRECTORY_TTL_MS)`. Five minutes.
pub const MAX_DIRECTORY_TTL_MS: u32 = 300_000;

/// Default directory queries per minute per remote PeerId (ADR-0031).
pub const DEFAULT_QUERIES_PER_PEER_PER_MINUTE: u32 = 12;
/// Hard ceiling on directory queries per minute per remote PeerId.
pub const MAX_QUERIES_PER_PEER_PER_MINUTE: u32 = 60;
/// Default concurrent directory exchanges per profile (ADR-0031).
pub const DEFAULT_INFLIGHT_QUERIES: usize = 16;
/// Hard ceiling on concurrent directory exchanges per profile.
pub const MAX_INFLIGHT_QUERIES: usize = 64;

/// A directory as it crosses the wire, before validation.
///
/// Peer-asserted throughout: this is what the authenticated remote PeerId
/// CLAIMS to advertise, and nothing in it proves who or what owns an
/// endpoint (ADR-0031). Validation, clamping and freshness are the
/// runtime's job — `transport-runtime`'s `directory` module — and its
/// tests are where those rules are proved; this type deliberately
/// enforces none of them, so a hostile list can be represented and then
/// refused rather than being unrepresentable and silently dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointDirectoryV1 {
    /// When the remote says it built the list. Diagnostic only.
    pub generated_at_ms: u64,
    /// The remote's suggested freshness, as sent.
    pub ttl_ms: u32,
    /// The advertised endpoints, as received.
    ///
    /// A `Vec`, not a set: a duplicate is a protocol violation the
    /// validator must be able to SEE, and a set would erase it on the way
    /// in.
    pub endpoints: Vec<EndpointId>,
}

/// Why a directory query was refused, as the wire distinguishes it.
///
/// Coarse on purpose (ADR-0031): none of these reveals whether any
/// endpoint exists, and the wire codes are hand-assigned in the codec
/// rather than derived from this enum's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryRefusal {
    /// The per-peer query budget or the profile's in-flight bound is spent.
    Overloaded,
    /// The querying peer is not data-plane trusted.
    Unauthorized,
    /// The directory is disabled for this profile, or the node is draining.
    Unavailable,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spec quotes 2094 as the largest response and derives it from
    /// these same widths. If either ceiling moves, this is the test that
    /// says the quoted number is now wrong.
    #[test]
    fn the_largest_directory_response_is_2094_bytes() {
        let tag = 1;
        let generated_at = 8;
        let ttl = 4;
        let count = 1;
        let entry = 1 + EndpointId::MAX_BYTES;
        assert_eq!(
            tag + generated_at + ttl + count + MAX_DIRECTORY_ENTRIES * entry,
            2094
        );
    }

    #[test]
    fn the_entry_count_fits_the_one_byte_wire_field() {
        assert!(u8::try_from(MAX_DIRECTORY_ENTRIES).is_ok());
    }
}
