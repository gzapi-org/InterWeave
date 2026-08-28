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
        // BOUNDED IN THE VISITOR, then parsed. `String::deserialize`
        // here materialised the whole token before `parse` could refuse
        // it, so a single enormous `protocol_id` allocated in full --
        // the streaming array visitor above capped the number of
        // elements and nothing capped one element. The derived path is
        // exactly how a validated type ends up more permissive than the
        // boundary it names, which is why neither half is derived now.
        let raw = bounded_string(d, "protocol_id", MAX_PROTOCOL_ID_BYTES)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
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
    #[serde(deserialize_with = "wire_provider_name")]
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
    /// Stops at `MAX_ADDRESSES + 1` instead of materializing the input.
    ///
    /// `Vec::<String>::deserialize` parses and allocates the WHOLE
    /// array before any length check can run, so a ceiling applied
    /// afterwards rejects the result while the input has already been
    /// paid for -- a semantic limit rather than the resource limit it
    /// exists to be. One element past the limit is enough to know, and
    /// it is the only element past the limit this ever holds.
    ///
    /// Each address is length-checked BEFORE it is materialized, not
    /// after: `next_element::<String>` allocates and parses the whole
    /// value first, so a check on the result rejects it while the
    /// memory has already been taken -- the same collect-then-count
    /// mistake as the array itself, one level down. `BoundedAddress`
    /// refuses inside `visit_str`, so `MAX_ADDRESSES * MAX_ADDRESS_BYTES`
    /// is a bound on what this can be made to hold rather than on what
    /// it will keep.
    struct Bounded;

    impl<'de> serde::de::Visitor<'de> for Bounded {
        type Value = BTreeSet<String>;

        fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "at most {MAX_ADDRESSES} unique addresses")
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            use serde::de::Error as _;
            let mut out = BTreeSet::new();
            // COUNTED, not measured by the set. Duplicates collapse, and
            // the ceiling is on the array that was sent.
            let mut count = 0_usize;
            while let Some(BoundedAddress(address)) = seq.next_element::<BoundedAddress>()? {
                count = count.saturating_add(1);
                if count > MAX_ADDRESSES {
                    return Err(A::Error::custom(format!(
                        "at most {MAX_ADDRESSES} addresses, got more"
                    )));
                }
                out.insert(address);
            }
            if out.len() != count {
                return Err(A::Error::custom("addresses must be unique on the wire"));
            }
            Ok(out)
        }
    }

    deserializer.deserialize_seq(Bounded)
}

/// One address, refused before *this crate* owns it.
///
/// `String`'s own `Deserialize` allocates whatever arrives and hands it
/// over; a length check after that has already paid for the input it
/// rejects. This refuses inside the visitor, so nothing of ours holds
/// the value.
///
/// **What this does NOT bound, stated because the first version of this
/// comment claimed it did.** A visitor runs after the deserializer has
/// produced a `&str`. For an unescaped token `serde_json` borrows
/// straight from the input and allocates nothing; for a token carrying
/// escapes it must unescape into its own scratch buffer first, and that
/// allocation happens whatever this visitor later decides. No `Visitor`
/// hook runs earlier than that, so `MAX_ADDRESS_BYTES` cannot be the
/// ceiling on what a *deserializer* materialises -- only on what is
/// retained past it.
///
/// The ceiling on materialised input therefore belongs where the bytes
/// are read, and it exists there: the peer cache refuses a file over
/// `MAX_CACHE_FILE_BYTES` before parsing any of it, and the IPC v2 frame
/// codec refuses an over-ceiling declared length before allocating. This
/// crate is types and validation with no I/O, so a bound on the reader
/// is not its to impose.
/// Refuse an over-long string INSIDE the visitor, for any bounded field.
///
/// WHY THIS IS A HELPER AND NOT A THIRD COPY. `BoundedAddress` closed
/// this for addresses and the sibling fields in the same module kept the
/// derived path: `ProtocolId::deserialize` called `String::deserialize`
/// and checked `MAX_PROTOCOL_ID_BYTES` afterwards, and `source`/`name`
/// were plain `String` fields whose bound lived only in `validate`, which
/// runs after the whole record is materialised. Fixing the instance and
/// documenting it left two more, which is how the first one arrived.
///
/// The same caveat as `BoundedAddress` applies and is not repeated at
/// each call site: a visitor runs after the deserializer has produced a
/// `&str`, so this bounds what is RETAINED, never what a deserializer
/// materialises. The ceiling on materialised input belongs where the
/// bytes are read.
fn visit_bounded<E: serde::de::Error>(
    value: &str,
    field: &'static str,
    max: usize,
) -> Result<String, E> {
    if value.len() > max {
        return Err(E::custom(format!(
            "{field} must be at most {max} bytes, got {}",
            value.len()
        )));
    }
    Ok(value.to_owned())
}

