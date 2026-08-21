// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The frozen defaults from `architecture/discovery/providers/peer-cache.md`.
//!
//! Restated here as constants rather than read from configuration
//! because they are the contract's numbers, and a cache that silently
//! grew past them would stop being bounded advisory state and start
//! being an unbounded map with a nice name.

/// Time-to-live after the last successful or validated observation.
///
/// Seven days. Long enough that a laptop closed over a holiday still
/// cold-starts fast; short enough that an address abandoned two weeks
/// ago is not still being dialled.
pub const DEFAULT_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1_000;

/// Maximum peers retained.
pub const MAX_PEERS: usize = 1_024;

/// Maximum addresses retained per peer.
pub const MAX_ADDRESSES_PER_PEER: usize = 8;

/// Maximum protocol capability observations retained per peer.
///
/// Positive and negative observations share this budget: a peer that
/// stopped advertising a protocol should not be able to double its
/// footprint by generating one of each.
pub const MAX_CAPABILITIES_PER_PEER: usize = 16;

/// Longest opaque address retained, in bytes.
///
/// Matches the neutral candidate contract's own address bound. The
/// on-disk record is a `String`, so without this the file decides how
/// much memory a load allocates.
pub const MAX_ADDRESS_BYTES: usize = 256;

/// Longest protocol family, network hash, or role string, in bytes.
///
/// One number for the three because they are the same kind of thing: a
/// short opaque label compared exactly and never parsed.
pub const MAX_LABEL_BYTES: usize = 128;

/// Largest cache file this build will read, in bytes.
///
/// DERIVED, not chosen. One peer at every other limit is roughly
/// 64 bytes of PeerId, 8 addresses of 256 plus a timestamp each, and 16
/// capability observations of three 128-byte labels plus scalars — under
/// 8 KiB once JSON overhead is counted generously. At [`MAX_PEERS`] that
/// is 8 MiB, and a file larger than the format's own worst case is not a
/// big cache, it is not this format.
///
/// The point is that the size is checked BEFORE the bytes are read. The
/// cache is advisory and disposable, so a file that cannot be true is
/// quarantined rather than parsed.
pub const MAX_CACHE_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Minimum interval between writes to disk.
///
/// Five seconds. A burst of successful dials on startup would otherwise
/// rewrite the whole file once per connection.
pub const WRITE_DEBOUNCE_MS: u64 = 5_000;

/// The bounds one cache instance runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheLimits {
    /// Time-to-live after the last successful observation.
    pub ttl_ms: u64,
    /// Maximum peers retained.
    pub max_peers: usize,
    /// Maximum addresses per peer.
    pub max_addresses_per_peer: usize,
    /// Maximum capability observations per peer.
    pub max_capabilities_per_peer: usize,
    /// Minimum interval between writes.
    pub write_debounce_ms: u64,
}

impl Default for CacheLimits {
    fn default() -> Self {
        Self {
            ttl_ms: DEFAULT_TTL_MS,
            max_peers: MAX_PEERS,
            max_addresses_per_peer: MAX_ADDRESSES_PER_PEER,
            max_capabilities_per_peer: MAX_CAPABILITIES_PER_PEER,
            write_debounce_ms: WRITE_DEBOUNCE_MS,
        }
    }
}
