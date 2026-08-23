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
///
/// # Why the fields are private
///
/// These limits govern what the RUNTIME will hold, while
/// [`MAX_CACHE_FILE_BYTES`] governs what a LOAD will read -- and the
/// second is computed from the frozen format constants, not from these.
/// Public fields let a caller ask for more peers or more addresses per
/// peer than the format allows, at which point the cache accepts the
/// records, serializes them, flushes a perfectly ordinary file, and
/// quarantines that same file on the next start because it is over the
/// format ceiling. A cache that deletes its own contents on restart,
/// with nothing in the logs but a size complaint about a file it wrote
/// itself.
///
/// So the only door is [`CacheLimitsBuilder::build`], and it permits
/// NARROWING only. A deployment may hold fewer peers than the format
/// allows; it may not hold more, because "more" is not this type's to
/// grant -- it is a change to the frozen disk format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheLimits {
    ttl_ms: u64,
    max_peers: usize,
    max_addresses_per_peer: usize,
    max_capabilities_per_peer: usize,
    write_debounce_ms: u64,
}

impl CacheLimits {
    /// Time-to-live after the last successful observation.
    #[must_use]
    pub const fn ttl_ms(&self) -> u64 {
        self.ttl_ms
    }

    /// Maximum peers retained.
    #[must_use]
    pub const fn max_peers(&self) -> usize {
        self.max_peers
    }

    /// Maximum addresses per peer.
    #[must_use]
    pub const fn max_addresses_per_peer(&self) -> usize {
        self.max_addresses_per_peer
    }

    /// Maximum capability observations per peer.
    #[must_use]
    pub const fn max_capabilities_per_peer(&self) -> usize {
        self.max_capabilities_per_peer
    }

    /// Minimum interval between writes.
    #[must_use]
    pub const fn write_debounce_ms(&self) -> u64 {
        self.write_debounce_ms
    }
}

impl Default for CacheLimits {
    fn default() -> Self {
        // The frozen format itself, which narrows nothing and is
        // therefore valid. `the_format_defaults_are_valid` keeps that
        // true if a constant is ever retuned.
        Self {
            ttl_ms: DEFAULT_TTL_MS,
            max_peers: MAX_PEERS,
            max_addresses_per_peer: MAX_ADDRESSES_PER_PEER,
            max_capabilities_per_peer: MAX_CAPABILITIES_PER_PEER,
            write_debounce_ms: WRITE_DEBOUNCE_MS,
        }
    }
}

/// Proposed limits, on their way to becoming [`CacheLimits`].
///
/// Public fields on purpose: this is plainly the unchecked side of the
/// boundary. [`Default`] is the frozen format, so `..Default::default()`
/// narrows one bound without restating the other four.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheLimitsBuilder {
    /// Time-to-live after the last successful observation.
    pub ttl_ms: u64,
    /// Maximum peers retained. At most [`MAX_PEERS`].
    pub max_peers: usize,
    /// Maximum addresses per peer. At most [`MAX_ADDRESSES_PER_PEER`].
    pub max_addresses_per_peer: usize,
    /// Maximum capability observations per peer. At most
    /// [`MAX_CAPABILITIES_PER_PEER`].
    pub max_capabilities_per_peer: usize,
    /// Minimum interval between writes.
    pub write_debounce_ms: u64,
}

impl Default for CacheLimitsBuilder {
    fn default() -> Self {
        let d = CacheLimits::default();
        Self {
            ttl_ms: d.ttl_ms,
            max_peers: d.max_peers,
            max_addresses_per_peer: d.max_addresses_per_peer,
            max_capabilities_per_peer: d.max_capabilities_per_peer,
            write_debounce_ms: d.write_debounce_ms,
        }
    }
}

impl CacheLimitsBuilder {
    /// Narrow the frozen format, or say why the request is not a
    /// narrowing.
    ///
    /// # Errors
    /// Returns the first [`InvalidCacheLimits`] that applies.
    pub const fn build(self) -> Result<CacheLimits, InvalidCacheLimits> {
        use InvalidCacheLimits as E;

        if self.max_peers == 0 || self.max_peers > MAX_PEERS {
            return Err(E::PeersOutOfRange);
        }
        if self.max_addresses_per_peer == 0 || self.max_addresses_per_peer > MAX_ADDRESSES_PER_PEER
        {
            return Err(E::AddressesPerPeerOutOfRange);
        }
        if self.max_capabilities_per_peer == 0
            || self.max_capabilities_per_peer > MAX_CAPABILITIES_PER_PEER
        {
            return Err(E::CapabilitiesPerPeerOutOfRange);
        }
        // A zero TTL expires every record the instant it is written, so
        // the cache holds nothing and every start is cold. That is a
        // misconfiguration rather than a policy.
        if self.ttl_ms == 0 {
            return Err(E::ZeroTtl);
        }

        Ok(CacheLimits {
            ttl_ms: self.ttl_ms,
            max_peers: self.max_peers,
            max_addresses_per_peer: self.max_addresses_per_peer,
            max_capabilities_per_peer: self.max_capabilities_per_peer,
            write_debounce_ms: self.write_debounce_ms,
        })
    }
}

/// Why proposed cache limits were refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidCacheLimits {
    /// `max_peers` is zero or above the frozen [`MAX_PEERS`].
    PeersOutOfRange,
    /// `max_addresses_per_peer` is zero or above the frozen bound.
    AddressesPerPeerOutOfRange,
    /// `max_capabilities_per_peer` is zero or above the frozen bound.
    CapabilitiesPerPeerOutOfRange,
    /// `ttl_ms` is zero, so nothing is ever retained.
    ZeroTtl,
}

impl core::fmt::Display for InvalidCacheLimits {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::PeersOutOfRange => {
                "max_peers must be 1..=MAX_PEERS; the disk format cannot hold more"
            }
            Self::AddressesPerPeerOutOfRange => {
                "max_addresses_per_peer must be 1..=MAX_ADDRESSES_PER_PEER"
            }
            Self::CapabilitiesPerPeerOutOfRange => {
                "max_capabilities_per_peer must be 1..=MAX_CAPABILITIES_PER_PEER"
            }
            Self::ZeroTtl => "ttl_ms of zero retains nothing",
        })
    }
}

impl core::error::Error for InvalidCacheLimits {}
