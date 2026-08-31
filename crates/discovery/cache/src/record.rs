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

/// The protocol family this cache maps to and from a wire protocol id.
pub const KAD_PROTOCOL_FAMILY: &str = "interweave/kad";
/// The role whose presence the wire protocol id implies.
pub const KAD_SERVER_ROLE: &str = "server";

/// Render a Kademlia server capability as the exact protocol string a
/// server advertises: `/interweave/kad/<wire_major>.0.0/<network_hash>`.
///
/// THE MAPPING DECISION (Stage 10 prerequisite, decided 2026-08-30 and
/// stated in `kademlia-integration.md` §7): a stored observation is
/// `(protocol_family, wire_major, network_hash, role)` and a
/// `ProtocolObservation` carries one `protocol_id`, so the four are
/// encoded AS the derived protocol string. `role = server` is implied
/// by presence — only a server advertises this protocol, and SPIKE-003
/// F17 measured that a walk never returns a client-mode peer at all —
/// and `<wire_major>.0.0` is the explicit generalisation of ADR-0047's
/// `1.0.0`: the minor and patch are always zero in the derived name,
/// because compatibility is decided on the major alone.
#[must_use]
pub fn kad_server_protocol_id(wire_major: u32, network_hash: &str) -> String {
    format!("/{KAD_PROTOCOL_FAMILY}/{wire_major}.0.0/{network_hash}")
}

/// Parse the reverse direction: an observed protocol id back into the
/// `(wire_major, network_hash)` pair, when — and only when — it is a
/// well-formed Kademlia server protocol for SOME network.
///
/// Exact grammar, not a prefix match: SPIKE-003's suite mutations showed
/// a prefix comparison lets a different network's evidence carry over.
/// The hash is 26 characters of lowercase RFC4648 base32 (16 bytes,
/// unpadded — the truncation is load-bearing per the frozen fixture),
/// and anything else is not this protocol, however close it looks.
#[must_use]
pub fn parse_kad_server_protocol_id(protocol_id: &str) -> Option<(u32, &str)> {
    let rest = protocol_id
        .strip_prefix('/')?
        .strip_prefix(KAD_PROTOCOL_FAMILY)?;
    let rest = rest.strip_prefix('/')?;
    let (version, hash) = rest.split_once('/')?;
    let major = version.strip_suffix(".0.0")?;
    // CANONICAL DIGITS ONLY. `01.0.0` and `1.0.0` are DIFFERENT strings
    // to libp2p, which matches protocol names by exact equality — so a
    // peer advertising the first does not speak the second, and taking
    // it as evidence for major 1 spends a targeted lookup on a peer
    // that will not answer. A leading zero is refused, which also
    // refuses major 0: the family starts at 1, as
    // `ProviderConfigError::ZeroWireMajor` says on the config side.
    if major.is_empty()
        || major.len() > 10
        || major.starts_with('0')
        || !major.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let major: u32 = major.parse().ok()?;
    if hash.len() != 26
        || !hash
            .bytes()
            .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
    {
        return None;
    }
    Some((major, hash))
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
    /// Bounded by the ENCLOSING record's expiry AND by each
    /// observation's own age. The record bound: a capability observed
    /// yesterday on a record that expired this morning is evidence
    /// about a peer this cache has stopped vouching for. The own-age
    /// bound: a record kept fresh by reachability alone must not
    /// republish an arbitrarily old observation — refreshing an address
    /// says nothing about what protocols the peer still serves.
    /// `capability_freshness_never_outlives_the_enclosing_record` and
    /// `a_refreshed_record_does_not_revive_an_aged_capability` each
    /// hold one bound.
    pub fn fresh_capabilities(
        &self,
        now_ms: u64,
        ttl_ms: u64,
    ) -> impl Iterator<Item = &ProtocolCapabilityObservation> {
        let record_fresh = self.is_fresh_at(now_ms, ttl_ms);
        self.capabilities
            .iter()
            .filter(move |c| record_fresh && now_ms < c.observed_at_ms.saturating_add(ttl_ms))
    }
}

#[cfg(test)]
mod mapping_tests {
    use super::{kad_server_protocol_id, parse_kad_server_protocol_id};

    #[test]
    fn the_renderer_and_parser_agree_and_the_grammar_is_exact() {
        let hash = "ssbtblqj7mexczivog5qfbfjvi";
        let id = kad_server_protocol_id(1, hash);
        assert_eq!(id, "/interweave/kad/1.0.0/ssbtblqj7mexczivog5qfbfjvi");
        assert_eq!(parse_kad_server_protocol_id(&id), Some((1, hash)));
        // A future major generalises the same way.
        assert_eq!(
            parse_kad_server_protocol_id(&kad_server_protocol_id(2, hash)),
            Some((2, hash))
        );

        // NOT this protocol, however close: each of these is one step
        // from the grammar, and a prefix match would take most of them.
        for wrong in [
            "/interweave/kad/1.0.0",                                     // no hash
            "/interweave/kad/1.0.1/ssbtblqj7mexczivog5qfbfjvi",          // patch not zero
            "/interweave/kad/1.1.0/ssbtblqj7mexczivog5qfbfjvi",          // minor not zero
            "/interweave/kad/x.0.0/ssbtblqj7mexczivog5qfbfjvi",          // major not digits
            "/interweave/kad/01.0.0/ssbtblqj7mexczivog5qfbfjvi",         // non-canonical major
            "/interweave/kad/0000000001.0.0/ssbtblqj7mexczivog5qfbfjvi", // ditto, padded
            "/interweave/kad/0.0.0/ssbtblqj7mexczivog5qfbfjvi",          // the family starts at 1
            "/interweave/kad/1.0.0/ssbtblqj7mexczivog5qfbfjv",           // 25-char hash
            "/interweave/kad/1.0.0/ssbtblqj7mexczivog5qfbfjviZ",         // bad charset
            "/interweave/kad/1.0.0/ssbtblqj7mexczivog5qfbfjv1",          // '1' not base32
            "/interweave/direct/2.0.0",                                  // other family
            "interweave/kad/1.0.0/ssbtblqj7mexczivog5qfbfjvi",           // no leading slash
        ] {
            assert_eq!(
                parse_kad_server_protocol_id(wrong),
                None,
                "{wrong} must not parse"
            );
        }
    }
}
