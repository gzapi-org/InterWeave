// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Profile configuration v2: structures and the cross-field rules.
//!
//! The interesting work here is **not** the shapes — JSON Schema already
//! describes those, and `validate_contracts.py` checks them. It is the
//! rules that relate one field to another, which a schema cannot express:
//! that a default endpoint names an *enabled* entry, that a static subset
//! is genuinely a subset of profile trust, that advertised entries fit the
//! directory bound. Those are the errors an operator actually makes.
//!
//! # Every rule is a hard error, and all of them are reported
//!
//! [`ProfileConfig::validate`] returns *every* violation rather than the
//! first. An operator fixing a configuration one error per restart is the
//! experience this avoids, and the cost of collecting them is a `Vec`.
//!
//! # No file, no path, no format
//!
//! Nothing here reads anything. The rules are the same whether the profile
//! arrived as YAML on disk, JSON over an admin socket, or a literal in a
//! test — and a crate that knew about paths would tie them to one
//! deployment's layout.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use interweave_transport_api::{EndpointId, TransportIdentity};
use interweave_trust_api::{EndpointTrustPolicy, PeerTrustPolicy};
use serde::{Deserialize, Serialize};

pub mod paths;
pub mod persist;

pub use paths::{NAMESPACE, PROFILES, ProfilePaths, XdgRoots, absolute_or_none};
pub use persist::{
    OWNER_ONLY_DIR, OWNER_ONLY_FILE, create_private_dir, create_private_exclusive, is_owner_only,
    write_atomic, write_private_atomic,
};

/// What can go wrong resolving paths or writing to disk.
///
/// SEPARATE from [`ConfigError`], which is the vocabulary of
/// configuration VALIDATION — a `Vec<ConfigError>` is what `validate`
/// returns, and it is `Clone + PartialEq` because callers compare and
/// collect it. `std::io::Error` is none of those things, and folding it
/// in would cost every validation caller the ability to compare results.
#[derive(Debug)]
pub enum PersistError {
    /// The filesystem refused.
    Io(std::io::Error),
    /// `$HOME` is unset and an XDG default was needed.
    MissingHome,
    /// `XDG_RUNTIME_DIR` is unset.
    ///
    /// Deliberately fatal rather than defaulted. That directory's
    /// guarantees — owner-only, per-user, cleared per boot — are exactly
    /// what an IPC socket relies on, and inventing a `/tmp` path in its
    /// absence would silently drop all three.
    NoRuntimeDir,
    /// A profile name that could escape or hide in a path.
    InvalidProfileName {
        /// The rejected name.
        name: String,
    },
    /// Owner-only permissions cannot be enforced on this platform.
    ///
    /// Refusing beats writing a key file this build cannot protect.
    UnsupportedPlatform,
    /// An exclusive create found the target already present.
    ///
    /// Reported by [`persist::create_private_exclusive`], which installs
    /// a file only if nothing is there — decided by the filesystem in one
    /// operation, so two processes racing to create the same identity
    /// cannot both believe they won.
    AlreadyExists,
}

impl core::fmt::Display for PersistError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "filesystem: {e}"),
            Self::MissingHome => write!(f, "$HOME is unset and an XDG default was required"),
            Self::NoRuntimeDir => write!(
                f,
                "XDG_RUNTIME_DIR is unset; there is no safe substitute for the IPC socket directory"
            ),
            Self::InvalidProfileName { name } => write!(
                f,
                "profile name {name:?} must be 1-64 characters of [A-Za-z0-9_-] and must not begin with a dot"
            ),
            Self::UnsupportedPlatform => write!(
                f,
                "owner-only file permissions cannot be enforced on this platform"
            ),
            Self::AlreadyExists => write!(f, "a file already exists at that path"),
        }
    }
}

impl core::error::Error for PersistError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

/// Maximum configured endpoints in one profile.
pub const MAX_ENDPOINTS: usize = 64;
/// Maximum advertised endpoints the directory may hold.
pub const MAX_ADVERTISED_CEILING: u32 = 32;
/// Default `directory.max_advertised`.
pub const DEFAULT_MAX_ADVERTISED: u32 = 16;
/// Maximum peers in the profile allowlist.
pub const MAX_ALLOWED_PEERS: usize = 4096;
/// Maximum client kinds one endpoint may list.
pub const MAX_CLIENT_KINDS: usize = 16;
/// Longest client-kind label, in characters.
///
/// Characters and not bytes: the contract states the bound as JSON
/// Schema `maxLength`, which counts code points.
pub const MAX_CLIENT_KIND_CHARS: usize = 64;

