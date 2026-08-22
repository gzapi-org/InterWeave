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
/// COMPUTED from the other limits, not chosen to look round. The first
/// version of this was a hand-derived 8 MiB that undercounted a legal
/// peer by a third: at every documented maximum one record serializes to
/// roughly 11 KiB, so a full cache is over 11 MiB — `flush` would write
/// a perfectly legal file that the next `load` quarantined, and the
/// cache would delete its own contents on restart for no reason a user
/// could see.
///
/// Deriving it means the two can no longer disagree. Raising
/// [`MAX_ADDRESSES_PER_PEER`] or [`MAX_LABEL_BYTES`] moves this with
/// them.
pub const MAX_CACHE_FILE_BYTES: u64 = {
    // Field names, quotes, commas, braces, and the indentation of a
    // pretty-printed file. Generous per item rather than exact: this is
    // a ceiling, and being loose costs nothing while being tight costs a
    // legal file.
    const STRUCTURE_PER_ITEM: u64 = 128;
    const STRUCTURE_PER_CAPABILITY: u64 = 256;
    const PEER_ID_BYTES: u64 = 64;
    const TIMESTAMP_BYTES: u64 = 24;

    // JSON ESCAPING, which the first version of this budgeted at 1×.
    //
    // A stored byte is not a serialized byte. Within printable ASCII the
    // worst case is `"` and `\`, which encode as two bytes each — so a
    // value contributes at most twice its length. That bound only holds
    // because [`is_bounded_label`] refuses everything else: a control
    // character encodes as six (`\u0000`), and a cache of those would
    // serialize to three times this ceiling while passing every input
    // check.
    const ESCAPE_FACTOR: u64 = 2;

    let per_address =
        ESCAPE_FACTOR * MAX_ADDRESS_BYTES as u64 + TIMESTAMP_BYTES + STRUCTURE_PER_ITEM;
    let per_capability =
        ESCAPE_FACTOR * 3 * MAX_LABEL_BYTES as u64 + 2 * TIMESTAMP_BYTES + STRUCTURE_PER_CAPABILITY;
    let per_peer = PEER_ID_BYTES
        + 3 * TIMESTAMP_BYTES
        + MAX_ADDRESSES_PER_PEER as u64 * per_address
        + MAX_CAPABILITIES_PER_PEER as u64 * per_capability
        + STRUCTURE_PER_ITEM;

    MAX_PEERS as u64 * per_peer + 4096
};

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
