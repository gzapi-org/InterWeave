// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Discovery candidates, provider descriptors, and provider health.
//!
//! Discovery answers exactly one question: *which peers might be
//! reachable, and at which addresses?* It does not decide whether to
//! connect to them, whether they are trusted, or what any of their traffic
//! means (ADR-0006, ADR-0011, ADR-0012).
//!
//! **This crate does not depend on `trust-api`, and that is deliberate.**
//! An observation is evidence, never authority, so the types a provider
//! produces should not be able to reach a trust decision at all — if the
//! trust types are not in scope, a provider cannot accidentally consult or
//! mutate them. The one-way direction is the contract: `DiscoveryManager`
//! consults trust, providers never do.
//!
//! Two consequences show up in the shapes here:
//!
//! - a [`CandidatePeer`] carries `observed_at` and an optional
//!   `expires_at`, because reachability is perishable and a candidate with
//!   no freshness is indistinguishable from a fresh one;
//! - a [`ProtocolObservation`] records what a peer was *seen* supporting on
//!   an authenticated connection. It is not a capability grant, and the
//!   type says so rather than leaving a reader to infer it.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use interweave_transport_api::TransportIdentity;
use serde::{Deserialize, Serialize};

/// Maximum addresses retained per candidate.
pub const MAX_ADDRESSES: usize = 64;
/// Maximum protocol observations retained per candidate.
pub const MAX_PROTOCOL_OBSERVATIONS: usize = 16;
/// Maximum length of one opaque address string.
pub const MAX_ADDRESS_BYTES: usize = 256;

/// Longest protocol identifier, in bytes.
pub const MAX_PROTOCOL_ID_BYTES: usize = 256;
/// Maximum length of a provider name.
pub const MAX_PROVIDER_NAME_BYTES: usize = 64;

/// Why a candidate or descriptor was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryError {
    /// A bounded collection exceeded its cap.
    TooManyItems {
        /// Which collection.
        field: &'static str,
        /// Items supplied.
        got: usize,
        /// Items permitted.
        max: usize,
    },
    /// A bounded string was empty or too long.
    InvalidLength {
        /// Which field.
        field: &'static str,
        /// Bytes supplied.
        got: usize,
        /// Bytes permitted.
        max: usize,
    },
    /// `expires_at` was earlier than `observed_at`.
    ///
    /// A candidate that expired before it was seen is not merely odd; it
    /// would be treated as permanently stale, so a provider bug would look
    /// like a peer that is never reachable.
    ExpiryBeforeObservation {
        /// When it was observed.
        observed_at: u64,
        /// When it claims to expire.
        expires_at: u64,
    },
    /// A protocol identifier carried a byte outside printable ASCII.
    ///
    /// The schema says `^[\x20-\x7E]+$` and means it: these strings are
    /// compared exactly and never parsed, so control bytes and arbitrary
    /// UTF-8 buy nothing and give a provider room to smuggle content
    /// through a field nothing inspects.
    NonPrintableProtocolId {
        /// Byte offset of the first offending byte.
        at: usize,
    },
}

impl core::fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooManyItems { field, got, max } => {
                write!(f, "{field} has {got} items; the cap is {max}")
            }
            Self::InvalidLength { field, got, max } => {
                write!(f, "{field} is {got} bytes; the limit is 1..={max}")
            }
            Self::NonPrintableProtocolId { at } => write!(
                f,
                "a protocol_id byte at offset {at} is outside printable ASCII"
            ),
            Self::ExpiryBeforeObservation {
                observed_at,
                expires_at,
            } => write!(
                f,
                "expires_at {expires_at} precedes observed_at {observed_at}"
            ),
        }
    }
}

impl core::error::Error for DiscoveryError {}

/// A transport fact observed on an **authenticated** connection.
///
/// An opaque protocol identifier: 1..=256 printable ASCII bytes.
///
/// A TYPE rather than a check, because the check was the thing that was
/// missing. `CandidatePeer::validate` counted observations and looked
/// inside none of them, so a provider could park sixteen strings of any
/// length and any byte content in another node's bounded cache. A
/// validated field cannot be constructed past the boundary that
/// validates it, which is a different guarantee from one every future
/// call site has to remember.
///
/// Compared exactly and never parsed for meaning, which is why the
/// grammar is this narrow: control bytes and arbitrary UTF-8 buy a
/// provider nothing except room to carry content through a field nothing
/// inspects.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ProtocolId(String);