/// One label in an endpoint's `allowed_client_kinds`.
///
/// # Hygiene, and a bounded string
///
/// This is an accidental-misbinding guard and NEVER authentication
/// (ADR-0017, `contracts/ENDPOINTS.md`): a local client asserts its own
/// kind, so the field decides nothing an attacker could not simply
/// claim. That is exactly why it is a validated type rather than a
/// `String` -- a field with no authority is the one nobody thinks to
/// bound, and it still arrives from configuration and still occupies
/// memory.
///
/// The contract states `minLength: 1, maxLength: 64`. The bound is on
/// CHARACTERS, because that is what JSON Schema counts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ClientKind(String);

impl ClientKind {
    /// Parse a client-kind label.
    ///
    /// # Errors
    /// Returns [`InvalidClientKind`] outside `1..=`[`MAX_CLIENT_KIND_CHARS`].
    pub fn parse(s: impl Into<String>) -> Result<Self, InvalidClientKind> {
        let s = s.into();
        let chars = s.chars().count();
        if chars == 0 || chars > MAX_CLIENT_KIND_CHARS {
            return Err(InvalidClientKind { chars });
        }
        Ok(Self(s))
    }

    /// The label.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for ClientKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ClientKind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Through `parse`, so the wire path and the Rust path enforce
        // the same rule. A derived `Deserialize` on a newtype accepts
        // anything the inner type accepts, which for `String` is
        // everything.
        Self::parse(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// A client-kind label outside the contract's length bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidClientKind {
    /// The length supplied, in characters.
    pub chars: usize,
}

impl core::fmt::Display for InvalidClientKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "a client kind is 1..={MAX_CLIENT_KIND_CHARS} characters, got {}",
            self.chars
        )
    }
}

impl core::error::Error for InvalidClientKind {}

/// Read a bounded, unique array without materializing it first.
///
/// `Vec::deserialize` parses and allocates the whole array before a
/// length check can run, so a ceiling applied afterwards rejects the
/// RESULT while the input has already been paid for -- and that cost is
/// the thing the ceiling exists to bound. One element past the limit is
/// enough to know.
fn bounded_unique_seq<'de, D, T>(
    deserializer: D,
    max: usize,
    what: &'static str,
) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Ord + Clone,
{
    struct Bounded<T> {
        max: usize,
        what: &'static str,
        _item: core::marker::PhantomData<T>,
    }

    impl<'de, T: Deserialize<'de> + Ord + Clone> serde::de::Visitor<'de> for Bounded<T> {
        type Value = Vec<T>;

        fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "at most {} distinct {}", self.max, self.what)
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut out: Vec<T> = Vec::new();
            let mut seen: BTreeSet<T> = BTreeSet::new();
            while let Some(item) = seq.next_element::<T>()? {
                if out.len() >= self.max {
                    return Err(serde::de::Error::custom(format!(
                        "at most {} {}, got more",
                        self.max, self.what
                    )));
                }
                if !seen.insert(item.clone()) {
                    return Err(serde::de::Error::custom(format!(
                        "{} are uniqueItems; an entry is repeated",
                        self.what
                    )));
                }
                out.push(item);
            }
            Ok(out)
        }
    }

    deserializer.deserialize_seq(Bounded {
        max,
        what,
        _item: core::marker::PhantomData,
    })
}

/// The endpoint's `allowed_client_kinds` array, judged as it arrived.
fn wire_client_kinds<'de, D>(deserializer: D) -> Result<Vec<ClientKind>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    bounded_unique_seq(deserializer, MAX_CLIENT_KINDS, "client kinds")
}

/// The endpoint entry array, judged as it arrived.
///
/// [`ProfileConfig::validate`] also reports [`ConfigError::TooManyEndpoints`],
/// because a Rust caller can build the vector directly. The read path
/// needs its own bound for a different reason: validation runs after
/// the whole document has been parsed.
fn wire_entries<'de, D>(deserializer: D) -> Result<Vec<EndpointConfig>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Bounded;

    impl<'de> serde::de::Visitor<'de> for Bounded {
        type Value = Vec<EndpointConfig>;

        fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "at most {MAX_ENDPOINTS} endpoint entries")
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            // Duplicate ids are NOT rejected here. `validate` reports
            // them as `DuplicateEndpointId`, naming the offending id,
            // and an operator fixing a sixty-entry file needs that
            // message rather than a parse failure at an offset.
            let mut out = Vec::new();
            while let Some(entry) = seq.next_element::<EndpointConfig>()? {
                if out.len() >= MAX_ENDPOINTS {
                    return Err(serde::de::Error::custom(format!(
                        "at most {MAX_ENDPOINTS} endpoints, got more"
                    )));
                }
                out.push(entry);
            }
            Ok(out)
        }
    }

    deserializer.deserialize_seq(Bounded)
}

