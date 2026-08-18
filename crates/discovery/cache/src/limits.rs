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