/// `serde` adaptor for a bounded string field, used via
/// `#[serde(deserialize_with = ...)]` where the field is a plain
/// `String` and a newtype would change the public shape.
fn bounded_string<'de, D: serde::Deserializer<'de>>(
    d: D,
    field: &'static str,
    max: usize,
) -> Result<String, D::Error> {
    struct Bounded(&'static str, usize);

    impl serde::de::Visitor<'_> for Bounded {
        type Value = String;

        fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "{} of at most {} bytes", self.0, self.1)
        }

        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
            visit_bounded(value, self.0, self.1)
        }

        /// A deserializer that already owns the buffer hands it over
        /// here. Checking before taking it means the owned value is
        /// dropped rather than kept.
        fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
            self.visit_str(&value)
        }
    }

    d.deserialize_str(Bounded(field, max))
}

/// `source` on a candidate: bounded in the visitor, not in `validate`.
fn wire_provider_name<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    bounded_string(d, "source", MAX_PROVIDER_NAME_BYTES)
}

/// `name` on a descriptor. Same bound, named for its own field so the
/// error says which one was too long.
fn wire_descriptor_name<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    bounded_string(d, "name", MAX_PROVIDER_NAME_BYTES)
}

struct BoundedAddress(String);

impl<'de> Deserialize<'de> for BoundedAddress {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Bounded;

        impl serde::de::Visitor<'_> for Bounded {
            type Value = BoundedAddress;

            fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, "an address of at most {MAX_ADDRESS_BYTES} bytes")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                // Through the shared helper, so there is ONE bounded
                // string rule in this module rather than one per field.
                // Three fields drifted apart the last time there were
                // two copies of it.
                visit_bounded(value, "address", MAX_ADDRESS_BYTES).map(BoundedAddress)
            }

            /// A deserializer that already owns the buffer hands it over
            /// here. Checking before taking it means the owned value is
            /// dropped rather than kept, which is the most this side can
            /// do once the other side has allocated.
            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
                self.visit_str(&value)
            }
        }

        deserializer.deserialize_str(Bounded)
    }
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
    /// The mirror of [`wire_address_set`]'s visitor, bounded for the
    /// same reason: materializing the array first pays for input the
    /// ceiling exists to refuse. Each observation's own `protocol_id`
    /// is length-checked by [`ProtocolId::parse`] during element
    /// deserialization, so the per-element bound is already in place
    /// here and only the count needs stopping early.
    struct Bounded;

    impl<'de> serde::de::Visitor<'de> for Bounded {
        type Value = BTreeSet<ProtocolObservation>;

        fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(
                f,
                "at most {MAX_PROTOCOL_OBSERVATIONS} unique protocol observations"
            )
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            use serde::de::Error as _;
            let mut out = BTreeSet::new();
            let mut count = 0_usize;
            while let Some(observation) = seq.next_element::<ProtocolObservation>()? {
                count = count.saturating_add(1);
                if count > MAX_PROTOCOL_OBSERVATIONS {
                    return Err(A::Error::custom(format!(
                        "at most {MAX_PROTOCOL_OBSERVATIONS} protocol observations, got more"
                    )));
                }
                out.insert(observation);
            }
            if out.len() != count {
                return Err(A::Error::custom(
                    "protocol observations must be unique on the wire",
                ));
            }
            Ok(out)
        }
    }

    deserializer.deserialize_seq(Bounded)
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
    #[serde(deserialize_with = "wire_descriptor_name")]
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