/// The profile allowlist, counted as the array the file supplied.
///
/// A `BTreeSet` collapses repeats before anything can count them, so
/// any number of copies of one PeerId arrived as a set of one and
/// passed the ceiling -- having been read in full on the way.
fn wire_allowed_peers<'de, D>(deserializer: D) -> Result<BTreeSet<TransportIdentity>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Bounded;

    impl<'de> serde::de::Visitor<'de> for Bounded {
        type Value = BTreeSet<TransportIdentity>;

        fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "at most {MAX_ALLOWED_PEERS} peer ids")
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut out = BTreeSet::new();
            let mut count = 0usize;
            while let Some(peer) = seq.next_element::<TransportIdentity>()? {
                count = count.saturating_add(1);
                if count > MAX_ALLOWED_PEERS {
                    return Err(serde::de::Error::custom(format!(
                        "at most {MAX_ALLOWED_PEERS} allowed peers, got more"
                    )));
                }
                out.insert(peer);
            }
            Ok(out)
        }
    }

    deserializer.deserialize_seq(Bounded)
}

/// The profile trust block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustConfig {
    /// The only v1 policy.
    pub policy: TrustPolicyKind,
    /// The data-plane allowlist.
    #[serde(default, deserialize_with = "wire_allowed_peers")]
    pub allowed_peers: BTreeSet<TransportIdentity>,
}

/// The trust policy kinds a profile may select.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrustPolicyKind {
    /// Deny-by-default static allowlist (ADR-0012).
    #[default]
    StaticAllowlist,
}

/// How endpoints may be registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistrationPolicy {
    /// Only configured endpoints exist; a client cannot invent one.
    #[default]
    ConfiguredOnly,
}

/// The endpoint-directory block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryConfig {
    /// Whether the directory answers remote queries.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// How many endpoints may be advertised.
    #[serde(default = "default_max_advertised")]
    pub max_advertised: u32,
}

const fn default_true() -> bool {
    true
}
const fn default_max_advertised() -> u32 {
    DEFAULT_MAX_ADVERTISED
}

impl Default for DirectoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_advertised: DEFAULT_MAX_ADVERTISED,
        }
    }
}

/// One configured endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointConfig {
    /// The route label.
    pub id: EndpointId,
    /// Whether it accepts traffic.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Whether it may appear in the directory.
    #[serde(default)]
    pub advertise: bool,
    /// Client kinds permitted to lease it. Hygiene only, never authority.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "wire_client_kinds"
    )]
    pub allowed_client_kinds: Vec<ClientKind>,
    /// Inbound narrowing filter.
    #[serde(default)]
    pub inbound: EndpointTrustPolicy,
    /// Outbound narrowing filter.
    #[serde(default)]
    pub outbound: EndpointTrustPolicy,
}

/// The endpoints block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointsConfig {
    /// Registration policy.
    #[serde(default)]
    pub registration_policy: RegistrationPolicy,
    /// The default direct endpoint, if one is chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_direct_endpoint: Option<EndpointId>,
    /// Directory settings.
    #[serde(default)]
    pub directory: DirectoryConfig,
    /// The configured endpoints.
    #[serde(default, deserialize_with = "wire_entries")]
    pub entries: Vec<EndpointConfig>,
}

/// A profile's configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileConfig {
    /// Always 2 for this contract.
    pub schema_version: u32,
    /// Trust.
    pub trust: TrustConfig,
    /// Endpoints.
    pub endpoints: EndpointsConfig,
}

