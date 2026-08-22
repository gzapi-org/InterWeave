// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Bounded advisory peer-cache persistence.
//!
//! An advisory fast-restart candidate source, and nothing more. A cached
//! peer is never trusted because it was cached, and a cached protocol
//! capability is never authorization and never a guarantee that the peer
//! is reachable now.
//!
//! # Safe to delete
//!
//! Deleting this file costs a cold start and nothing else. No PeerId, no
//! trust policy, no endpoint lease, no application content, and no human
//! presence depends on it — which is also why a corrupt file is
//! quarantined and the cache continues empty rather than failing
//! startup. The one piece of state whose loss is genuinely harmless
//! would be a poor choice to make fatal.
//!
//! # Bounded at every level
//!
//! Peers, addresses per peer, and capability observations per peer are
//! all capped, and each cap evicts by least recent usefulness rather
//! than insertion order. An insertion-ordered cache evicts the entry
//! currently in use and keeps an untouched newer one.

#![forbid(unsafe_code)]

pub mod cache;
pub mod limits;
pub mod record;

pub use cache::{CacheHealth, FORMAT_VERSION, PeerCache, SOURCE};
pub use limits::{
    CacheLimits, DEFAULT_TTL_MS, MAX_ADDRESS_BYTES, MAX_ADDRESSES_PER_PEER, MAX_CACHE_FILE_BYTES,
    MAX_CAPABILITIES_PER_PEER, MAX_LABEL_BYTES, MAX_PEERS, WRITE_DEBOUNCE_MS,
};
pub use record::{AddressObservation, PeerRecord, ProtocolCapabilityObservation};

/// What can go wrong reading or writing the cache.
#[derive(Debug)]
pub enum CacheError {
    /// The filesystem refused.
    Io(std::io::Error),
    /// The cache could not be serialised.
    Serialize(serde_json::Error),
    /// A value offered to the cache is outside the bounded format.
    ///
    /// Refused at the point it enters. The load path validates every
    /// record, so accepting an over-long address here would mean `flush`
    /// writing a file the next `load` quarantines — the cache deleting
    /// its own contents on restart, for a value it had already accepted.
    OutOfBounds {
        /// Which field.
        field: &'static str,
        /// Bytes supplied.
        got: usize,
        /// Bytes permitted.
        max: usize,
    },
}

impl From<std::io::Error> for CacheError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for CacheError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serialize(value)
    }
}

impl core::fmt::Display for CacheError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "peer cache i/o: {e}"),
            Self::Serialize(e) => write!(f, "peer cache serialisation: {e}"),
            Self::OutOfBounds { field, got, max } => {
                write!(f, "{field} is {got} bytes; the limit is 1..={max}")
            }
        }
    }
}

impl core::error::Error for CacheError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Serialize(e) => Some(e),
            Self::OutOfBounds { .. } => None,
        }
    }
}