/// A reachability or capability fact handed DOWN to a provider.
///
/// The hint path is `ConnectionManager -> TransportRuntime -> provider`
/// (ADR-0027): the runtime learns something from an authenticated
/// connection and offers it to whichever provider persists such things. A
/// hint is evidence, never authority — accepting one grants no trust and
/// creates no obligation to dial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerHint {
    /// This peer answered at this address.
    ObservedReachable {
        /// Who answered.
        peer_id: TransportIdentity,
        /// Where, opaque like every other address here.
        address: String,
        /// When, in local milliseconds.
        observed_at: u64,
    },
    /// This peer was seen supporting (or not supporting) a protocol.
    ObservedProtocol {
        /// About whom.
        peer_id: TransportIdentity,
        /// Which protocol.
        protocol_id: ProtocolId,
        /// Whether it was supported.
        supported: bool,
        /// When, in local milliseconds.
        observed_at: u64,
    },
    /// A whole candidate observed elsewhere.
    CandidateHint(Box<CandidatePeer>),
}

/// What a provider did with a hint.
///
/// `Unsupported` is a REQUIRED answer, not a courtesy: `DISCOVERY.md` says
/// a provider must reject a hint class it does not handle explicitly
/// rather than silently taking ownership of it, because a provider that
/// quietly accepts is a provider drifting into owning connection policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HintDisposition {
    /// Taken, and it may influence later events.
    Accepted,
    /// This provider does not handle this hint class. Not an error.
    Unsupported,
    /// Handled in principle, but this one was malformed.
    Rejected(DiscoveryError),
}

/// Why a provider refused a lifecycle call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// `start` was called twice.
    AlreadyStarted,
    /// The call needs a started provider.
    NotStarted,
    /// The provider's own mechanism failed.
    ///
    /// Carries a short static reason rather than a formatted string: a
    /// provider's failure text ends up in health diagnostics, and an
    /// unbounded message from a remote-influenced code path is a log
    /// injection waiting to be written.
    Failed(&'static str),
}

/// A source of candidate peers.
///
/// `DISCOVERY.md` sketches this as an async trait returning a stream and
/// says so non-normatively — "the behavioral contract below is
/// normative". This is the pull-shaped equivalent, and the shape is
/// deliberate: this crate must stay free of any runtime, so the async
/// character belongs to whoever owns the provider's I/O, and
/// [`drain_events`](Self::drain_events) is the bounded batch that
/// contract requires. Every normative rule survives the translation:
///
/// - **no events before `start`** — `drain_events` returns empty until
///   then (`provider_starts_cleanly`);
/// - **bounded batches** — the caller states `max` and the provider must
///   respect it (`provider_respects_state_bounds`);
/// - **deterministic termination** — after `shutdown` the drain is empty
///   forever (`provider_event_stream_closes_after_shutdown`);
/// - **cooperative, idempotent shutdown**
///   (`provider_shutdown_is_idempotent_and_bounded`);
/// - **explicit hint refusal** — [`HintDisposition::Unsupported`];
/// - **failures become health, not panics** — every method is total
///   except the two lifecycle calls, which return [`ProviderError`].
///
/// A provider never dials, never consults or mutates trust, never touches
/// a Swarm, and never sends application traffic. Those absences are
/// structural here: nothing in this crate can reach any of them.
pub trait DiscoveryProvider {
    /// What this provider is, including whether it expresses expiry and
    /// accepts hints. The `name` is also the `source` on every candidate
    /// it emits, which is what makes provenance checkable.
    fn descriptor(&self) -> ProviderDescriptor;