/// One violated rule, with enough context to fix it.
///
/// Each carries the offending value rather than only naming the rule: an
/// operator with sixty endpoints needs to know *which* one, and a message
/// that says "duplicate endpoint id" without saying which is a message
/// that sends them looking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// `schema_version` was not 2.
    UnsupportedSchemaVersion {
        /// The version found.
        found: u32,
    },
    /// Two entries share an id.
    DuplicateEndpointId {
        /// The repeated id.
        id: EndpointId,
    },
    /// More entries than the profile may hold.
    TooManyEndpoints {
        /// Entries supplied.
        got: usize,
    },
    /// The allowlist exceeded its ceiling.
    TooManyAllowedPeers {
        /// Peers supplied.
        got: usize,
    },
    /// The default names an endpoint that is not configured.
    DefaultEndpointUnknown {
        /// The named endpoint.
        id: EndpointId,
    },
    /// The default names a configured but disabled endpoint.
    DefaultEndpointDisabled {
        /// The named endpoint.
        id: EndpointId,
    },
    /// A static subset names a peer outside profile trust.
    ///
    /// The rule ADR-0012 states as "endpoint policy may narrow but never
    /// widen". Silently ignoring the extra peer would leave an operator
    /// believing they authorized something they did not.
    SubsetWidensTrust {
        /// Which endpoint.
        endpoint: EndpointId,
        /// Which direction.
        direction: PolicyDirection,
        /// The peer that is not profile-trusted.
        peer: TransportIdentity,
    },
    /// More advertised endpoints than the directory bound allows.
    TooManyAdvertised {
        /// Enabled advertised entries.
        got: usize,
        /// The configured bound.
        max: u32,
    },
    /// `directory.max_advertised` exceeds the architecture ceiling.
    MaxAdvertisedAboveCeiling {
        /// The configured value.
        got: u32,
    },
    /// An endpoint lists more client kinds than the contract allows.
    TooManyClientKinds {
        /// Which endpoint.
        endpoint: EndpointId,
        /// Kinds supplied.
        got: usize,
    },
    /// An endpoint lists the same client kind twice.
    DuplicateClientKind {
        /// Which endpoint.
        endpoint: EndpointId,
        /// The repeated kind.
        kind: ClientKind,
    },
}

/// Which narrowing filter a violation came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyDirection {
    /// The inbound filter.
    Inbound,
    /// The outbound filter.
    Outbound,
}

impl core::fmt::Display for PolicyDirection {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Inbound => "inbound",
            Self::Outbound => "outbound",
        })
    }
}

impl core::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnsupportedSchemaVersion { found } => {
                write!(f, "schema_version is {found}; this build implements 2")
            }
            Self::DuplicateEndpointId { id } => {
                write!(
                    f,
                    "endpoint id '{}' is configured more than once",
                    id.as_str()
                )
            }
            Self::TooManyEndpoints { got } => {
                write!(f, "{got} endpoints exceeds the maximum of {MAX_ENDPOINTS}")
            }
            Self::TooManyAllowedPeers { got } => {
                write!(
                    f,
                    "{got} allowed peers exceeds the maximum of {MAX_ALLOWED_PEERS}"
                )
            }
            Self::DefaultEndpointUnknown { id } => write!(
                f,
                "default_direct_endpoint '{}' is not a configured endpoint",
                id.as_str()
            ),
            Self::DefaultEndpointDisabled { id } => write!(
                f,
                "default_direct_endpoint '{}' names a disabled endpoint",
                id.as_str()
            ),
            Self::SubsetWidensTrust {
                endpoint,
                direction,
                peer,
            } => write!(
                f,
                "endpoint '{}' {direction} static_subset names '{}', which profile trust does not allow — endpoint policy may narrow but never widen",
                endpoint.as_str(),
                peer.as_str()
            ),
            Self::TooManyAdvertised { got, max } => write!(
                f,
                "{got} enabled advertised endpoints exceeds directory.max_advertised of {max}"
            ),
            Self::MaxAdvertisedAboveCeiling { got } => write!(
                f,
                "directory.max_advertised is {got}; the ceiling is {MAX_ADVERTISED_CEILING}"
            ),
            Self::TooManyClientKinds { endpoint, got } => write!(
                f,
                "endpoint '{}' lists {got} client kinds; the maximum is {MAX_CLIENT_KINDS}",
                endpoint.as_str()
            ),
            Self::DuplicateClientKind { endpoint, kind } => write!(
                f,
                "endpoint '{}' lists client kind '{kind}' more than once",
                endpoint.as_str()
            ),
        }
    }
}

impl core::error::Error for ConfigError {}