impl ProtocolId {
    /// Parse a protocol identifier.
    ///
    /// # Errors
    /// Returns [`DiscoveryError::InvalidLength`] when empty or longer
    /// than [`MAX_PROTOCOL_ID_BYTES`], or
    /// [`DiscoveryError::NonPrintableProtocolId`] for a byte outside
    /// `0x20..=0x7E`.
    pub fn parse(value: impl Into<String>) -> Result<Self, DiscoveryError> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_PROTOCOL_ID_BYTES {
            return Err(DiscoveryError::InvalidLength {
                field: "protocol_observations[].protocol_id",
                got: value.len(),
                max: MAX_PROTOCOL_ID_BYTES,
            });
        }
        if let Some(at) = value.bytes().position(|b| !(0x20..=0x7E).contains(&b)) {
            return Err(DiscoveryError::NonPrintableProtocolId { at });
        }
        Ok(Self(value))
    }

    /// The identifier as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ProtocolId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // The wire path goes through the same parse. A derived
        // `Deserialize` on the inner `String` is exactly how a validated
        // type ends up more permissive than the boundary it names.
        Self::parse(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// Advisory only. "This peer was seen speaking protocol X" is not "this
/// peer may use protocol X here": authorization is the trust policy's
/// answer, and a routing decision that consulted this instead would let a
/// peer advertise its way into a role.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
// CLOSED, like the candidate that carries it. The item schema declares
// `additionalProperties: false`, and an observation is exactly the
// bounded advisory record a provider would be tempted to hang an
// EndpointId or a role off.
#[serde(deny_unknown_fields)]
pub struct ProtocolObservation {
    /// The exact protocol string observed, e.g. an Identify entry.
    pub protocol_id: ProtocolId,
    /// Whether the peer was observed supporting it.
    pub supported: bool,
    /// Local millisecond timestamp of the observation.
    pub observed_at: u64,
}

/// A peer that might be reachable, with the addresses to try.
///
/// Produced by providers, consumed only by `DiscoveryManager`. Holding one
/// implies nothing about trust, reachability now, or willingness to talk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// CLOSED, because this schema's own description says so:
// `additionalProperties: false` is what stops an EndpointId, a ChannelId,
// a membership record, or a presence flag being added to a candidate
// "as an obvious convenience". Discovery is exactly where such a field
// would leak routing or presence to anyone who can query, and the type
// that was relied on to refuse one was accepting it.
#[serde(deny_unknown_fields)]
pub struct CandidatePeer {
    /// The peer this candidate is about.
    pub peer_id: TransportIdentity,
    /// Opaque reachability addresses.
    ///
    /// Opaque on purpose: a multiaddr is a backend concept, and parsing it
    /// here would put libp2p's address grammar into a neutral contract.
    /// A `BTreeSet` because the schema declares a set — duplicates would
    /// consume the cap without adding information.
    ///
    /// Deserialized through the wire sequence rather than straight into
    /// the set: collecting first destroys the duplicate, and `validate`
    /// would then count one address where sixty-five arrived.
    #[serde(deserialize_with = "wire_address_set")]
    pub addresses: BTreeSet<String>,
    /// Which provider observed it.
    pub source: String,
    /// Local millisecond timestamp of the observation.
    pub observed_at: u64,
    /// When the observation stops being usable, if the provider knows.
    ///
    /// `None` means the provider does not express expiry, not that the
    /// candidate never expires. `DiscoveryManager` applies its own bound in
    /// that case rather than treating the observation as permanent.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "absent_or_u64"
    )]
    pub expires_at: Option<u64>,
    /// Bounded advisory protocol facts.
    ///
    /// Judged as it arrived, for the same reason `addresses` is: the
    /// `uniqueItems` and `maxItems` in the schema are properties of the
    /// ARRAY, and collecting into a set during deserialization erases
    /// both before anything can check them.
    #[serde(
        default,
        skip_serializing_if = "BTreeSet::is_empty",
        deserialize_with = "wire_observation_set"
    )]
    pub protocol_observations: BTreeSet<ProtocolObservation>,
}