    /// Begin. Called once; a second call is [`ProviderError::AlreadyStarted`].
    ///
    /// # Errors
    /// [`ProviderError`] when the provider cannot begin, which the caller
    /// records as health rather than propagating as a fault.
    fn start(&mut self, now_ms: u64) -> Result<(), ProviderError>;

    /// Take at most `max` pending events, oldest first.
    ///
    /// Empty before `start` and after `shutdown`. Total by construction:
    /// a provider reports trouble through [`Self::health`], never by
    /// panicking on a caller's schedule.
    fn drain_events(&mut self, now_ms: u64, max: usize) -> Vec<DiscoveryEvent>;

    /// Offer a hint. A class this provider does not handle must come back
    /// [`HintDisposition::Unsupported`].
    fn add_hint(&mut self, hint: PeerHint, now_ms: u64) -> HintDisposition;

    /// Current health. `Unavailable` before `start` and after `shutdown`.
    fn health(&self) -> ProviderHealth;

    /// Stop. Idempotent and bounded: a second call is a no-op, and the
    /// event stream is empty from here on.
    fn shutdown(&mut self, now_ms: u64);
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

    #[test]
    fn an_oversized_address_array_is_refused_without_materializing_it() {
        // The ceiling used to be applied AFTER `Vec::<String>::deserialize`
        // had parsed and allocated the whole array, so a document with a
        // million addresses was fully paid for and then rejected -- a
        // semantic limit, not the resource limit it exists to be.
        //
        // Far past the cap, so a visitor that stopped at MAX + 1 and one
        // that materialized everything are distinguishable in cost even
        // though both refuse.
        let addresses: Vec<String> = (0..MAX_ADDRESSES * 50)
            .map(|i| format!("/ip4/192.0.2.1/tcp/{i}"))
            .collect();
        let json = format!(
            r#"{{"peer_id":"{P1}","addresses":{},"source":"mdns","observed_at":1}}"#,
            serde_json::to_string(&addresses).expect("json")
        );
        let error = serde_json::from_str::<CandidatePeer>(&json)
            .expect_err("an over-long address array is refused");
        assert!(
            error.to_string().contains("at most"),
            "refused by the ceiling rather than incidentally: {error}"
        );
    }