impl ProfileConfig {
    /// Check every cross-field rule, returning all violations.
    ///
    /// Returns every violation rather than the first: fixing a
    /// configuration one error per restart is the experience this avoids.
    #[must_use]
    pub fn validate(&self) -> Vec<ConfigError> {
        let mut errors = Vec::new();

        if self.schema_version != 2 {
            errors.push(ConfigError::UnsupportedSchemaVersion {
                found: self.schema_version,
            });
        }
        if self.trust.allowed_peers.len() > MAX_ALLOWED_PEERS {
            errors.push(ConfigError::TooManyAllowedPeers {
                got: self.trust.allowed_peers.len(),
            });
        }

        let entries = &self.endpoints.entries;
        if entries.len() > MAX_ENDPOINTS {
            errors.push(ConfigError::TooManyEndpoints { got: entries.len() });
        }

        // Rule 1 — endpoint ids are unique.
        let mut seen: BTreeSet<&EndpointId> = BTreeSet::new();
        for e in entries {
            if !seen.insert(&e.id) {
                errors.push(ConfigError::DuplicateEndpointId { id: e.id.clone() });
            }
        }

        // Rule 2 — a set default names an ENABLED endpoint. Unknown and
        // disabled are separate errors because they need different fixes.
        if let Some(default) = &self.endpoints.default_direct_endpoint {
            match entries.iter().find(|e| &e.id == default) {
                None => errors.push(ConfigError::DefaultEndpointUnknown {
                    id: default.clone(),
                }),
                Some(e) if !e.enabled => errors.push(ConfigError::DefaultEndpointDisabled {
                    id: default.clone(),
                }),
                Some(_) => {}
            }
        }

        // Rules 3 and 4 — a static subset is a genuine subset of profile
        // trust. Narrowing is legal; widening is not, whichever direction.
        let profile = PeerTrustPolicy::new(self.trust.allowed_peers.iter().cloned());
        if let Ok(profile) = &profile {
            for e in entries {
                for (direction, policy) in [
                    (PolicyDirection::Inbound, &e.inbound),
                    (PolicyDirection::Outbound, &e.outbound),
                ] {
                    if policy.is_subset_of(profile) {
                        continue;
                    }
                    // Name the offending peers, not merely the rule.
                    if let EndpointTrustPolicy::StaticSubset { allowed_peers } = policy {
                        for peer in allowed_peers.difference(&self.trust.allowed_peers) {
                            errors.push(ConfigError::SubsetWidensTrust {
                                endpoint: e.id.clone(),
                                direction,
                                peer: peer.clone(),
                            });
                        }
                    }
                }
            }
        }

        // Rule 5 — an endpoint's client kinds fit the contract's array
        // bounds. The read path enforces these too; this covers the
        // caller who built the struct in Rust, and it names the
        // offending endpoint rather than failing at a byte offset.
        for e in entries {
            if e.allowed_client_kinds.len() > MAX_CLIENT_KINDS {
                errors.push(ConfigError::TooManyClientKinds {
                    endpoint: e.id.clone(),
                    got: e.allowed_client_kinds.len(),
                });
            }
            let mut seen_kinds: BTreeSet<&ClientKind> = BTreeSet::new();
            for kind in &e.allowed_client_kinds {
                if !seen_kinds.insert(kind) {
                    errors.push(ConfigError::DuplicateClientKind {
                        endpoint: e.id.clone(),
                        kind: kind.clone(),
                    });
                }
            }
        }

        // Rule 6 — advertised entries fit the directory bound. A DISABLED
        // entry does not count: it advertises nothing, so counting it
        // would reject a configuration that behaves correctly.
        let max_advertised = self.endpoints.directory.max_advertised;
        if max_advertised > MAX_ADVERTISED_CEILING {
            errors.push(ConfigError::MaxAdvertisedAboveCeiling {
                got: max_advertised,
            });
        }
        let advertised = entries.iter().filter(|e| e.advertise && e.enabled).count();
        if advertised > max_advertised as usize {
            errors.push(ConfigError::TooManyAdvertised {
                got: advertised,
                max: max_advertised,
            });
        }

        errors
    }

    /// Whether the configuration satisfies every rule.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.validate().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    const P2: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";

    fn peer(s: &str) -> TransportIdentity {
        TransportIdentity::parse(s).expect("valid identity")
    }

