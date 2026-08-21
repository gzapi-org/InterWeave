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

/// The profile trust block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustConfig {
    /// The only v1 policy.
    pub policy: TrustPolicyKind,
    /// The data-plane allowlist.
    #[serde(default)]
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_client_kinds: Vec<String>,
    /// Inbound narrowing filter.
    #[serde(default)]
    pub inbound: EndpointTrustPolicy,
    /// Outbound narrowing filter.
    #[serde(default)]
    pub outbound: EndpointTrustPolicy,
}

/// The endpoints block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    #[serde(default)]
    pub entries: Vec<EndpointConfig>,
}

/// A profile's configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

        // Rule 5 — advertised entries fit the directory bound. A DISABLED
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