    #[test]
    fn an_oversized_observation_array_is_refused_without_materializing_it() {
        let observations: Vec<String> = (0..MAX_PROTOCOL_OBSERVATIONS * 50)
            .map(|i| format!(r#"{{"protocol_id":"/spike/{i}","supported":true,"observed_at":1}}"#))
            .collect();
        let json = format!(
            r#"{{"peer_id":"{P1}","addresses":[],"source":"mdns","observed_at":1,"protocol_observations":[{}]}}"#,
            observations.join(",")
        );
        let error = serde_json::from_str::<CandidatePeer>(&json)
            .expect_err("an over-long observation array is refused");
        assert!(
            error.to_string().contains("at most"),
            "refused by the ceiling rather than incidentally: {error}"
        );
    }

    #[test]
    fn an_oversized_address_inside_a_legal_array_is_still_refused() {
        // The count bound alone is not a resource bound: one enormous
        // string inside a short array is unbounded input the array
        // ceiling never sees. Checked per element as it arrives.
        let huge = "/ip4/192.0.2.1/tcp/".to_owned() + &"9".repeat(MAX_ADDRESS_BYTES);
        let json = format!(
            r#"{{"peer_id":"{P1}","addresses":["{huge}"],"source":"mdns","observed_at":1}}"#
        );
        let error = serde_json::from_str::<CandidatePeer>(&json)
            .expect_err("an over-long address is refused");
        assert!(
            error.to_string().contains("bytes"),
            "refused for its length: {error}"
        );
    }

    #[test]
    fn an_enormous_address_is_refused_before_it_is_owned() {
        // `next_element::<String>` allocates and parses the whole value
        // before any check on the result can run, so a length check
        // afterwards rejects input it has already paid for -- the same
        // collect-then-count mistake as the array itself, one level
        // down. Far past the limit, so the two are distinguishable in
        // cost even though both refuse.
        let huge = "/ip4/192.0.2.1/tcp/".to_owned() + &"9".repeat(MAX_ADDRESS_BYTES * 100);
        let json = format!(
            r#"{{"peer_id":"{P1}","addresses":["{huge}"],"source":"mdns","observed_at":1}}"#
        );
        let error = serde_json::from_str::<CandidatePeer>(&json)
            .expect_err("an over-long address is refused");
        assert!(
            error.to_string().contains("bytes"),
            "refused for its length: {error}"
        );
    }

    /// The refusal must hold on the path where `serde_json` builds the
    /// string itself, not only where it can borrow one.
    ///
    /// Every other address test uses unescaped bytes, which `from_str`
    /// hands to `visit_str` as a borrowed slice -- so they never travel
    /// the unescape path at all, and review was right that they proved
    /// less than the comment beside them claimed. `\u0039` is `9`, so
    /// this is the same address as the test above, arriving the other
    /// way.
    #[test]
    fn an_escaped_address_is_refused_on_the_unescape_path() {
        let escaped = "\\u0039".repeat(MAX_ADDRESS_BYTES + 1);
        let json = format!(
            r#"{{"peer_id":"{P1}","addresses":["/ip4/192.0.2.1/tcp/{escaped}"],"source":"mdns","observed_at":1}}"#
        );
        let error = serde_json::from_str::<CandidatePeer>(&json)
            .expect_err("an over-long escaped address is refused");
        assert!(
            error.to_string().contains("bytes"),
            "refused for its length, not its syntax: {error}"
        );
    }

    /// EVERY BOUNDED STRING, not the one review happened to name.
    ///
    /// `BoundedAddress` closed the address path and the two sibling
    /// fields in this module kept the derived one: `ProtocolId` called
    /// `String::deserialize` and checked afterwards, and `source`/`name`
    /// were bounded only in `validate`, which runs once the whole record
    /// is already materialised. Review found the first of those; this
    /// table exists so the third does not need a seventh round.
    #[test]
    fn every_bounded_string_field_is_refused_inside_the_visitor() {
        let long_protocol = "p".repeat(MAX_PROTOCOL_ID_BYTES + 1);
        let long_name = "n".repeat(MAX_PROVIDER_NAME_BYTES + 1);

        let cases: [(&str, String); 3] = [
            (
                "protocol_id",
                format!(r#"{{"protocol_id":"{long_protocol}","supported":true,"observed_at":1}}"#),
            ),
            (
                "source",
                format!(
                    r#"{{"peer_id":"{P1}","addresses":["/ip4/192.0.2.1/tcp/1"],"source":"{long_name}","observed_at":1}}"#
                ),
            ),
            (
                "name",
                format!(r#"{{"name":"{long_name}","interface_version":"1.0"}}"#),
            ),
        ];

        for (field, json) in cases {
            let error = match field {
                "protocol_id" => serde_json::from_str::<ProtocolObservation>(&json)
                    .err()
                    .map(|e| e.to_string()),
                "source" => serde_json::from_str::<CandidatePeer>(&json)
                    .err()
                    .map(|e| e.to_string()),
                _ => serde_json::from_str::<ProviderDescriptor>(&json)
                    .err()
                    .map(|e| e.to_string()),
            }
            .unwrap_or_else(|| panic!("an over-long `{field}` must be refused"));

            // "must be at most" is the VISITOR's phrasing. Asserting
            // only "bytes" passed with `ProtocolId` reverted to
            // `String::deserialize`, because `parse` rejects the same
            // input afterwards with "is N bytes; the limit is ..." --
            // the test agreed with the fix instead of testing it, and
            // the mutation is what said so. The claim here is not "an
            // over-long value is refused" but "it is refused BEFORE it
            // is owned", and the message is what distinguishes them.
            assert!(
                error.contains("must be at most"),
                "`{field}` refused inside the VISITOR, not after parsing: {error}"
            );
        }
    }

    /// The same three through the UNESCAPE path, which is where the
    /// earlier address fix found its own gap: an unescaped token is
    /// borrowed, so a test written with plain characters never exercises
    /// the branch that owns a buffer.
    #[test]
    fn every_bounded_string_field_is_refused_on_the_unescape_path() {
        // `\u0039` is `9`, so each of these is the same over-long value
        // arriving the other way.
        let esc_protocol = "\\u0039".repeat(MAX_PROTOCOL_ID_BYTES + 1);
        let esc_name = "\\u0039".repeat(MAX_PROVIDER_NAME_BYTES + 1);

        let protocol_json =
            format!(r#"{{"protocol_id":"{esc_protocol}","supported":true,"observed_at":1}}"#);
        assert!(
            serde_json::from_str::<ProtocolObservation>(&protocol_json)
                .expect_err("escaped over-long protocol_id is refused")
                .to_string()
                .contains("must be at most")
        );

        let source_json = format!(
            r#"{{"peer_id":"{P1}","addresses":["/ip4/192.0.2.1/tcp/1"],"source":"{esc_name}","observed_at":1}}"#
        );
        assert!(
            serde_json::from_str::<CandidatePeer>(&source_json)
                .expect_err("escaped over-long source is refused")
                .to_string()
                .contains("must be at most")
        );

        let name_json = format!(r#"{{"name":"{esc_name}","interface_version":"1.0"}}"#);
        assert!(
            serde_json::from_str::<ProviderDescriptor>(&name_json)
                .expect_err("escaped over-long name is refused")
                .to_string()
                .contains("must be at most")
        );
    }

    #[test]
    fn duplicate_addresses_are_still_refused_by_the_visitor() {
        // The uniqueness check moved into the visitor with the count.
        // Losing it there would be silent: a `BTreeSet` collapses
        // duplicates, so the result looks perfectly well-formed.
        let json = format!(
            r#"{{"peer_id":"{P1}","addresses":["/ip4/192.0.2.1/tcp/1","/ip4/192.0.2.1/tcp/1"],"source":"mdns","observed_at":1}}"#
        );
        let error =
            serde_json::from_str::<CandidatePeer>(&json).expect_err("duplicates are refused");
        assert!(error.to_string().contains("unique"), "{error}");
    }
}

#[cfg(test)]
mod provider_contract_tests {
    use super::*;

    const P1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";

    fn peer() -> TransportIdentity {
        TransportIdentity::parse(P1).expect("valid test identity")
    }

    /// A minimal conforming provider: it emits one candidate on start and
    /// obeys every lifecycle rule. Its purpose is to show the trait CAN be
    /// satisfied — the conformance suite proves that a provider breaking
    /// these rules is caught.
    struct Conforming {
        started: bool,
        stopped: bool,
        pending: Vec<DiscoveryEvent>,
    }

    impl Conforming {
        fn new() -> Self {
            Self {
                started: false,
                stopped: false,
                pending: Vec::new(),
            }
        }
    }

    impl DiscoveryProvider for Conforming {
        fn descriptor(&self) -> ProviderDescriptor {
            ProviderDescriptor {
                name: "conforming".to_owned(),
                interface_version: "1.0".to_owned(),
                config_version: None,
                scope: ProviderScope::Configured,
                mode: ProviderMode::Passive,
                supports_expiry: false,
                supports_hints: false,
            }
        }

        fn start(&mut self, now_ms: u64) -> Result<(), ProviderError> {
            if self.started {
                return Err(ProviderError::AlreadyStarted);
            }
            self.started = true;
            self.pending.push(DiscoveryEvent::CandidateObserved {
                candidate: Box::new(CandidatePeer {
                    peer_id: peer(),
                    addresses: ["/ip4/127.0.0.1/tcp/1".to_owned()].into_iter().collect(),
                    source: "conforming".to_owned(),
                    observed_at: now_ms,
                    expires_at: None,
                    protocol_observations: BTreeSet::new(),
                }),
            });
            Ok(())
        }

        fn drain_events(&mut self, _now_ms: u64, max: usize) -> Vec<DiscoveryEvent> {
            if !self.started || self.stopped {
                return Vec::new();
            }
            let take = max.min(self.pending.len());
            self.pending.drain(..take).collect()
        }

        fn add_hint(&mut self, _hint: PeerHint, _now_ms: u64) -> HintDisposition {
            // `supports_hints: false`, so this MUST be explicit.
            HintDisposition::Unsupported
        }

        fn health(&self) -> ProviderHealth {
            if self.started && !self.stopped {
                ProviderHealth::Healthy
            } else {
                ProviderHealth::Unavailable
            }
        }

        fn shutdown(&mut self, _now_ms: u64) {
            self.stopped = true;
            self.pending.clear();
        }
    }

    #[test]
    fn no_events_exist_before_start() {
        let mut p = Conforming::new();
        assert!(p.drain_events(0, 8).is_empty());
        assert_eq!(p.health(), ProviderHealth::Unavailable);
        p.start(1).expect("starts");
        assert_eq!(p.drain_events(1, 8).len(), 1);
        assert_eq!(p.health(), ProviderHealth::Healthy);
    }

    #[test]
    fn a_second_start_is_refused() {
        let mut p = Conforming::new();
        p.start(1).expect("starts");
        assert_eq!(p.start(2), Err(ProviderError::AlreadyStarted));
    }

    #[test]
    fn the_drain_respects_the_callers_bound() {
        let mut p = Conforming::new();
        p.start(1).expect("starts");
        // One event pending, but a zero bound takes nothing — the caller
        // states the batch size, not the provider.
        assert!(p.drain_events(1, 0).is_empty());
        assert_eq!(p.drain_events(1, 1).len(), 1);
    }

    #[test]
    fn shutdown_is_idempotent_and_closes_the_stream() {
        let mut p = Conforming::new();
        p.start(1).expect("starts");
        p.shutdown(2);
        p.shutdown(3); // no panic, no change
        assert!(
            p.drain_events(4, 8).is_empty(),
            "the stream is empty from shutdown onward"
        );
        assert_eq!(p.health(), ProviderHealth::Unavailable);
    }

    #[test]
    fn an_unsupported_hint_is_refused_explicitly() {
        let mut p = Conforming::new();
        p.start(1).expect("starts");
        // The descriptor says supports_hints: false, and the answer says
        // so too — silence would be the provider taking ownership.
        assert!(!p.descriptor().supports_hints);
        assert_eq!(
            p.add_hint(
                PeerHint::ObservedReachable {
                    peer_id: peer(),
                    address: "/ip4/127.0.0.1/tcp/1".to_owned(),
                    observed_at: 1,
                },
                1
            ),
            HintDisposition::Unsupported
        );
    }

    #[test]
    fn a_providers_name_is_the_source_it_stamps() {
        // Provenance is checkable only because these are the same string:
        // the manager refuses an event whose source is not the registered
        // provider's name.
        let mut p = Conforming::new();
        p.start(1).expect("starts");
        let name = p.descriptor().name;
        let events = p.drain_events(1, 8);
        let DiscoveryEvent::CandidateObserved { candidate } = &events[0] else {
            panic!("expected an observation");
        };
        assert_eq!(candidate.source, name);
    }
}