/// Read the wire array, judge it as it arrived, then collect.
///
/// `uniqueItems: true` and `maxItems: 64` are properties of the ARRAY.
/// Collecting into a `BTreeSet` during deserialization erases both, and
/// the check in [`CandidatePeer::validate`] then runs against a
/// collection that no longer resembles what was sent.
fn wire_address_set<'de, D>(deserializer: D) -> Result<BTreeSet<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    let items = Vec::<String>::deserialize(deserializer)?;
    if items.len() > MAX_ADDRESSES {
        return Err(D::Error::custom(format!(
            "at most {MAX_ADDRESSES} addresses, got {}",
            items.len()
        )));
    }
    let count = items.len();
    let set: BTreeSet<String> = items.into_iter().collect();
    if set.len() != count {
        return Err(D::Error::custom("addresses must be unique on the wire"));
    }
    Ok(set)
}

/// Read the wire array of observations, judge it, then collect.
///
/// The mirror of [`wire_address_set`], and needed for the same reason:
/// seventeen observations or two identical ones both become a set that
/// no longer resembles what was sent, and every later check then runs
/// against the wrong collection.
fn wire_observation_set<'de, D>(deserializer: D) -> Result<BTreeSet<ProtocolObservation>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    let items = Vec::<ProtocolObservation>::deserialize(deserializer)?;
    if items.len() > MAX_PROTOCOL_OBSERVATIONS {
        return Err(D::Error::custom(format!(
            "at most {MAX_PROTOCOL_OBSERVATIONS} protocol observations, got {}",
            items.len()
        )));
    }
    let count = items.len();
    let set: BTreeSet<ProtocolObservation> = items.into_iter().collect();
    if set.len() != count {
        return Err(D::Error::custom(
            "protocol observations must be unique on the wire",
        ));
    }
    Ok(set)
}

/// An optional integer that may be ABSENT but never explicitly `null`.
fn absent_or_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    Option::<u64>::deserialize(deserializer)?
        .map(Some)
        .ok_or_else(|| D::Error::custom("must be an integer or omitted entirely, not null"))
}

impl CandidatePeer {
    /// Check every bound the contract states.
    ///
    /// Separate from construction because a candidate arrives as a whole
    /// from a provider, and reporting *which* bound failed is more useful
    /// than refusing to build the value.
    ///
    /// # Errors
    /// Returns [`DiscoveryError`] for an over-cap collection, an
    /// out-of-range string, or an expiry preceding the observation.
    pub fn validate(&self) -> Result<(), DiscoveryError> {
        if self.addresses.len() > MAX_ADDRESSES {
            return Err(DiscoveryError::TooManyItems {
                field: "addresses",
                got: self.addresses.len(),
                max: MAX_ADDRESSES,
            });
        }
        for a in &self.addresses {
            if a.is_empty() || a.len() > MAX_ADDRESS_BYTES {
                return Err(DiscoveryError::InvalidLength {
                    field: "addresses[]",
                    got: a.len(),
                    max: MAX_ADDRESS_BYTES,
                });
            }
        }
        if self.protocol_observations.len() > MAX_PROTOCOL_OBSERVATIONS {
            return Err(DiscoveryError::TooManyItems {
                field: "protocol_observations",
                got: self.protocol_observations.len(),
                max: MAX_PROTOCOL_OBSERVATIONS,
            });
        }
        // Each identifier's bounds are [`ProtocolId`]'s to keep — the cap
        // used to be enforced on the collection and on nothing inside it,
        // and a validated type is what makes that unreachable rather than
        // remembered.
        if self.source.is_empty() || self.source.len() > MAX_PROVIDER_NAME_BYTES {
            return Err(DiscoveryError::InvalidLength {
                field: "source",
                got: self.source.len(),
                max: MAX_PROVIDER_NAME_BYTES,
            });
        }
        if let Some(expires_at) = self.expires_at
            && expires_at < self.observed_at
        {
            return Err(DiscoveryError::ExpiryBeforeObservation {
                observed_at: self.observed_at,
                expires_at,
            });
        }
        Ok(())
    }

    /// Whether this candidate is still fresh at `now_ms`.
    ///
    /// A candidate with no stated expiry is **not** treated as eternally
    /// fresh; it returns `true` here and the manager applies its own bound.
    /// This function answers only the question the provider expressed.
    #[must_use]
    pub fn is_fresh_at(&self, now_ms: u64) -> bool {
        self.expires_at.is_none_or(|e| now_ms < e)
    }
}

/// Where a provider looks for peers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderScope {
    /// The local link, e.g. mDNS.
    Local,
    /// Operator-configured entries.
    Configured,
    /// The wider network, e.g. a DHT.
    Network,
}

