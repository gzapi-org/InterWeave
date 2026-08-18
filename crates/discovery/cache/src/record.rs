// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! What one cached peer looks like on disk.
//!
//! # What is NOT here, and cannot be added by accident
//!
//! No EndpointId, no ChannelId, no trust record, no membership record,
//! no application payload, no human presence. Those absences are the
//! reason this file is safe to delete and safe to lose, and they are
//! checked by a test that serialises a fully-populated record and looks
//! for their field names — so an added field fails the suite rather
//! than being noticed in review.

use serde::{Deserialize, Serialize};

/// An address that was observed to work, and when.
///
/// Carries a timestamp rather than living in a set, because "which
/// addresses worked recently" is the question the cache exists to
/// answer, and it is also what makes eviction principled: the address
/// dropped at the cap is the least recently successful one, not an
/// arbitrary member of a set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressObservation {
    /// The opaque transport address.
    ///
    /// Opaque on purpose. Parsing a multiaddr here would put libp2p's
    /// address grammar into persisted state, and the cache would then
    /// need a migration every time that grammar moved.
    pub address: String,
    /// When this address last succeeded.
    pub last_success_ms: u64,
}

/// An advisory protocol fact learned from an authenticated connection.
///
/// Never authorization, and never proof the peer is reachable now. It
/// answers one scheduling question — is a targeted lookup worth trying —
/// and a fresh Identify exchange supersedes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolCapabilityObservation {
    /// The protocol family, e.g. `interweave/kad`.
    pub protocol_family: String,
    /// The wire major version observed.
    pub wire_major: u32,
    /// The network namespace hash the peer advertised.
    pub network_hash: String,
    /// The role advertised, e.g. `server`.
    pub role: String,
    /// Whether the peer was observed to support it.
    ///
    /// A `false` observation is deliberately storable: recording that a
    /// peer does NOT advertise the Kademlia server protocol suppresses
    /// pointless targeting until expiry, which is cheaper than
    /// rediscovering the absence on every restart.
    pub supported: bool,
    /// When it was observed.
    pub observed_at_ms: u64,
}

/// One cached peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerRecord {
    /// The peer, as its canonical string form.
    pub peer_id: String,
    /// Addresses that worked, most recently successful first.
    #[serde(default)]
    pub addresses: Vec<AddressObservation>,
    /// When this peer was first observed to work.
    pub first_success_ms: u64,
    /// When it last worked. The TTL runs from here.
    pub last_success_ms: u64,
    /// When a dial to it last failed, if one has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure_ms: Option<u64>,
    /// Bounded advisory capability observations.
    #[serde(default)]
    pub capabilities: Vec<ProtocolCapabilityObservation>,
}

impl PeerRecord {
    /// When this record stops being usable.
    #[must_use]
    pub const fn expires_at_ms(&self, ttl_ms: u64) -> u64 {
        self.last_success_ms.saturating_add(ttl_ms)
    }

    /// Whether this record is still within its TTL at `now_ms`.
    #[must_use]
    pub const fn is_fresh_at(&self, now_ms: u64, ttl_ms: u64) -> bool {
        now_ms < self.expires_at_ms(ttl_ms)
    }

    /// The capability observations still fresh at `now_ms`.
    ///
    /// Bounded by the ENCLOSING record's expiry as well as their own age
    /// — "capability freshness never outlives the enclosing peer-cache
    /// record TTL". A capability observed yesterday on a record that
    /// expired this morning is not fresh evidence; it is evidence about
    /// a peer this cache has already stopped vouching for.
    #[must_use]
    pub fn fresh_capabilities(&self, now_ms: u64, ttl_ms: u64) -> &[ProtocolCapabilityObservation] {
        if self.is_fresh_at(now_ms, ttl_ms) {
            &self.capabilities
        } else {
            &[]
        }
    }
}