    #[test]
    fn a_misspelled_field_is_refused_rather_than_ignored() {
        // THE FAILURE MODE IS NOT "unexpected configuration". Serde
        // ignores unknown fields by default, and every security-relevant
        // field here defaults toward the PERMISSIVE answer: `enabled` to
        // true, both narrowing filters to inherit-profile-trust,
        // `allowed_client_kinds` to empty meaning no restriction. So a
        // typo does not disable a bound, it removes one -- the operator
        // wrote a narrowing and got the wide default, with no diagnostic
        // anywhere.
        let base = format!(
            r#"{{"schema_version":2,
                 "trust":{{"policy":"static-allowlist","allowed_peers":["{P1}"]}},
                 "endpoints":{{"entries":[{{"id":"human","enabled":false"#
        );

        // Same document, correct spelling: parses, and the endpoint is
        // off. This is the control -- without it the assertions below
        // could be failing for the shape.
        let good = format!("{base}}}]}}}}");
        let cfg: ProfileConfig = serde_json::from_str(&good).expect("the correct spelling parses");
        assert!(!cfg.endpoints.entries[0].enabled);

        // `enabeld: false` used to leave `enabled` at its `true`
        // default: an endpoint the operator believed was off, accepting
        // traffic.
        let typo = format!(
            r#"{{"schema_version":2,
                 "trust":{{"policy":"static-allowlist","allowed_peers":["{P1}"]}},
                 "endpoints":{{"entries":[{{"id":"human","enabeld":false}}]}}}}"#
        );
        let err = serde_json::from_str::<ProfileConfig>(&typo)
            .expect_err("an unknown endpoint field must be refused")
            .to_string();
        assert!(err.contains("enabeld"), "the message must name it: {err}");

        // `inboud` used to leave `inbound` inheriting full profile
        // trust: the operator wrote a narrowing filter and got none.
        let typo = format!(
            r#"{{"schema_version":2,
                 "trust":{{"policy":"static-allowlist","allowed_peers":["{P1}"]}},
                 "endpoints":{{"entries":[{{"id":"human",
                   "inboud":{{"static_subset":["{P1}"]}}}}]}}}}"#
        );
        assert!(
            serde_json::from_str::<ProfileConfig>(&typo).is_err(),
            "a misspelled narrowing filter must not silently widen"
        );

        // Every closed object, not only the endpoint. `additionalProperties:
        // false` is the contract's answer at each level.
        for doc in [
            r#"{"schema_version":2,"trusst":{},
                "trust":{"policy":"static-allowlist"},"endpoints":{}}"#
                .to_owned(),
            format!(
                r#"{{"schema_version":2,
                        "trust":{{"policy":"static-allowlist","allowd_peers":["{P1}"]}},
                        "endpoints":{{}}}}"#
            ),
            r#"{"schema_version":2,"trust":{"policy":"static-allowlist"},
                "endpoints":{"registration_polcy":"configured-only"}}"#
                .to_owned(),
            r#"{"schema_version":2,"trust":{"policy":"static-allowlist"},
                "endpoints":{"directory":{"enabld":false}}}"#
                .to_owned(),
        ] {
            assert!(
                serde_json::from_str::<ProfileConfig>(&doc).is_err(),
                "an unknown field must be refused at every level: {doc}"
            );
        }
    }

    #[test]
    fn a_client_kind_array_is_judged_as_the_array_the_contract_describes() {
        // `endpoint-config.schema.json`: maxItems 16, uniqueItems, and
        // each item 1..=64 characters. The field was a raw `Vec<String>`
        // with none of that enforced -- and being "hygiene, not
        // authority" is precisely why nobody bounded it, while it still
        // arrives from configuration and still occupies memory.
        let doc = |kinds: &str| {
            format!(
                r#"{{"schema_version":2,
                     "trust":{{"policy":"static-allowlist","allowed_peers":["{P1}"]}},
                     "endpoints":{{"entries":[
                       {{"id":"human","allowed_client_kinds":{kinds}}}]}}}}"#
            )
        };

        let cfg: ProfileConfig =
            serde_json::from_str(&doc(r#"["human-client"]"#)).expect("a legal single kind parses");
        assert_eq!(
            cfg.endpoints.entries[0].allowed_client_kinds[0].as_str(),
            "human-client"
        );

        // Seventeen distinct kinds: one past the ceiling.
        let over: Vec<String> = (0..=MAX_CLIENT_KINDS)
            .map(|i| format!(r#""k{i}""#))
            .collect();
        assert!(
            serde_json::from_str::<ProfileConfig>(&doc(&format!("[{}]", over.join(",")))).is_err()
        );
        // Exactly the ceiling is legal, so the case above failed for the
        // count and not for the shape.
        let at_cap: Vec<String> = (0..MAX_CLIENT_KINDS)
            .map(|i| format!(r#""k{i}""#))
            .collect();
        assert!(
            serde_json::from_str::<ProfileConfig>(&doc(&format!("[{}]", at_cap.join(",")))).is_ok()
        );

        // uniqueItems.
        let err = serde_json::from_str::<ProfileConfig>(&doc(r#"["human-client","human-client"]"#))
            .expect_err("a repeated kind is invalid")
            .to_string();
        assert!(err.contains("repeated"), "unexpected message: {err}");

        // Item bounds, both ends. The empty string is the one a derived
        // `Vec<String>` accepted most cheerfully.
        assert!(serde_json::from_str::<ProfileConfig>(&doc(r#"[""]"#)).is_err());
        let long = "k".repeat(MAX_CLIENT_KIND_CHARS + 1);
        assert!(serde_json::from_str::<ProfileConfig>(&doc(&format!(r#"["{long}"]"#))).is_err());
        let at_len = "k".repeat(MAX_CLIENT_KIND_CHARS);
        assert!(serde_json::from_str::<ProfileConfig>(&doc(&format!(r#"["{at_len}"]"#))).is_ok());

        // CHARACTERS, not bytes: JSON Schema `maxLength` counts code
        // points, so 64 multi-byte characters is legal and a byte
        // ceiling would have refused it.
        let wide = "é".repeat(MAX_CLIENT_KIND_CHARS);
        assert_eq!(
            wide.len(),
            MAX_CLIENT_KIND_CHARS * 2,
            "the bytes exceed the bound"
        );
        assert!(
            serde_json::from_str::<ProfileConfig>(&doc(&format!(r#"["{wide}"]"#))).is_ok(),
            "the contract bounds characters"
        );

        // And the Rust path, which no deserializer sees.
        let mut e = endpoint("human");
        e.allowed_client_kinds = vec![
            ClientKind::parse("human-client").expect("legal"),
            ClientKind::parse("human-client").expect("legal"),
        ];
        assert!(
            config(vec![e])
                .validate()
                .iter()
                .any(|err| matches!(err, ConfigError::DuplicateClientKind { .. }))
        );
    }

    #[test]
    fn the_allowlist_and_entry_arrays_are_bounded_on_the_read() {
        // Both ceilings existed only in `validate`, which runs after the
        // whole document has been parsed -- so the array was read and
        // allocated in full and then judged. For the allowlist the
        // ceiling was also evadable: a `BTreeSet` collapses repeats
        // before anything counts them, so any number of copies of one
        // PeerId arrived as a set of one.
        let repeated = vec![format!(r#""{P1}""#); MAX_ALLOWED_PEERS + 1].join(",");
        let doc = format!(
            r#"{{"schema_version":2,
                 "trust":{{"policy":"static-allowlist","allowed_peers":[{repeated}]}},
                 "endpoints":{{}}}}"#
        );
        assert!(
            serde_json::from_str::<ProfileConfig>(&doc).is_err(),
            "an over-length allowlist must be refused before its duplicates vanish"
        );

        let entries: Vec<String> = (0..=MAX_ENDPOINTS)
            .map(|i| format!(r#"{{"id":"e{i}"}}"#))
            .collect();
        let doc = format!(
            r#"{{"schema_version":2,"trust":{{"policy":"static-allowlist"}},
                 "endpoints":{{"entries":[{}]}}}}"#,
            entries.join(",")
        );
        assert!(serde_json::from_str::<ProfileConfig>(&doc).is_err());

        // Exactly the ceiling parses.
        let entries: Vec<String> = (0..MAX_ENDPOINTS)
            .map(|i| format!(r#"{{"id":"e{i}"}}"#))
            .collect();
        let doc = format!(
            r#"{{"schema_version":2,"trust":{{"policy":"static-allowlist"}},
                 "endpoints":{{"entries":[{}]}}}}"#,
            entries.join(",")
        );
        let cfg: ProfileConfig = serde_json::from_str(&doc).expect("the ceiling itself is legal");
        assert_eq!(cfg.endpoints.entries.len(), MAX_ENDPOINTS);
    }

    fn endpoint(id: &str) -> EndpointConfig {
        EndpointConfig {
            id: EndpointId::parse(id).expect("valid endpoint"),
            enabled: true,
            advertise: false,
            allowed_client_kinds: Vec::new(),
            inbound: EndpointTrustPolicy::default(),
            outbound: EndpointTrustPolicy::default(),
        }
    }

    fn config(entries: Vec<EndpointConfig>) -> ProfileConfig {
        ProfileConfig {
            schema_version: 2,
            trust: TrustConfig {
                policy: TrustPolicyKind::StaticAllowlist,
                allowed_peers: [peer(P1), peer(P2)].into_iter().collect(),
            },
            endpoints: EndpointsConfig {
                registration_policy: RegistrationPolicy::ConfiguredOnly,
                default_direct_endpoint: None,
                directory: DirectoryConfig::default(),
                entries,
            },
        }
    }

    #[test]
    fn a_minimal_profile_is_valid() {
        assert!(config(vec![endpoint("human"), endpoint("claude")]).is_valid());
    }

    #[test]
    fn duplicate_endpoint_ids_are_named() {
        let errors = config(vec![endpoint("human"), endpoint("human")]).validate();
        assert_eq!(
            errors,
            vec![ConfigError::DuplicateEndpointId {
                id: EndpointId::parse("human").expect("valid")
            }]
        );
    }

    #[test]
    fn the_default_must_name_an_enabled_endpoint() {
        let mut c = config(vec![endpoint("human")]);
        c.endpoints.default_direct_endpoint = Some(EndpointId::parse("claude").expect("valid"));
        assert_eq!(
            c.validate(),
            vec![ConfigError::DefaultEndpointUnknown {
                id: EndpointId::parse("claude").expect("valid")
            }]
        );

        // Unknown and disabled are DIFFERENT errors: one is a typo, the
        // other is a deliberate change with a forgotten consequence.
        let mut disabled = endpoint("human");
        disabled.enabled = false;
        let mut c = config(vec![disabled]);
        c.endpoints.default_direct_endpoint = Some(EndpointId::parse("human").expect("valid"));
        assert_eq!(
            c.validate(),
            vec![ConfigError::DefaultEndpointDisabled {
                id: EndpointId::parse("human").expect("valid")
            }]
        );
    }

    #[test]
    fn a_widening_subset_names_the_offending_peer() {
        let stranger = peer(&format!(
            "12D3KooW{}",
            "Stranger".to_owned() + &"z".repeat(36)
        ));
        let mut e = endpoint("human");
        e.inbound = EndpointTrustPolicy::StaticSubset {
            allowed_peers: [peer(P1), stranger.clone()].into_iter().collect(),
        };
        let errors = config(vec![e]).validate();
        assert_eq!(
            errors,
            vec![ConfigError::SubsetWidensTrust {
                endpoint: EndpointId::parse("human").expect("valid"),
                direction: PolicyDirection::Inbound,
                peer: stranger,
            }]
        );
    }

    #[test]
    fn narrowing_is_legal_in_both_directions() {
        let mut e = endpoint("human");
        e.inbound = EndpointTrustPolicy::StaticSubset {
            allowed_peers: [peer(P1)].into_iter().collect(),
        };
        e.outbound = EndpointTrustPolicy::StaticSubset {
            allowed_peers: [peer(P1), peer(P2)].into_iter().collect(),
        };
        assert!(config(vec![e]).is_valid());
    }

    #[test]
    fn a_disabled_advertised_entry_does_not_count() {
        // It advertises nothing, so counting it would reject a profile
        // that behaves correctly.
        let mut entries: Vec<_> = (0..16)
            .map(|i| {
                let mut e = endpoint(&format!("e{i}"));
                e.advertise = true;
                e
            })
            .collect();
        let mut spare = endpoint("spare");
        spare.advertise = true;
        spare.enabled = false;
        entries.push(spare);
        assert!(config(entries).is_valid());
    }

    #[test]
    fn advertised_entries_must_fit_the_directory_bound() {
        let entries: Vec<_> = (0..17)
            .map(|i| {
                let mut e = endpoint(&format!("e{i}"));
                e.advertise = true;
                e
            })
            .collect();
        assert_eq!(
            config(entries).validate(),
            vec![ConfigError::TooManyAdvertised { got: 17, max: 16 }]
        );
    }

    #[test]
    fn every_violation_is_reported_not_only_the_first() {
        // Fixing a configuration one error per restart is the experience
        // this avoids.
        let mut c = config(vec![endpoint("human"), endpoint("human")]);
        c.schema_version = 3;
        c.endpoints.default_direct_endpoint = Some(EndpointId::parse("absent").expect("valid"));
        let errors = c.validate();
        assert!(errors.len() >= 3, "expected several, got {errors:?}");
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ConfigError::UnsupportedSchemaVersion { .. }))
        );
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ConfigError::DuplicateEndpointId { .. }))
        );
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ConfigError::DefaultEndpointUnknown { .. }))
        );
    }

    #[test]
    fn the_directory_ceiling_is_enforced() {
        let mut c = config(vec![endpoint("human")]);
        c.endpoints.directory.max_advertised = 33;
        assert!(
            c.validate()
                .iter()
                .any(|e| matches!(e, ConfigError::MaxAdvertisedAboveCeiling { got: 33 }))
        );
    }

    #[test]
    fn error_messages_name_the_offending_value() {
        let e = ConfigError::DuplicateEndpointId {
            id: EndpointId::parse("human").expect("valid"),
        };
        assert!(e.to_string().contains("human"));
        let e = ConfigError::TooManyAdvertised { got: 17, max: 16 };
        assert!(e.to_string().contains("17") && e.to_string().contains("16"));
    }
}