/// Whether a provider waits for observations or goes looking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderMode {
    /// Only reports what arrives unprompted.
    Passive,
    /// Actively queries.
    Active,
    /// Both.
    Mixed,
}

/// One provider's own health.
///
/// A provider failure changes this and must not terminate unrelated
/// providers — and the transport can be healthy while discovery is
/// degraded, because peers already connected do not need discovering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderHealth {
    /// Operating normally.
    Healthy,
    /// Operating with reduced capability.
    Degraded,
    /// Not operating.
    Unavailable,
}

/// What a provider declares about itself at registration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDescriptor {
    /// Stable provider name, also used as `CandidatePeer::source`.
    pub name: String,
    /// The provider-interface version this implements, `major.minor`.
    pub interface_version: String,
    /// The provider's own configuration version, if it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_version: Option<String>,
    /// Where it looks.
    pub scope: ProviderScope,
    /// How it looks.
    pub mode: ProviderMode,
    /// Whether it can express candidate expiry.
    pub supports_expiry: bool,
    /// Whether it accepts lookup hints.
    pub supports_hints: bool,
}

impl ProviderDescriptor {
    /// Check the bounds and the `major.minor` version shapes.
    ///
    /// # Errors
    /// Returns [`DiscoveryError::InvalidLength`] for an out-of-range name
    /// or a version that is not `digits.digits`.
    pub fn validate(&self) -> Result<(), DiscoveryError> {
        if self.name.is_empty() || self.name.len() > MAX_PROVIDER_NAME_BYTES {
            return Err(DiscoveryError::InvalidLength {
                field: "name",
                got: self.name.len(),
                max: MAX_PROVIDER_NAME_BYTES,
            });
        }
        for (field, value) in [
            ("interface_version", Some(&self.interface_version)),
            ("config_version", self.config_version.as_ref()),
        ] {
            let Some(v) = value else { continue };
            if !is_major_minor(v) {
                return Err(DiscoveryError::InvalidLength {
                    field,
                    got: v.len(),
                    max: 0,
                });
            }
        }
        Ok(())
    }
}

/// `^[0-9]+\.[0-9]+$`
fn is_major_minor(value: &str) -> bool {
    let mut parts = value.split('.');
    let (Some(major), Some(minor), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let numeric = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    numeric(major) && numeric(minor)
}

/// What a provider emits to `DiscoveryManager`.
///
/// An event stream, not a query interface: a provider reports what it
/// learned and never asks whether to act on it. There is deliberately no
/// variant meaning "connect to this peer" — providers never dial
/// (ADR-0006, ADR-0011).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
pub enum DiscoveryEvent {
    /// A candidate was observed or refreshed.
    CandidateObserved {
        /// The candidate.
        candidate: Box<CandidatePeer>,
    },
    /// A previously reported candidate is gone or expired.
    ///
    /// Advisory like everything else: it does not disconnect anything, and
    /// an established connection to that peer is unaffected.
    CandidateExpired {
        /// Which peer.
        peer_id: TransportIdentity,
        /// Which provider is retracting it.
        source: String,
        /// Which of that provider's addresses expired.
        ///
        /// Empty retracts the whole `(peer_id, source)` candidate, which
        /// is what a provider with no per-address lifetime means.
        ///
        /// Present because ADR-0007 makes expiry per source AND address:
        /// one provider may report several addresses with independent
        /// lifetimes, and `DISCOVERY.md` gives `Expired` an optional
        /// `addresses` for exactly that. Without the selector the manager
        /// had a choice of two wrong answers — drop addresses that are
        /// still valid, or keep one that has expired — and no way to
        /// express what the provider actually observed.
        #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
        addresses: BTreeSet<String>,
    },
    /// The provider's own health changed.
    HealthChanged {
        /// Which provider.
        source: String,
        /// Its new health.
        health: ProviderHealth,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    const P1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

    fn peer() -> TransportIdentity {
        TransportIdentity::parse(P1).expect("valid test identity")
    }

    fn candidate() -> CandidatePeer {
        CandidatePeer {
            peer_id: peer(),
            addresses: ["/ip4/192.0.2.1/tcp/4001".to_owned()].into_iter().collect(),
            source: "mdns".to_owned(),
            observed_at: 1_000,
            expires_at: Some(2_000),
            protocol_observations: BTreeSet::new(),
        }
    }

    #[test]
    fn a_well_formed_candidate_validates() {
        assert_eq!(candidate().validate(), Ok(()));
    }

    #[test]
    fn every_protocol_identifier_is_bounded_not_just_their_number() {
        // The cap used to be enforced on the collection and on nothing
        // inside it, so a provider could park sixteen strings of any
        // length in another node's bounded cache. It is the type's now,
        // which makes the invalid value unconstructible rather than
        // caught by a check every future call site has to remember.
        assert!(matches!(
            ProtocolId::parse("x".repeat(MAX_PROTOCOL_ID_BYTES + 1)),
            Err(DiscoveryError::InvalidLength { got, max, .. })
                if got == MAX_PROTOCOL_ID_BYTES + 1 && max == MAX_PROTOCOL_ID_BYTES
        ));
        assert!(matches!(
            ProtocolId::parse(""),
            Err(DiscoveryError::InvalidLength { got: 0, .. })
        ));
        assert!(ProtocolId::parse("/interweave/direct/2.0.0").is_ok());
        assert!(ProtocolId::parse("x".repeat(MAX_PROTOCOL_ID_BYTES)).is_ok());
    }

    #[test]
    fn a_protocol_identifier_outside_printable_ascii_is_refused() {
        // These strings are compared exactly and never parsed, so a
        // control byte or arbitrary UTF-8 buys a provider nothing except
        // room to carry content through a field nothing inspects.
        for (id, at) in [("\u{1}nope", 0), ("ok\u{7f}", 2), ("caf\u{e9}", 3)] {
            assert_eq!(
                ProtocolId::parse(id),
                Err(DiscoveryError::NonPrintableProtocolId { at }),
                "{id:?} must be refused"
            );
        }
    }

    #[test]
    fn the_wire_path_goes_through_the_same_parse() {
        // A derived `Deserialize` on the inner String is exactly how a
        // validated type ends up more permissive than the boundary it
        // names — the recurring defect this crate has now hit twice.
        let over = "x".repeat(MAX_PROTOCOL_ID_BYTES + 1);
        let doc = format!(
            r#"{{"peer_id":"{P1}","addresses":["/a"],"source":"s","observed_at":1,"protocol_observations":[{{"protocol_id":"{over}","supported":true,"observed_at":1}}]}}"#
        );
        assert!(
            serde_json::from_str::<CandidatePeer>(&doc).is_err(),
            "an over-length protocol id must not deserialize"
        );

        let doc = format!(
            r#"{{"peer_id":"{P1}","addresses":["/a"],"source":"s","observed_at":1,"protocol_observations":[{{"protocol_id":"café","supported":true,"observed_at":1}}]}}"#
        );
        assert!(
            serde_json::from_str::<CandidatePeer>(&doc).is_err(),
            "a non-ASCII protocol id must not deserialize"
        );

        let doc = format!(
            r#"{{"peer_id":"{P1}","addresses":["/a"],"source":"s","observed_at":1,"protocol_observations":[{{"protocol_id":"/interweave/direct/2.0.0","supported":true,"observed_at":1}}]}}"#
        );
        let parsed = serde_json::from_str::<CandidatePeer>(&doc).expect("a legal one still parses");
        assert_eq!(parsed.validate(), Ok(()));
    }

    #[test]
    fn observations_are_judged_as_the_array_that_arrived() {
        // `uniqueItems` and `maxItems` are properties of the ARRAY.
        // Collecting into a set first destroys the duplicate and shrinks
        // the count, so both checks then run against a collection that
        // no longer resembles what was sent.
        let one = r#"{"protocol_id":"a","supported":true,"observed_at":1}"#;
        let dup = format!(
            r#"{{"peer_id":"{P1}","addresses":["/a"],"source":"s","observed_at":1,"protocol_observations":[{one},{one}]}}"#
        );
        assert!(
            serde_json::from_str::<CandidatePeer>(&dup).is_err(),
            "a duplicate observation must not collapse into a valid set"
        );

        let many: Vec<String> = (0..=MAX_PROTOCOL_OBSERVATIONS)
            .map(|i| format!(r#"{{"protocol_id":"p{i}","supported":true,"observed_at":1}}"#))
            .collect();
        let over = format!(
            r#"{{"peer_id":"{P1}","addresses":["/a"],"source":"s","observed_at":1,"protocol_observations":[{}]}}"#,
            many.join(",")
        );
        assert!(
            serde_json::from_str::<CandidatePeer>(&over).is_err(),
            "more observations than the cap must be refused on the wire"
        );
    }

    #[test]
    fn an_observation_is_a_closed_object() {
        // The item schema says `additionalProperties: false`, and an
        // observation is exactly the bounded advisory record someone
        // would hang an EndpointId or a role off.
        let doc = format!(
            r#"{{"peer_id":"{P1}","addresses":["/a"],"source":"s","observed_at":1,"protocol_observations":[{{"protocol_id":"a","supported":true,"observed_at":1,"endpoint":"human"}}]}}"#
        );
        assert!(
            serde_json::from_str::<CandidatePeer>(&doc).is_err(),
            "an unknown property on an observation must be refused"
        );
    }

    #[test]
    fn an_expiry_can_name_the_addresses_it_retracts() {
        // ADR-0007 makes expiry per source AND address. One provider may
        // report several addresses with independent lifetimes, so an
        // event that can only retract the whole (peer_id, source) leaves
        // the manager choosing between dropping still-valid addresses and
        // keeping an expired one.
        let scoped = DiscoveryEvent::CandidateExpired {
            peer_id: peer(),
            source: "mdns".to_owned(),
            addresses: ["/ip4/192.0.2.1/tcp/4001".to_owned()].into_iter().collect(),
        };
        let json = serde_json::to_string(&scoped).expect("serializes");
        assert!(
            json.contains("\"addresses\""),
            "the selector reaches the wire"
        );
        assert_eq!(
            serde_json::from_str::<DiscoveryEvent>(&json).expect("round trips"),
            scoped
        );

        // Absent means the whole candidate, which is what a provider with
        // no per-address lifetime means — and stays the wire shape it was.
        let whole = DiscoveryEvent::CandidateExpired {
            peer_id: peer(),
            source: "mdns".to_owned(),
            addresses: BTreeSet::new(),
        };
        let json = serde_json::to_string(&whole).expect("serializes");
        assert!(
            !json.contains("addresses"),
            "an empty selector is omitted, not sent as []"
        );
        assert_eq!(
            serde_json::from_str::<DiscoveryEvent>(&json).expect("round trips"),
            whole
        );
    }

    #[test]
    fn candidate_collections_are_bounded() {
        let mut c = candidate();
        c.addresses = (0..=MAX_ADDRESSES).map(|i| format!("/addr/{i}")).collect();
        assert_eq!(
            c.validate(),
            Err(DiscoveryError::TooManyItems {
                field: "addresses",
                got: MAX_ADDRESSES + 1,
                max: MAX_ADDRESSES,
            })
        );

        let mut c = candidate();
        c.protocol_observations = (0..=MAX_PROTOCOL_OBSERVATIONS)
            .map(|i| ProtocolObservation {
                protocol_id: ProtocolId::parse(format!("/p/{i}")).expect("valid"),
                supported: true,
                observed_at: 1,
            })
            .collect();
        assert!(matches!(
            c.validate(),
            Err(DiscoveryError::TooManyItems {
                field: "protocol_observations",
                ..
            })
        ));
    }

    #[test]
    fn an_expiry_before_the_observation_is_rejected() {
        // Otherwise a provider bug reads as a peer that is never reachable.
        let mut c = candidate();
        c.observed_at = 5_000;
        c.expires_at = Some(4_999);
        assert_eq!(
            c.validate(),
            Err(DiscoveryError::ExpiryBeforeObservation {
                observed_at: 5_000,
                expires_at: 4_999,
            })
        );
        // Equal is allowed: an observation valid for zero milliseconds is
        // strange but not contradictory, and it expires immediately.
        c.expires_at = Some(5_000);
        assert_eq!(c.validate(), Ok(()));
        assert!(!c.is_fresh_at(5_000));
    }

    #[test]
    fn freshness_reflects_only_what_the_provider_expressed() {
        let c = candidate();
        assert!(c.is_fresh_at(1_999));
        assert!(!c.is_fresh_at(2_000));

        // No stated expiry is not "eternally fresh" as a policy; this
        // function reports what the provider said, and the manager bounds
        // the rest.
        let mut c = candidate();
        c.expires_at = None;
        assert!(c.is_fresh_at(u64::MAX));
    }

    #[test]
    fn addresses_stay_opaque_and_deduplicated() {
        let mut c = candidate();
        c.addresses.insert("/ip4/192.0.2.1/tcp/4001".to_owned());
        // A set: re-inserting the same address adds nothing and cannot eat
        // into the cap.
        assert_eq!(c.addresses.len(), 1);
        // Nothing here parses the address; it is a backend concept.
        c.addresses.insert("not-a-multiaddr-at-all".to_owned());
        assert_eq!(c.validate(), Ok(()));
    }

    #[test]
    fn provider_descriptors_validate_their_version_shapes() {
        let d = ProviderDescriptor {
            name: "mdns".to_owned(),
            interface_version: "1.0".to_owned(),
            config_version: None,
            scope: ProviderScope::Local,
            mode: ProviderMode::Passive,
            supports_expiry: true,
            supports_hints: false,
        };
        assert_eq!(d.validate(), Ok(()));

        for bad in ["1", "1.", ".1", "1.0.0", "v1.0", "", "a.b"] {
            let mut d = d.clone();
            d.interface_version = bad.to_owned();
            assert!(d.validate().is_err(), "{bad:?} should not validate");
        }

        let mut d = d.clone();
        d.name = String::new();
        assert!(d.validate().is_err());
    }

    #[test]
    fn events_serialize_with_their_discriminant() {
        let e = DiscoveryEvent::HealthChanged {
            source: "mdns".to_owned(),
            health: ProviderHealth::Degraded,
        };
        let json = serde_json::to_value(&e).expect("ser");
        assert_eq!(json["event"], "health_changed");
        assert_eq!(json["health"], "degraded");
        assert_eq!(
            serde_json::from_value::<DiscoveryEvent>(json).expect("de"),
            e
        );
    }

    #[test]
    fn a_candidate_round_trips_through_json() {
        let c = candidate();
        let json = serde_json::to_value(&c).expect("ser");
        assert_eq!(json["source"], "mdns");
        // An empty observation set is omitted rather than serialized empty.
        assert!(json.get("protocol_observations").is_none());
        assert_eq!(
            serde_json::from_value::<CandidatePeer>(json).expect("de"),
            c
        );
    }
    #[test]
    fn a_candidate_refuses_the_fields_discovery_must_never_carry() {
        // The schema is closed precisely so an EndpointId or a presence
        // flag cannot be added to a candidate as a convenience. The type
        // has to refuse them or that reasoning is decoration.
        for extra in [
            r#""endpoint_id":"human""#,
            r#""channel_id":"c1""#,
            r#""presence":"online""#,
            r#""trusted":true"#,
        ] {
            let json = format!(
                r#"{{"peer_id":"{P1}","addresses":["/ip4/10.0.0.1/tcp/4001"],
                "source":"peer-cache","observed_at":1,{extra}}}"#
            );
            assert!(
                serde_json::from_str::<CandidatePeer>(&json).is_err(),
                "discovery must refuse {extra}"
            );
        }
    }

    #[test]
    fn duplicate_wire_addresses_are_refused_before_the_set_hides_them() {
        // Sixty-five duplicates collapse to one member, and validate()
        // would then count one address where sixty-five arrived.
        let many = vec![r#""/ip4/10.0.0.1/tcp/4001""#; MAX_ADDRESSES + 1].join(",");
        let json = format!(
            r#"{{"peer_id":"{P1}","addresses":[{many}],
            "source":"peer-cache","observed_at":1}}"#
        );
        assert!(serde_json::from_str::<CandidatePeer>(&json).is_err());

        let two = r#""/ip4/10.0.0.1/tcp/4001","/ip4/10.0.0.1/tcp/4001""#;
        let json = format!(
            r#"{{"peer_id":"{P1}","addresses":[{two}],
            "source":"peer-cache","observed_at":1}}"#
        );
        assert!(
            serde_json::from_str::<CandidatePeer>(&json).is_err(),
            "a duplicate within the cap is still a duplicate"
        );
    }

    #[test]
    fn an_explicit_null_expiry_is_not_absence() {
        // `None` means the provider does not express expiry. An explicit
        // null is a value the schema does not permit, and reading it as
        // "no expiry" would let a malformed record become a permanent one.
        let json = format!(
            r#"{{"peer_id":"{P1}","addresses":["/ip4/10.0.0.1/tcp/4001"],
            "source":"peer-cache","observed_at":1,"expires_at":null}}"#
        );
        assert!(serde_json::from_str::<CandidatePeer>(&json).is_err());
    }
}
