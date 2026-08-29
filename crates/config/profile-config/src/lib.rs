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

use interweave_discovery_api::MAX_ADDRESS_BYTES;
use interweave_transport_api::{ChannelId, EndpointId, TransportIdentity};
use interweave_trust_api::{EndpointTrustPolicy, PeerTrustPolicy};
use serde::{Deserialize, Serialize};

pub mod paths;
pub mod persist;

pub use paths::{NAMESPACE, PROFILES, ProfilePaths, XdgRoots, absolute_or_none};
pub use persist::{
    OWNER_ONLY_DIR, OWNER_ONLY_FILE, create_private_dir, create_private_exclusive, is_owner_only,
    require_private_dir, write_atomic, write_private_atomic,
};

/// Which provider a `discovery.providers` entry configures.
///
/// A tagged union, per `config.schema.yaml`. `kademlia` is a KNOWN type
/// this build does not implement: it parses, and enabling it is a
/// validation error rather than a silent omission —
/// `PROVIDER-CONTRACT.md` is explicit that "the runtime must never
/// silently start while omitting a provider that configuration enables",
/// and ADR-0034 makes a reduced build reject a defaulted-on entry as a
/// hard startup error. Stage 10 implements it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiscoveryProviderType {
    /// Bounded advisory persistence of what this node observed.
    PeerCache,
    /// LAN multicast.
    Mdns,
    /// Operator-configured entries.
    StaticBootstrap,
    /// Peer routing over a DHT. Not implemented until Stage 10.
    Kademlia,
}

impl DiscoveryProviderType {
    /// The provider name this type is known by, matching the `source` its
    /// implementation stamps on every candidate.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PeerCache => "peer-cache",
            Self::Mdns => "mdns",
            Self::StaticBootstrap => "static-bootstrap",
            Self::Kademlia => "kademlia",
        }
    }

    /// Whether this build can actually RUN this provider.
    ///
    /// Not a preference: it is a fact about the binary, and a profile
    /// enabling a provider that cannot run must fail loudly rather than
    /// starting a node that silently discovers nothing
    /// (`PROVIDER-CONTRACT.md`).
    ///
    /// `Mdns` is false for a reason worth stating, because the crate
    /// exists and its tests pass: `interweave-discovery-mdns` is the
    /// NORMALIZATION half, and its multicast backend is deferred while
    /// `libp2p-mdns` pins a `hickory-proto` carrying RUSTSEC-2026-0118
    /// and -0119 (see the workspace manifest). Without that backend the
    /// provider receives nothing, so an operator enabling `mdns` would
    /// get a healthy-looking provider performing no LAN discovery — the
    /// same silent omission the Kademlia rule exists to prevent. It flips
    /// to true in the change that wires the backend.
    #[must_use]
    pub const fn is_implemented(self) -> bool {
        match self {
            Self::PeerCache | Self::StaticBootstrap => true,
            Self::Mdns | Self::Kademlia => false,
        }
    }
}

/// The provider-specific `config` namespace.
///
/// The schema gives every provider type its own block under `config`,
/// and the canonical examples are written that way. Modelled as one
/// struct whose fields are all optional rather than as an enum keyed on
/// the type: `serde` resolves the tag and the body separately, and a
/// per-type enum would have to be internally tagged on the SAME `type`
/// field the parent already consumed. Which fields belong to which
/// provider is enforced in validation, where the error can name both.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryProviderSettings {
    /// `static-bootstrap`: the configured entries, each a multiaddr
    /// ending in `/p2p/<PeerId>`, checked against the vocabulary
    /// `architecture/discovery/providers/static-bootstrap.md` accepts.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "wire_static_peers"
    )]
    pub peers: Vec<String>,
    /// `peer-cache`: how long a record stays usable.
    ///
    /// Parsed but not yet consumed: `PeerCache` carries its own frozen
    /// TTL, and narrowing it from configuration is Stage 12's composition
    /// (which is what builds the cache). Accepting the documented field
    /// and ignoring it silently would be worse than either — so it is
    /// parsed, and the runtime that grows a use for it is where it starts
    /// mattering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
    /// `peer-cache`: how many peers to retain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_entries: Option<u32>,

    // `kademlia`: the documented namespace, PARSED THOUGH THE PROVIDER IS
    // NOT BUILT.
    //
    // This struct is `deny_unknown_fields`, so omitting these did not
    // leave them unread — it made the canonical profile in
    // `architecture/config/examples/kademlia-enabled.yaml` fail to
    // deserialize, with a serde error about an unknown key. That error
    // arrives BEFORE `validate`, so the refusal an operator was meant to
    // read ("this build does not include KademliaDiscovery; Stage 10 adds
    // it") could never be reached, and a DISABLED kademlia entry — which
    // is legal and is how an operator stages a profile ahead of the
    // build — was rejected outright.
    //
    // So the schema is modelled now and consumed at Stage 10. Nothing
    // here interprets a value; `validate` still refuses the provider when
    // it is enabled.
    /// `kademlia`: config schema version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_version: Option<u32>,
    /// `kademlia`: the non-secret DHT namespace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_id: Option<String>,
    /// `kademlia`: `client` or `server`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// `kademlia`: which trust the DHT routers are held to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_peer_policy: Option<String>,
    /// `kademlia`: providers seeding the routing table.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "wire_seed_sources"
    )]
    pub seed_sources: Vec<String>,
    /// `kademlia`: how long a routing candidate stays usable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_ttl: Option<String>,
    /// `kademlia`: k-bucket width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kbucket_size: Option<u32>,
    /// `kademlia`: routing-table ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_routing_peers: Option<u32>,
    /// `kademlia`: per-query timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_timeout: Option<String>,
    /// `kademlia`: query parallelism.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallelism: Option<u32>,
    /// `kademlia`: whether query paths must be disjoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disjoint_query_paths: Option<bool>,
    /// `kademlia`: concurrent query ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_queries: Option<u32>,
    /// `kademlia`: query rate ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_queries_per_minute: Option<u32>,
    /// `kademlia`: how often routing exploration runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exploration_interval: Option<String>,
    /// `kademlia`: jitter applied to that interval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exploration_jitter_percent: Option<u32>,
    /// `kademlia`: results accepted from one query.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_results_per_query: Option<u32>,
    /// `kademlia`: routing-table target size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_routing_peers: Option<u32>,
    /// `kademlia`: minimum spacing between targeted lookups.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub targeted_lookup_cooldown: Option<String>,
    /// `kademlia`: floor on bootstrap frequency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_min_interval: Option<String>,
    /// `kademlia`: routine bootstrap refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_refresh_interval: Option<String>,
    /// `kademlia`: peer routing only — records are not used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_mode: Option<String>,
}

impl DiscoveryProviderSettings {
    /// Every kademlia setting outside the value `config.schema.yaml`
    /// documents.
    ///
    /// CHECKED EVEN WHEN THE ENTRY IS DISABLED. A disabled entry is how an
    /// operator stages a profile ahead of the build, so leaving these
    /// unchecked means `mode: "typo"` or `kbucket_size: 1` is reported
    /// valid here and surfaces only after deployment on a supporting
    /// build — the profile silently changes meaning between the machine
    /// that validated it and the one that runs it.
    ///
    /// Per-field only. The schema's cross-field rules
    /// (`target_routing_peers <= max_routing_peers` and the rest) are
    /// explicitly gated on `enabled=true`, which this build refuses
    /// outright, so they belong with the provider that implements them.
    #[must_use]
    pub fn kademlia_value_errors(&self) -> Vec<ConfigError> {
        let mut errors = Vec::new();

        let mut literal = |field: &'static str, got: Option<&String>, want: &str| {
            if let Some(value) = got
                && value != want
            {
                errors.push(ConfigError::InvalidKademliaSetting {
                    field,
                    reason: format!("must be '{want}', got '{value}'"),
                });
            }
        };
        literal(
            "routing_peer_policy",
            self.routing_peer_policy.as_ref(),
            "data-plane-trusted",
        );
        literal("record_mode", self.record_mode.as_ref(), "disabled");

        if let Some(mode) = &self.mode
            && mode != "client"
            && mode != "server"
        {
            errors.push(ConfigError::InvalidKademliaSetting {
                field: "mode",
                reason: format!("must be 'client' or 'server', got '{mode}'"),
            });
        }
        if let Some(version) = self.config_version
            && version != 1
        {
            errors.push(ConfigError::InvalidKademliaSetting {
                field: "config_version",
                reason: format!("must be 1, got {version}"),
            });
        }
        if let Some(id) = &self.network_id
            && !is_network_id(id)
        {
            errors.push(ConfigError::InvalidKademliaSetting {
                field: "network_id",
                reason: format!("must match ^[a-z0-9][a-z0-9._-]{{0,63}}$, got '{id}'"),
            });
        }

        for (field, got, lo, hi) in [
            ("kbucket_size", self.kbucket_size, 8, 20),
            ("max_routing_peers", self.max_routing_peers, 20, 1024),
            ("parallelism", self.parallelism, 1, 10),
            ("max_concurrent_queries", self.max_concurrent_queries, 1, 8),
            ("max_queries_per_minute", self.max_queries_per_minute, 1, 60),
            (
                "exploration_jitter_percent",
                self.exploration_jitter_percent,
                0,
                50,
            ),
            ("max_results_per_query", self.max_results_per_query, 1, 20),
            ("target_routing_peers", self.target_routing_peers, 8, 256),
        ] {
            if let Some(value) = got
                && (value < lo || value > hi)
            {
                errors.push(ConfigError::InvalidKademliaSetting {
                    field,
                    reason: format!("must be {lo}..={hi}, got {value}"),
                });
            }
        }

        for (field, got, range) in [
            ("candidate_ttl", self.candidate_ttl.as_ref(), None),
            (
                "query_timeout",
                self.query_timeout.as_ref(),
                Some((5_000u32, 120_000u32)),
            ),
            (
                "exploration_interval",
                self.exploration_interval.as_ref(),
                Some((30_000, 3_600_000)),
            ),
            (
                "targeted_lookup_cooldown",
                self.targeted_lookup_cooldown.as_ref(),
                Some((30_000, 3_600_000)),
            ),
            (
                "bootstrap_min_interval",
                self.bootstrap_min_interval.as_ref(),
                Some((60_000, 3_600_000)),
            ),
            (
                "bootstrap_refresh_interval",
                self.bootstrap_refresh_interval.as_ref(),
                Some((300_000, 86_400_000)),
            ),
        ] {
            let Some(text) = got else { continue };
            match parse_duration_ms(text) {
                Err(reason) => errors.push(ConfigError::InvalidKademliaSetting { field, reason }),
                Ok(ms) => {
                    if let Some((lo, hi)) = range
                        && (ms < lo || ms > hi)
                    {
                        errors.push(ConfigError::InvalidKademliaSetting {
                            field,
                            reason: format!("must be {lo}ms..={hi}ms, got {ms}ms"),
                        });
                    }
                }
            }
        }

        errors
    }

    /// The first kademlia-only key this block carries, if any.
    ///
    /// Named rather than boolean so the error can point an operator at
    /// something they can search their profile for. The list is every
    /// kademlia key in `config.schema.yaml`; a key added to the struct
    /// without being added here is a key that goes back to being
    /// silently accepted anywhere, which is what
    /// `kademlia_settings_are_refused_on_every_other_provider` pins.
    #[must_use]
    pub fn first_kademlia_field(&self) -> Option<&'static str> {
        let checks: [(&'static str, bool); 21] = [
            ("config_version", self.config_version.is_some()),
            ("network_id", self.network_id.is_some()),
            ("mode", self.mode.is_some()),
            ("routing_peer_policy", self.routing_peer_policy.is_some()),
            ("seed_sources", !self.seed_sources.is_empty()),
            ("candidate_ttl", self.candidate_ttl.is_some()),
            ("kbucket_size", self.kbucket_size.is_some()),
            ("max_routing_peers", self.max_routing_peers.is_some()),
            ("query_timeout", self.query_timeout.is_some()),
            ("parallelism", self.parallelism.is_some()),
            ("disjoint_query_paths", self.disjoint_query_paths.is_some()),
            (
                "max_concurrent_queries",
                self.max_concurrent_queries.is_some(),
            ),
            (
                "max_queries_per_minute",
                self.max_queries_per_minute.is_some(),
            ),
            ("exploration_interval", self.exploration_interval.is_some()),
            (
                "exploration_jitter_percent",
                self.exploration_jitter_percent.is_some(),
            ),
            (
                "max_results_per_query",
                self.max_results_per_query.is_some(),
            ),
            ("target_routing_peers", self.target_routing_peers.is_some()),
            (
                "targeted_lookup_cooldown",
                self.targeted_lookup_cooldown.is_some(),
            ),
            (
                "bootstrap_min_interval",
                self.bootstrap_min_interval.is_some(),
            ),
            (
                "bootstrap_refresh_interval",
                self.bootstrap_refresh_interval.is_some(),
            ),
            ("record_mode", self.record_mode.is_some()),
        ];
        checks.iter().find(|(_, set)| *set).map(|(name, _)| *name)
    }
}

/// One configured discovery provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoveryProviderConfig {
    /// Which provider.
    #[serde(rename = "type")]
    pub provider_type: DiscoveryProviderType,
    /// Whether it runs.
    ///
    /// No `serde` default: the schema gives one only to `kademlia`
    /// (`enabled: bool = true`) and requires the field on the other
    /// three. A blanket default silently turned an incomplete profile
    /// ON — `{ "type": "static-bootstrap", ... }` with no `enabled`
    /// started dialling configured peers because a field was forgotten,
    /// which is a runtime behaviour change from an omission rather than
    /// a decision. Applied per type in `DiscoveryProviderConfig`'s own
    /// `Deserialize`, since a field default cannot see the tag.
    pub enabled: bool,
    /// Composition guidance for address selection, never trust
    /// (ADR-0007). Lower sorts first.
    #[serde(default)]
    pub priority: i32,
    /// The provider-specific block.
    #[serde(default)]
    pub config: DiscoveryProviderSettings,
}

impl<'de> Deserialize<'de> for DiscoveryProviderConfig {
    /// `enabled` defaults only where the schema says it does.
    ///
    /// A `#[serde(default)]` on the field cannot express this: the
    /// default has to know the provider type, and the type is a sibling
    /// field on the same map. So the map is read first and the default
    /// applied after the tag is known — which is also why
    /// `deny_unknown_fields` is spelled out by hand here rather than
    /// derived.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            #[serde(rename = "type")]
            provider_type: DiscoveryProviderType,
            #[serde(default)]
            enabled: Option<bool>,
            #[serde(default)]
            // No default: `config.schema.yaml` declares `priority: int`
            // on every provider type without one. Defaulting to 0 turned
            // a forgotten field into routing policy — and in the
            // preferred direction, since lower sorts first, so the
            // omission silently outranked every provider an operator had
            // actually thought about.
            priority: Option<i32>,
            #[serde(default)]
            config: DiscoveryProviderSettings,
        }

        let wire = Wire::deserialize(d)?;
        let enabled = match wire.enabled {
            Some(value) => value,
            // Only kademlia carries a documented default.
            None if wire.provider_type == DiscoveryProviderType::Kademlia => true,
            None => {
                return Err(serde::de::Error::custom(format!(
                    "provider '{}' requires `enabled`; only kademlia defaults it",
                    wire.provider_type.as_str()
                )));
            }
        };
        let Some(priority) = wire.priority else {
            return Err(serde::de::Error::custom(format!(
                "provider '{}' requires `priority`; the schema gives none a default",
                wire.provider_type.as_str()
            )));
        };
        Ok(Self {
            provider_type: wire.provider_type,
            enabled,
            priority,
            config: wire.config,
        })
    }
}

/// The `discovery` block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryConfig {
    /// The composed providers.
    #[serde(default = "default_providers", deserialize_with = "wire_providers")]
    pub providers: Vec<DiscoveryProviderConfig>,
}

fn default_providers() -> Vec<DiscoveryProviderConfig> {
    // The cache alone: it costs nothing on a node that has never
    // connected, and mDNS stays OFF by default because LAN discovery
    // reveals that a P2P service exists (`providers/mdns.md`).
    vec![DiscoveryProviderConfig {
        provider_type: DiscoveryProviderType::PeerCache,
        enabled: true,
        priority: 10,
        config: DiscoveryProviderSettings::default(),
    }]
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            providers: default_providers(),
        }
    }
}

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
    /// A directory that must be owner-only is not.
    ///
    /// Reported before key-equivalent material is written into it. A
    /// `0600` file inside a directory another account can write is a
    /// file that account can replace, and a `0600` file inside one it
    /// can traverse still leaks its existence, size, and mtime. The
    /// mode on the file is only half the guarantee.
    ///
    /// Refused rather than repaired: a directory that has been open was
    /// open, and quietly narrowing it would hide that it ever was.
    DirectoryNotPrivate {
        /// The directory.
        path: std::path::PathBuf,
        /// The mode it carries, or the owning uid if that is the
        /// problem.
        detail: String,
    },
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
            Self::DirectoryNotPrivate { path, detail } => write!(
                f,
                "{} must be owner-only before key-equivalent material is written into it: {detail}",
                path.display()
            ),
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
/// Maximum discovery providers one profile may compose.
///
/// `config.schema.yaml`: `providers: list[ProviderConfig, max=16]`.
pub const MAX_DISCOVERY_PROVIDERS: usize = 16;

/// `config.schema.yaml`: `seed_sources: list[enum[...], max=3]`.
pub const MAX_KADEMLIA_SEED_SOURCES: usize = 3;

/// The closed set `seed_sources` draws from.
pub const KADEMLIA_SEED_SOURCES: [&str; 3] = ["peer-cache", "mdns", "static-bootstrap"];
/// Maximum static bootstrap entries.
pub const MAX_STATIC_BOOTSTRAP_PEERS: usize = 64;

/// Host protocols a configured address may name.
///
/// From `architecture/discovery/providers/static-bootstrap.md`, which
/// names `/dns4` and `/dns6` explicitly, plus the IP literals every
/// documented profile uses. Not a guess at what libp2p can parse: it is
/// the set an operator is allowed to configure, and it widens by
/// decision.
const ADDRESS_HOST_PROTOCOLS: [&str; 4] = ["ip4", "ip6", "dns4", "dns6"];

/// Transport protocols a configured address may name.
///
/// TCP alone, which is what the substrate builds (Stage 4). A profile
/// naming a transport this build cannot dial is a configuration error an
/// operator should read here, not a dial failure later.
const ADDRESS_TRANSPORT_PROTOCOLS: [&str; 1] = ["tcp"];

/// `/<host>/<value>/<transport>/<port>` against the documented set.
fn validate_address_grammar(address: &str) -> Result<(), &'static str> {
    if !address.starts_with('/') {
        return Err("the address does not start with '/'");
    }
    let parts: Vec<&str> = address.split('/').skip(1).collect();
    if parts.iter().any(|component| component.is_empty()) {
        return Err("the address has an empty component");
    }
    let [host, host_value, transport, port] = parts.as_slice() else {
        return Err("the address is not /<host>/<value>/<transport>/<port>");
    };
    if !ADDRESS_HOST_PROTOCOLS.contains(host) {
        return Err("the address names a host protocol this build does not support");
    }
    if !ADDRESS_TRANSPORT_PROTOCOLS.contains(transport) {
        return Err("the address names a transport this build does not support");
    }
    if port.parse::<u16>().is_err() {
        return Err("the port is not a number in 0..=65535");
    }
    match *host {
        // PARSED, not approximated. Checking the alphabet accepted
        // `:::`, which no dial can use — a check that describes what a
        // literal LOOKS like rather than what one is will always have
        // another shape like that in it. `std::net` owns both grammars
        // exactly and costs no dependency.
        "ip4" => {
            if host_value.parse::<std::net::Ipv4Addr>().is_err() {
                return Err("the /ip4 value is not an IPv4 address");
            }
        }
        "ip6" => {
            if host_value.parse::<std::net::Ipv6Addr>().is_err() {
                return Err("the /ip6 value is not an IPv6 address");
            }
        }
        // A DNS name is not RESOLVED here and must not be: resolution is
        // the dial path's job, and a name that fails to resolve later is
        // a dial diagnostic rather than a bad profile (the same file).
        // Its SYNTAX is another matter — `a..b` cannot resolve for any
        // nameserver, so it is a bad profile and not a dial diagnostic,
        // and checking only the ends left every interior fault standing.
        _ => {
            if host_value.is_empty() || host_value.len() > 253 {
                return Err("the DNS name is empty or longer than 253 bytes");
            }
            for label in host_value.split('.') {
                if label.is_empty() || label.len() > 63 {
                    return Err("the DNS name has an empty or over-long label");
                }
                // Letters, digits and hyphen — the preferred name syntax,
                // and what a hostname a dial can use is limited to.
                if !label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                {
                    return Err("a DNS label has a character outside [A-Za-z0-9-]");
                }
                if label.starts_with('-') || label.ends_with('-') {
                    return Err("a DNS label starts or ends with '-'");
                }
            }
        }
    }
    Ok(())
}

/// Split a `multiaddr-with-peer-id` into its address and PeerId halves.
///
/// The address half is checked STRUCTURALLY and not against the multiaddr
/// grammar — see the comment in the body for why, and for what that does
/// and does not catch. This checks that the entry ends in a
/// `/p2p/<PeerId>` component and that the identity parses, which is
/// exactly what `StaticBootstrapDiscovery` needs to build an entry. It
/// does not parse the rest, because the multiaddr grammar is a backend
/// concept and this crate has no business knowing it.
///
/// # Errors
/// A short static reason naming what is missing.
pub fn split_peer_multiaddr(entry: &str) -> Result<(&str, TransportIdentity), &'static str> {
    let (address, peer) = entry
        .rsplit_once("/p2p/")
        .ok_or("no trailing /p2p/<PeerId> component")?;
    if address.is_empty() {
        return Err("no address before /p2p/");
    }
    // THE DOCUMENTED VOCABULARY, not merely a shape.
    // `architecture/discovery/providers/static-bootstrap.md` requires
    // invalid multiaddress syntax to fail CONFIG validation, and a
    // structural check alone let `/nonsense/1/p2p/<valid id>` validate,
    // report healthy, and fail at every dial.
    //
    // Spelled out here rather than delegated to libp2p: a configuration
    // crate pulling in a networking stack to name five protocols inverts
    // the layering, and the accepted set is a documented decision (that
    // same file) rather than whatever a dependency happens to parse. It
    // widens when a transport is added, in the commit that adds it.
    validate_address_grammar(address)?;
    if peer.is_empty() || peer.contains('/') {
        return Err("the /p2p/ component is not a single PeerId");
    }
    let identity = TransportIdentity::parse(peer.to_owned())
        .map_err(|_| "the PeerId is not a valid identity")?;
    Ok((address, identity))
}
/// The ceiling on one configured `<multiaddr>/p2p/<PeerId>` entry.
///
/// DERIVED FROM THE PARTS, not chosen. A flat 256 was applied to the
/// whole value while `StaticEntry` accepts an address of 256 on its own,
/// so a legal 220-byte address became illegal the moment its required
/// peer suffix was appended — a limit that contradicted the API it feeds.
///
/// Each half is also checked against its own limit after the split, so
/// this bound stops an oversized entry from being read and the halves
/// decide what is actually well-formed.
pub const MAX_STATIC_PEER_BYTES: usize =
    MAX_ADDRESS_BYTES + "/p2p/".len() + TransportIdentity::MAX_BYTES;
/// Maximum advertised endpoints the directory may hold.
///
/// The wire's own bound (ADR-0031), read from the contract crate rather
/// than restated: a response cannot carry more than this, so a profile
/// must not be allowed to advertise more.
pub const MAX_ADVERTISED_CEILING: u32 = interweave_transport_api::MAX_DIRECTORY_ENTRIES as u32;
/// Default `directory.max_advertised`.
pub const DEFAULT_MAX_ADVERTISED: u32 = 16;
/// Maximum channels a profile may desire.
///
/// `config.schema.yaml` states it as `list[ChannelId, max=128]`. A
/// desired channel keeps a mesh warm and therefore costs what a joined
/// one costs, which is why it is bounded rather than left to taste.
pub const MAX_DESIRED_CHANNELS: usize = 128;
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
///
/// Once counted, a repeat is TOLERATED rather than refused, which is the
/// opposite of what an endpoint's `static_subset` does. That asymmetry is
/// the normative shape, not an inconsistency: `config.schema.yaml`
/// declares this field `list[PeerId, max=256]`, while
/// `endpoints/endpoint-config.schema.json` declares the subset
/// `uniqueItems: true`.
/// The desired channels, counted as the array the file supplied.
///
/// Bounded HERE and not only in `validate`, for the reason
/// `wire_allowed_peers` and `wire_entries` are: a ceiling checked after
/// the whole `Vec` exists has already paid for the memory it was meant to
/// refuse. A profile naming a million channels must cost a comparison,
/// not a million allocations.
///
/// Duplicates are NOT rejected here. `validate` reports them as
/// `DuplicateDesiredChannel`, naming the offending channel, and an
/// operator fixing a long list needs that message rather than a parse
/// failure at an offset — the same division `wire_entries` draws.
fn wire_desired_channels<'de, D>(deserializer: D) -> Result<Vec<ChannelId>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Bounded;

    impl<'de> serde::de::Visitor<'de> for Bounded {
        type Value = Vec<ChannelId>;

        fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "at most {MAX_DESIRED_CHANNELS} channel ids")
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            let mut count = 0usize;
            while let Some(channel) = seq.next_element::<ChannelId>()? {
                count = count.saturating_add(1);
                if count > MAX_DESIRED_CHANNELS {
                    return Err(serde::de::Error::custom(format!(
                        "at most {MAX_DESIRED_CHANNELS} desired channels, got more"
                    )));
                }
                out.push(channel);
            }
            Ok(out)
        }
    }

    deserializer.deserialize_seq(Bounded)
}

/// `seed_sources`, bounded and checked against its documented enum
/// while it is read.
///
/// A DISABLED kademlia entry is legal on this build, and `validate` does
/// not look inside a provider it is not going to run. So without this
/// the schema's `list[enum[...], max=3]` was neither a resource bound
/// nor a semantic one: an arbitrarily long list of arbitrary names
/// parsed, allocated in full, and passed validation.
///
/// The names are checked here rather than in `validate` because they are
/// a closed set fixed by the schema, not a cross-field rule — an
/// unknown one is malformed input, and the parse is where malformed
/// input stops.
fn wire_seed_sources<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Bounded;

    impl<'de> serde::de::Visitor<'de> for Bounded {
        type Value = Vec<String>;

        fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(
                f,
                "at most {MAX_KADEMLIA_SEED_SOURCES} of {KADEMLIA_SEED_SOURCES:?}"
            )
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut out: Vec<String> = Vec::new();
            while let Some(entry) = seq.next_element_seed(BoundedStr)? {
                if out.len() >= MAX_KADEMLIA_SEED_SOURCES {
                    return Err(serde::de::Error::custom(format!(
                        "at most {MAX_KADEMLIA_SEED_SOURCES} kademlia seed sources, got more"
                    )));
                }
                if !KADEMLIA_SEED_SOURCES.contains(&entry.as_str()) {
                    return Err(serde::de::Error::custom(format!(
                        "unknown kademlia seed source '{entry}'; expected one of \
                         {KADEMLIA_SEED_SOURCES:?}"
                    )));
                }
                out.push(entry);
            }
            Ok(out)
        }
    }

    deserializer.deserialize_seq(Bounded)
}

/// The provider array, bounded WHILE it is read.
///
/// Same reason as `wire_static_peers`: `validate` enforces the same
/// ceiling, but on a value already built, and each element here is a
/// whole `DiscoveryProviderConfig` carrying its own nested lists. One
/// element past the limit is enough to know.
fn wire_providers<'de, D>(deserializer: D) -> Result<Vec<DiscoveryProviderConfig>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Bounded;

    impl<'de> serde::de::Visitor<'de> for Bounded {
        type Value = Vec<DiscoveryProviderConfig>;

        fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "at most {MAX_DISCOVERY_PROVIDERS} discovery providers")
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some(entry) = seq.next_element::<DiscoveryProviderConfig>()? {
                if out.len() >= MAX_DISCOVERY_PROVIDERS {
                    return Err(serde::de::Error::custom(format!(
                        "at most {MAX_DISCOVERY_PROVIDERS} discovery providers, got more"
                    )));
                }
                out.push(entry);
            }
            Ok(out)
        }
    }

    deserializer.deserialize_seq(Bounded)
}

/// The static-bootstrap list, bounded WHILE it is read.
///
/// `validate` enforces the same two limits, and that is where an operator
/// gets a message naming the offending entry — but validation runs on a
/// value that has already been built, so a profile claiming a million
/// entries, or one entry of a gigabyte, is paid for in full before
/// anything rejects it. The ceiling exists to bound the cost of the
/// input, so it has to apply to the input. One element past the limit is
/// enough to know.
///
/// Length is checked per entry as it arrives for the same reason: a
/// bounded count of unbounded strings is not a bound.
/// One static-bootstrap entry, refused at its byte ceiling before it is
/// copied into an owned `String`.
///
/// A `DeserializeSeed` rather than a plain visitor because the bound has
/// to reach the ELEMENT: a sequence visitor can only bound the count, and
/// a bounded count of unbounded strings is not a bound. This mirrors
/// `bounded_string` in `discovery-api`, which the candidate-address
/// fields already use.
struct BoundedStr;

impl<'de> serde::de::DeserializeSeed<'de> for BoundedStr {
    type Value = String;

    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<String, D::Error> {
        d.deserialize_str(self)
    }
}

impl serde::de::Visitor<'_> for BoundedStr {
    type Value = String;

    fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "a static-bootstrap peer entry of at most {MAX_STATIC_PEER_BYTES} bytes"
        )
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<String, E> {
        if value.len() > MAX_STATIC_PEER_BYTES {
            return Err(serde::de::Error::custom(format!(
                "a static-bootstrap peer entry is at most {MAX_STATIC_PEER_BYTES} bytes, got {}",
                value.len()
            )));
        }
        Ok(value.to_owned())
    }

    /// A deserializer that already owns the buffer hands it over here.
    /// Checking before taking it means the owned value is dropped rather
    /// than kept.
    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<String, E> {
        self.visit_str(&value)
    }
}

fn wire_static_peers<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Bounded;

    impl<'de> serde::de::Visitor<'de> for Bounded {
        type Value = Vec<String>;

        fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(
                f,
                "at most {MAX_STATIC_BOOTSTRAP_PEERS} static bootstrap peers"
            )
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut out: Vec<String> = Vec::new();
            // EACH ELEMENT THROUGH A BOUNDED VISITOR, not through
            // `next_element::<String>()`. That builds the owned `String`
            // first and hands it over complete, so checking its length
            // afterwards bounds what is KEPT and not what is paid for —
            // one oversized token is still allocated in full. `BoundedStr`
            // refuses inside `visit_str`, where a self-describing format
            // can hand over a borrowed slice of its own buffer.
            while let Some(entry) = seq.next_element_seed(BoundedStr)? {
                if out.len() >= MAX_STATIC_BOOTSTRAP_PEERS {
                    return Err(serde::de::Error::custom(format!(
                        "at most {MAX_STATIC_BOOTSTRAP_PEERS} static bootstrap peers, got more"
                    )));
                }
                out.push(entry);
            }
            Ok(out)
        }
    }

    deserializer.deserialize_seq(Bounded)
}

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

/// Bounds on `directory.cache_ttl`, in milliseconds — 10s..5m per
/// `config.schema.yaml`.
const MIN_CACHE_TTL_MS: u32 = 10_000;
const MAX_CACHE_TTL_MS: u32 = 300_000;

const fn default_cache_ttl_ms() -> u32 {
    60_000
}

/// Parse a duration string (`"60s"`, `"5m"`, `"500ms"`) into milliseconds.
///
/// The config documents durations as `<integer><unit>`. A bare integer is
/// also accepted and read as milliseconds, so a JSON producer that emits a
/// number round-trips.
///
/// `h` and `d` are here because the kademlia block uses them (`1h`,
/// `24h`, and `7d` for the cache TTL). `ms` is tested before `m`, and `d`
/// cannot collide with anything else this accepts.
/// `^[a-z0-9][a-z0-9._-]{0,63}$`, spelled out rather than regexed: the
/// crate carries no regex dependency and the grammar is three rules.
fn is_network_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    let Some(&first) = bytes.first() else {
        return false;
    };
    if bytes.len() > 64 {
        return false;
    }
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
}

fn parse_duration_ms(text: &str) -> Result<u32, String> {
    let text = text.trim();
    let (digits, unit): (&str, u64) = if let Some(n) = text.strip_suffix("ms") {
        (n, 1)
    } else if let Some(n) = text.strip_suffix('s') {
        (n, 1_000)
    } else if let Some(n) = text.strip_suffix('m') {
        (n, 60_000)
    } else if let Some(n) = text.strip_suffix('h') {
        (n, 3_600_000)
    } else if let Some(n) = text.strip_suffix('d') {
        (n, 86_400_000)
    } else {
        (text, 1)
    };
    let value: u64 = digits
        .trim()
        .parse()
        .map_err(|_| format!("'{text}' is not a duration like 60s, 5m, 1h, or 500ms"))?;
    u32::try_from(value.saturating_mul(unit))
        .map_err(|_| format!("duration '{text}' overflows the millisecond range"))
}

fn de_cache_ttl<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Wire {
        Text(String),
        Ms(u64),
    }
    match Wire::deserialize(d)? {
        Wire::Text(t) => parse_duration_ms(&t).map_err(serde::de::Error::custom),
        Wire::Ms(ms) => {
            u32::try_from(ms).map_err(|_| serde::de::Error::custom("cache_ttl overflows"))
        }
    }
}

fn ser_cache_ttl<S: serde::Serializer>(ms: &u32, s: S) -> Result<S::Ok, S::Error> {
    if ms.is_multiple_of(1_000) {
        s.serialize_str(&format!("{}s", ms / 1_000))
    } else {
        s.serialize_str(&format!("{ms}ms"))
    }
}

/// The endpoint-directory block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryConfig {
    /// Whether the directory answers remote queries.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// How long this node caches a remote directory result, in ms.
    ///
    /// The requester-side cache term, documented as `cache_ttl` in
    /// `config.schema.yaml` (a `10s..5m` duration; a bare integer is read
    /// as milliseconds).
    #[serde(
        rename = "cache_ttl",
        default = "default_cache_ttl_ms",
        deserialize_with = "de_cache_ttl",
        serialize_with = "ser_cache_ttl"
    )]
    pub cache_ttl_ms: u32,
    /// How many endpoints may be advertised.
    #[serde(default = "default_max_advertised")]
    pub max_advertised: u32,
    /// Directory queries admitted per minute from one remote PeerId.
    #[serde(default = "default_queries_per_minute")]
    pub max_queries_per_minute_per_peer: u32,
    /// Concurrent directory exchanges this profile answers at once.
    #[serde(default = "default_inflight_queries")]
    pub max_inflight_queries: u32,
}

const fn default_true() -> bool {
    true
}
const fn default_max_advertised() -> u32 {
    DEFAULT_MAX_ADVERTISED
}
const fn default_queries_per_minute() -> u32 {
    interweave_transport_api::DEFAULT_QUERIES_PER_PEER_PER_MINUTE
}
const fn default_inflight_queries() -> u32 {
    interweave_transport_api::DEFAULT_INFLIGHT_QUERIES as u32
}

impl Default for DirectoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cache_ttl_ms: default_cache_ttl_ms(),
            max_advertised: DEFAULT_MAX_ADVERTISED,
            max_queries_per_minute_per_peer: default_queries_per_minute(),
            max_inflight_queries: default_inflight_queries(),
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
    ///
    /// REQUIRED, as the contract declares it. A default of `true` here
    /// meant an endpoint the operator never mentioned was on, and an
    /// endpoint they misspelled was on -- the permissive answer arrived
    /// by omission, which is the shape this whole boundary is meant to
    /// refuse.
    pub enabled: bool,
    /// Whether it may appear in the directory.
    ///
    /// Also required. `false` is the safe default and defaulting is
    /// still wrong: it is the schema's job to say the field must be
    /// present, and half-honouring `required` is how the two drift.
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

/// Broadcast channels this profile holds open.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelsConfig {
    /// Channels the daemon subscribes to whether or not a client joined.
    ///
    /// A **warm mesh, not a join.** PUBSUB.md is explicit: a desired
    /// channel with no local consumer delivers to nobody and buffers
    /// nothing, and no client may publish merely because the profile
    /// lists it. What it buys is that the topic's mesh is already formed
    /// when a client does join.
    #[serde(default, deserialize_with = "wire_desired_channels")]
    pub desired: Vec<ChannelId>,
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
    /// Discovery providers.
    ///
    /// Defaulted like `channels`: a profile that says nothing gets the
    /// peer cache and nothing else, which is the posture a node with no
    /// opinion should have.
    #[serde(default)]
    pub discovery: DiscoveryConfig,
    /// Broadcast channels.
    ///
    /// Defaulted, unlike `trust` and `endpoints`, and the difference is
    /// deliberate. Those two are postures every profile author must at
    /// least acknowledge — an empty allowlist is still an explicit
    /// "trust nobody". "This profile desires no channels" is not a
    /// decision anyone needs to state, and requiring it would have
    /// invalidated every existing profile document to record a fact
    /// their authors had no opinion about.
    #[serde(default)]
    pub channels: ChannelsConfig,
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
    /// Two desired channels are the same.
    DuplicateDesiredChannel {
        /// The repeated channel.
        id: ChannelId,
    },
    /// More desired channels than the profile may hold.
    TooManyDesiredChannels {
        /// Channels supplied.
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
    /// `directory.max_queries_per_minute_per_peer` is zero or over the
    /// ceiling.
    DirectoryQueryRateOutOfRange {
        /// The configured value.
        got: u32,
    },
    /// `directory.max_inflight_queries` is zero or over the ceiling.
    DirectoryInflightOutOfRange {
        /// The configured value.
        got: u32,
    },
    /// `directory.cache_ttl` is outside the 10s..5m range.
    DirectoryCacheTtlOutOfRange {
        /// The configured value, in milliseconds.
        got_ms: u32,
    },
    /// More discovery providers than the schema allows.
    TooManyDiscoveryProviders {
        /// How many were configured.
        got: usize,
    },
    /// The same provider type is configured twice.
    ///
    /// Composition is by TYPE (`PROVIDER-CONTRACT.md` dispatches on it),
    /// so two entries naming one type are two answers to the same
    /// question and there is no rule for choosing between them.
    DuplicateDiscoveryProvider {
        /// Which type.
        provider: &'static str,
    },
    /// A provider is enabled that this build does not implement.
    ///
    /// Never a silent omission: the runtime must not start while leaving
    /// out a provider the operator turned on.
    DiscoveryProviderNotImplemented {
        /// Which type.
        provider: &'static str,
    },
    /// More static bootstrap entries than the provider accepts.
    TooManyStaticPeers {
        /// How many were configured.
        got: usize,
    },
    /// A static bootstrap entry was empty or too long.
    InvalidStaticPeer {
        /// Its length in bytes.
        got: usize,
    },
    /// `peers` was set on a provider that has no such field.
    StaticPeersOnWrongProvider {
        /// Which type carried them.
        provider: &'static str,
    },
    /// A static bootstrap entry is not a peer-qualified multiaddr.
    StaticPeerNotPeerQualified {
        /// The entry as written.
        entry: String,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// Cache settings were set on a provider that has no such fields.
    CacheSettingsOnWrongProvider {
        /// Which type carried them.
        provider: &'static str,
    },
    /// A peer-cache setting is outside the value the schema documents.
    InvalidCacheSetting {
        /// Which key.
        field: &'static str,
        /// What is wrong with it.
        reason: String,
    },
    /// A kademlia setting is outside the value the schema documents.
    InvalidKademliaSetting {
        /// Which key.
        field: &'static str,
        /// What is wrong with it.
        reason: String,
    },
    /// Kademlia settings were set on a provider that has no such fields.
    KademliaSettingsOnWrongProvider {
        /// Which type carried them.
        provider: &'static str,
        /// The first misplaced key, so the message names something the
        /// operator can search their profile for.
        field: &'static str,
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
            Self::DuplicateDesiredChannel { id } => {
                write!(f, "channel '{}' is desired more than once", id.as_str())
            }
            Self::TooManyDesiredChannels { got } => {
                write!(
                    f,
                    "{got} desired channels exceeds the maximum of {MAX_DESIRED_CHANNELS}"
                )
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
            Self::DirectoryQueryRateOutOfRange { got } => write!(
                f,
                "directory.max_queries_per_minute_per_peer is {got}; the range is 1..={}",
                interweave_transport_api::MAX_QUERIES_PER_PEER_PER_MINUTE
            ),
            Self::DirectoryInflightOutOfRange { got } => write!(
                f,
                "directory.max_inflight_queries is {got}; the range is 1..={}",
                interweave_transport_api::MAX_INFLIGHT_QUERIES
            ),
            Self::DirectoryCacheTtlOutOfRange { got_ms } => write!(
                f,
                "directory.cache_ttl is {got_ms}ms; the range is {MIN_CACHE_TTL_MS}..={MAX_CACHE_TTL_MS}ms (10s..5m)"
            ),
            Self::TooManyDiscoveryProviders { got } => write!(
                f,
                "discovery.providers lists {got} entries; the maximum is {MAX_DISCOVERY_PROVIDERS}"
            ),
            Self::DuplicateDiscoveryProvider { provider } => write!(
                f,
                "discovery.providers configures '{provider}' twice; composition dispatches on type, so one entry decides it"
            ),
            Self::DiscoveryProviderNotImplemented { provider } => write!(
                f,
                "discovery provider '{provider}' is enabled but this build does not implement it; disable it or use a build that does"
            ),
            Self::TooManyStaticPeers { got } => write!(
                f,
                "static-bootstrap lists {got} peers; the maximum is {MAX_STATIC_BOOTSTRAP_PEERS}"
            ),
            Self::InvalidStaticPeer { got } => write!(
                f,
                "a static-bootstrap peer entry is {got} bytes; it must be 1..={MAX_STATIC_PEER_BYTES}"
            ),
            Self::StaticPeersOnWrongProvider { provider } => write!(
                f,
                "provider '{provider}' carries `peers`, which only static-bootstrap accepts"
            ),
            Self::StaticPeerNotPeerQualified { entry, reason } => write!(
                f,
                "static-bootstrap entry '{entry}' is not a multiaddr with a peer id: {reason}"
            ),
            Self::CacheSettingsOnWrongProvider { provider } => write!(
                f,
                "provider '{provider}' carries `ttl`/`max_entries`, which only peer-cache accepts"
            ),
            Self::InvalidCacheSetting { field, reason } => {
                write!(f, "peer-cache setting `{field}`: {reason}")
            }
            Self::InvalidKademliaSetting { field, reason } => {
                write!(f, "kademlia setting `{field}`: {reason}")
            }
            Self::KademliaSettingsOnWrongProvider { provider, field } => write!(
                f,
                "provider '{provider}' carries `{field}`, which only kademlia accepts"
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

        let desired = &self.channels.desired;
        if desired.len() > MAX_DESIRED_CHANNELS {
            errors.push(ConfigError::TooManyDesiredChannels { got: desired.len() });
        }
        // Reported rather than collapsed. A `Vec` is what the document
        // shape is -- `list[ChannelId, max=128]` -- and silently
        // deduplicating would let a profile claim 200 channels while
        // holding 128, which is the ceiling reading as satisfied when it
        // is not.
        let mut seen_channels: BTreeSet<&ChannelId> = BTreeSet::new();
        for c in desired {
            if !seen_channels.insert(c) {
                errors.push(ConfigError::DuplicateDesiredChannel { id: c.clone() });
            }
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
        // The query rate and concurrency bounds are 1..=ceiling: zero
        // would be a directory that admits nothing wearing the wrong
        // error, and above the ceiling is the resource bound the design
        // stops being bounded below.
        let rate = self.endpoints.directory.max_queries_per_minute_per_peer;
        if rate == 0 || rate > interweave_transport_api::MAX_QUERIES_PER_PEER_PER_MINUTE {
            errors.push(ConfigError::DirectoryQueryRateOutOfRange { got: rate });
        }
        let inflight = self.endpoints.directory.max_inflight_queries;
        if inflight == 0 || inflight > interweave_transport_api::MAX_INFLIGHT_QUERIES as u32 {
            errors.push(ConfigError::DirectoryInflightOutOfRange { got: inflight });
        }
        let cache_ttl = self.endpoints.directory.cache_ttl_ms;
        if !(MIN_CACHE_TTL_MS..=MAX_CACHE_TTL_MS).contains(&cache_ttl) {
            errors.push(ConfigError::DirectoryCacheTtlOutOfRange { got_ms: cache_ttl });
        }

        // Rule 7 — discovery composition. Each of these is a
        // configuration the runtime must refuse to start on rather than
        // silently correct.
        let providers = &self.discovery.providers;
        if providers.len() > MAX_DISCOVERY_PROVIDERS {
            errors.push(ConfigError::TooManyDiscoveryProviders {
                got: providers.len(),
            });
        }
        let mut seen: BTreeSet<DiscoveryProviderType> = BTreeSet::new();
        for entry in providers {
            if !seen.insert(entry.provider_type) {
                errors.push(ConfigError::DuplicateDiscoveryProvider {
                    provider: entry.provider_type.as_str(),
                });
            }
            // ENABLED AND ABSENT IS A HARD ERROR. A disabled entry for an
            // unimplemented provider is fine — it records an intent — but
            // starting while omitting one the operator turned on is what
            // PROVIDER-CONTRACT.md forbids.
            if entry.enabled && !entry.provider_type.is_implemented() {
                errors.push(ConfigError::DiscoveryProviderNotImplemented {
                    provider: entry.provider_type.as_str(),
                });
            }
            if entry.provider_type == DiscoveryProviderType::StaticBootstrap {
                if entry.config.peers.len() > MAX_STATIC_BOOTSTRAP_PEERS {
                    errors.push(ConfigError::TooManyStaticPeers {
                        got: entry.config.peers.len(),
                    });
                }
                for peer in &entry.config.peers {
                    if peer.is_empty() || peer.len() > MAX_STATIC_PEER_BYTES {
                        errors.push(ConfigError::InvalidStaticPeer { got: peer.len() });
                        continue;
                    }
                    // PEER-QUALIFIED, and validated HERE rather than at
                    // startup. The schema says `multiaddr-with-peer-id`,
                    // and StaticBootstrapDiscovery needs a separate
                    // TransportIdentity to build an entry at all — so a
                    // bare `/ip4/.../tcp/4001` is a profile that cannot
                    // become a usable entry, and deferring the complaint
                    // to wiring turns a configuration error into a
                    // startup failure with no line number in it.
                    match split_peer_multiaddr(peer) {
                        Err(reason) => errors.push(ConfigError::StaticPeerNotPeerQualified {
                            entry: peer.clone(),
                            reason,
                        }),
                        // EACH HALF AGAINST ITS OWN LIMIT. The wire
                        // ceiling bounds what is READ and is the sum of
                        // the parts, so on its own it would accept an
                        // address longer than `StaticEntry` will take —
                        // moving the rejection to wiring, where it is a
                        // startup failure with no line number in it.
                        Ok((address, _)) if address.len() > MAX_ADDRESS_BYTES => {
                            errors.push(ConfigError::StaticPeerNotPeerQualified {
                                entry: peer.clone(),
                                reason: "the address is longer than a candidate address may be",
                            });
                        }
                        Ok(_) => {}
                    }
                }
            } else if !entry.config.peers.is_empty() {
                // A `peers` list on mdns or the cache is a configuration
                // that would do nothing, which is worth saying rather
                // than ignoring.
                errors.push(ConfigError::StaticPeersOnWrongProvider {
                    provider: entry.provider_type.as_str(),
                });
            }
            if entry.provider_type != DiscoveryProviderType::PeerCache
                && (entry.config.ttl.is_some() || entry.config.max_entries.is_some())
            {
                errors.push(ConfigError::CacheSettingsOnWrongProvider {
                    provider: entry.provider_type.as_str(),
                });
            }
            // A SHARED SETTINGS STRUCT MAKES EVERY KEY LEGAL EVERYWHERE,
            // which is why each type's keys are checked back to it. Without
            // this, `peer-cache` with `config: { network_id: "typo" }`
            // deserialized, validated, and was silently ignored — the
            // failure mode `deny_unknown_fields` exists to prevent,
            // reintroduced one level down by the struct being shared.
            // CHECKED EVEN WHEN DISABLED. A disabled entry is how an
            // operator stages a profile ahead of the build, so an invalid
            // value there is caught at deployment on a supporting build
            // rather than here — the profile reports valid and changes
            // meaning later, which is the worst moment for it.
            if entry.provider_type == DiscoveryProviderType::Kademlia {
                errors.extend(entry.config.kademlia_value_errors());
            }
            // The cache's own settings, for the same reason as kademlia's:
            // the misplacement check below says only that they are on the
            // right provider, and this build does not consume them yet —
            // so `ttl: "garbage"` was accepted as an arbitrary string and
            // silently ignored, and would first be noticed by the runtime
            // that grows a use for it.
            if entry.provider_type == DiscoveryProviderType::PeerCache {
                if let Some(ttl) = &entry.config.ttl {
                    match parse_duration_ms(ttl) {
                        Err(reason) => {
                            errors.push(ConfigError::InvalidCacheSetting {
                                field: "ttl",
                                reason,
                            });
                        }
                        // A zero TTL expires every record the instant it
                        // is written, so the cache holds nothing and
                        // every start is cold. `CacheLimitsBuilder`
                        // already calls that a misconfiguration; without
                        // this the profile validates here and fails when
                        // the cache is built, which is the wrong place
                        // for an operator to meet it.
                        Ok(0) => errors.push(ConfigError::InvalidCacheSetting {
                            field: "ttl",
                            reason: "must be greater than zero; every record would expire \
                                     as it was written"
                                .to_owned(),
                        }),
                        Ok(_) => {}
                    }
                }
                // The same rule one field over, which the builder also
                // enforces and the review did not name: a cache holding
                // zero peers is the same misconfiguration wearing a
                // different word. No upper bound here — the schema states
                // none, and the cache's own ceiling is its to apply.
                if entry.config.max_entries == Some(0) {
                    errors.push(ConfigError::InvalidCacheSetting {
                        field: "max_entries",
                        reason: "must be greater than zero; the cache would hold nothing"
                            .to_owned(),
                    });
                }
            }
            if entry.provider_type != DiscoveryProviderType::Kademlia
                && let Some(field) = entry.config.first_kademlia_field()
            {
                errors.push(ConfigError::KademliaSettingsOnWrongProvider {
                    provider: entry.provider_type.as_str(),
                    field,
                });
            }
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
                 "endpoints":{{"entries":[{{"id":"human","advertise":false,"enabled":false"#
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
                 "endpoints":{{"entries":[{{"id":"human","enabled":true,"advertise":false,"enabeld":false}}]}}}}"#
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
                 "endpoints":{{"entries":[{{"id":"human","enabled":true,"advertise":false,
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
                       {{"id":"human","enabled":true,"advertise":false,
                         "allowed_client_kinds":{kinds}}}]}}}}"#
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
            .map(|i| format!(r#"{{"id":"e{i}","enabled":true,"advertise":false}}"#))
            .collect();
        let doc = format!(
            r#"{{"schema_version":2,"trust":{{"policy":"static-allowlist"}},
                 "endpoints":{{"entries":[{}]}}}}"#,
            entries.join(",")
        );
        assert!(serde_json::from_str::<ProfileConfig>(&doc).is_err());

        // Exactly the ceiling parses.
        let entries: Vec<String> = (0..MAX_ENDPOINTS)
            .map(|i| format!(r#"{{"id":"e{i}","enabled":true,"advertise":false}}"#))
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
            discovery: DiscoveryConfig::default(),
            channels: ChannelsConfig::default(),
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
    fn the_directory_query_rate_range_is_enforced() {
        for (rate, catch) in [
            (0u32, 0u32),
            (
                interweave_transport_api::MAX_QUERIES_PER_PEER_PER_MINUTE + 1,
                61,
            ),
        ] {
            let mut c = config(vec![endpoint("human")]);
            c.endpoints.directory.max_queries_per_minute_per_peer = rate;
            assert!(
                c.validate().iter().any(
                    |e| matches!(e, ConfigError::DirectoryQueryRateOutOfRange { got } if *got == catch)
                ),
                "rate {rate} should be rejected"
            );
        }
        // The ceiling itself is accepted.
        let mut c = config(vec![endpoint("human")]);
        c.endpoints.directory.max_queries_per_minute_per_peer =
            interweave_transport_api::MAX_QUERIES_PER_PEER_PER_MINUTE;
        assert!(
            !c.validate()
                .iter()
                .any(|e| matches!(e, ConfigError::DirectoryQueryRateOutOfRange { .. }))
        );
    }

    #[test]
    fn cache_ttl_parses_the_documented_forms_and_round_trips() {
        // A profile using the schema's `cache_ttl: 60s` form parses.
        let json = r#"{"enabled":true,"cache_ttl":"60s","max_advertised":16}"#;
        let d: DirectoryConfig = serde_json::from_str(json).expect("60s parses");
        assert_eq!(d.cache_ttl_ms, 60_000);
        // Minutes, milliseconds, and a bare integer of ms.
        assert_eq!(parse_duration_ms("5m").expect("5m"), 300_000);
        assert_eq!(parse_duration_ms("10s").expect("10s"), 10_000);
        assert_eq!(parse_duration_ms("500ms").expect("500ms"), 500);
        assert_eq!(parse_duration_ms("45000").expect("bare ms"), 45_000);
        assert!(parse_duration_ms("soon").is_err());
        // Round-trips through a whole-second string.
        let back = serde_json::to_string(&d).expect("serializes");
        assert!(back.contains("\"cache_ttl\":\"60s\""), "got {back}");
    }

    #[test]
    fn a_profile_that_says_nothing_gets_the_cache_and_nothing_else() {
        // mDNS stays OFF by default: LAN discovery reveals that a P2P
        // service exists, so it is opt-in (`providers/mdns.md`).
        let c = config(vec![endpoint("human")]);
        let enabled: Vec<&str> = c
            .discovery
            .providers
            .iter()
            .filter(|p| p.enabled)
            .map(|p| p.provider_type.as_str())
            .collect();
        assert_eq!(enabled, vec!["peer-cache"]);
        assert!(c.validate().is_empty(), "the default composes cleanly");
    }

    #[test]
    fn an_enabled_unimplemented_provider_is_refused() {
        // PROVIDER-CONTRACT.md: the runtime must never silently start
        // while omitting a provider that configuration enables.
        let mut c = config(vec![endpoint("human")]);
        c.discovery.providers.push(DiscoveryProviderConfig {
            provider_type: DiscoveryProviderType::Kademlia,
            enabled: true,
            priority: 40,
            config: DiscoveryProviderSettings::default(),
        });
        assert!(
            c.validate().iter().any(|e| matches!(
                e,
                ConfigError::DiscoveryProviderNotImplemented {
                    provider: "kademlia"
                }
            )),
            "Stage 10 implements it; enabling it now must fail loudly"
        );
    }

    #[test]
    fn enabling_mdns_is_refused_while_its_backend_is_deferred() {
        // The crate exists and its tests pass, but it is the
        // NORMALIZATION half: without the multicast backend it receives
        // nothing, so an operator enabling it would get a healthy-looking
        // provider doing no LAN discovery. That is the same silent
        // omission the Kademlia rule prevents, so it fails the same way.
        let mut c = config(vec![endpoint("human")]);
        c.discovery.providers.push(DiscoveryProviderConfig {
            provider_type: DiscoveryProviderType::Mdns,
            enabled: true,
            priority: 20,
            config: DiscoveryProviderSettings::default(),
        });
        assert!(
            c.validate().iter().any(|e| matches!(
                e,
                ConfigError::DiscoveryProviderNotImplemented { provider: "mdns" }
            )),
            "an enabled provider with no backend must fail loudly"
        );
    }

    #[test]
    fn a_disabled_unimplemented_provider_is_allowed() {
        // A disabled entry records an intent and starts nothing.
        let mut c = config(vec![endpoint("human")]);
        c.discovery.providers.push(DiscoveryProviderConfig {
            provider_type: DiscoveryProviderType::Kademlia,
            enabled: false,
            priority: 40,
            config: DiscoveryProviderSettings::default(),
        });
        assert!(
            !c.validate()
                .iter()
                .any(|e| matches!(e, ConfigError::DiscoveryProviderNotImplemented { .. }))
        );
    }

    #[test]
    fn a_duplicate_provider_type_is_refused() {
        let mut c = config(vec![endpoint("human")]);
        c.discovery.providers.push(DiscoveryProviderConfig {
            provider_type: DiscoveryProviderType::PeerCache,
            enabled: true,
            priority: 20,
            config: DiscoveryProviderSettings::default(),
        });
        assert!(c.validate().iter().any(|e| matches!(
            e,
            ConfigError::DuplicateDiscoveryProvider {
                provider: "peer-cache"
            }
        )));
    }

    #[test]
    fn the_provider_count_is_bounded() {
        let mut c = config(vec![endpoint("human")]);
        // Distinct types run out, so repeat one: the count rule fires
        // regardless of the duplicate rule also firing.
        c.discovery.providers = (0..MAX_DISCOVERY_PROVIDERS + 1)
            .map(|_| DiscoveryProviderConfig {
                provider_type: DiscoveryProviderType::Mdns,
                enabled: true,
                priority: 0,
                config: DiscoveryProviderSettings::default(),
            })
            .collect();
        assert!(c.validate().iter().any(|e| matches!(
            e,
            ConfigError::TooManyDiscoveryProviders { got } if *got == MAX_DISCOVERY_PROVIDERS + 1
        )));
    }

    #[test]
    fn static_bootstrap_entries_are_bounded_and_validated() {
        let mut c = config(vec![endpoint("human")]);
        c.discovery.providers.push(DiscoveryProviderConfig {
            provider_type: DiscoveryProviderType::StaticBootstrap,
            enabled: true,
            priority: 30,
            config: DiscoveryProviderSettings {
                peers: (0..MAX_STATIC_BOOTSTRAP_PEERS + 1)
                    .map(|i| format!("/ip4/10.0.0.1/tcp/{i}/p2p/{P1}"))
                    .collect(),
                ..DiscoveryProviderSettings::default()
            },
        });
        assert!(c.validate().iter().any(|e| matches!(
            e,
            ConfigError::TooManyStaticPeers { got } if *got == MAX_STATIC_BOOTSTRAP_PEERS + 1
        )));

        let mut c = config(vec![endpoint("human")]);
        c.discovery.providers.push(DiscoveryProviderConfig {
            provider_type: DiscoveryProviderType::StaticBootstrap,
            enabled: true,
            priority: 30,
            config: DiscoveryProviderSettings {
                peers: vec![String::new()],
                ..DiscoveryProviderSettings::default()
            },
        });
        assert!(
            c.validate()
                .iter()
                .any(|e| matches!(e, ConfigError::InvalidStaticPeer { got: 0 }))
        );
    }

    #[test]
    fn peers_on_a_provider_that_has_no_such_field_is_refused() {
        // Silently ignoring it would leave an operator believing the
        // entries were composed.
        let mut c = config(vec![endpoint("human")]);
        c.discovery.providers.push(DiscoveryProviderConfig {
            provider_type: DiscoveryProviderType::Mdns,
            enabled: true,
            priority: 20,
            config: DiscoveryProviderSettings {
                peers: vec![format!("/ip4/10.0.0.1/tcp/4001/p2p/{P1}")],
                ..DiscoveryProviderSettings::default()
            },
        });
        assert!(c.validate().iter().any(|e| matches!(
            e,
            ConfigError::StaticPeersOnWrongProvider { provider: "mdns" }
        )));
    }

    #[test]
    fn a_static_peer_must_be_peer_qualified() {
        // config.schema.yaml says `multiaddr-with-peer-id`, and
        // StaticBootstrapDiscovery needs a separate identity to build an
        // entry — so a bare address is a profile that cannot become a
        // usable entry, and saying so here beats failing at startup.
        let mut c = config(vec![endpoint("human")]);
        c.discovery.providers.push(DiscoveryProviderConfig {
            provider_type: DiscoveryProviderType::StaticBootstrap,
            enabled: true,
            priority: 30,
            config: DiscoveryProviderSettings {
                peers: vec!["/ip4/10.0.0.1/tcp/4001".to_owned()],
                ..DiscoveryProviderSettings::default()
            },
        });
        assert!(
            c.validate()
                .iter()
                .any(|e| matches!(e, ConfigError::StaticPeerNotPeerQualified { .. })),
            "an address with no peer id is refused"
        );

        // With the identity appended it is accepted.
        let mut good = config(vec![endpoint("human")]);
        good.discovery.providers.push(DiscoveryProviderConfig {
            provider_type: DiscoveryProviderType::StaticBootstrap,
            enabled: true,
            priority: 30,
            config: DiscoveryProviderSettings {
                peers: vec![format!("/ip4/10.0.0.1/tcp/4001/p2p/{P1}")],
                ..DiscoveryProviderSettings::default()
            },
        });
        assert!(
            !good
                .validate()
                .iter()
                .any(|e| matches!(e, ConfigError::StaticPeerNotPeerQualified { .. }))
        );
    }

    #[test]
    fn split_peer_multiaddr_names_what_is_missing() {
        let entry = format!("/dns4/host.example/tcp/4001/p2p/{P1}");
        let (address, id) = split_peer_multiaddr(&entry).expect("a peer-qualified entry");
        assert_eq!(address, "/dns4/host.example/tcp/4001");
        assert_eq!(id, peer(P1));

        assert!(split_peer_multiaddr("/ip4/10.0.0.1/tcp/4001").is_err());
        assert!(split_peer_multiaddr(&format!("/p2p/{P1}")).is_err());
        assert!(split_peer_multiaddr("/ip4/10.0.0.1/tcp/1/p2p/not-an-identity").is_err());
        assert!(split_peer_multiaddr(&format!("/ip4/10.0.0.1/tcp/1/p2p/{P1}/extra")).is_err());
        // A valid PeerId does not launder a nonsense prefix.
        assert!(split_peer_multiaddr(&format!("garbage/p2p/{P1}")).is_err());
        assert!(split_peer_multiaddr(&format!("//p2p/{P1}")).is_err());
        assert!(split_peer_multiaddr(&format!("/ip4//tcp/1/p2p/{P1}")).is_err());
    }

    #[test]
    fn cache_settings_on_another_provider_are_refused() {
        let mut c = config(vec![endpoint("human")]);
        c.discovery.providers.push(DiscoveryProviderConfig {
            provider_type: DiscoveryProviderType::Mdns,
            enabled: true,
            priority: 20,
            config: DiscoveryProviderSettings {
                ttl: Some("7d".to_owned()),
                ..DiscoveryProviderSettings::default()
            },
        });
        assert!(c.validate().iter().any(|e| matches!(
            e,
            ConfigError::CacheSettingsOnWrongProvider { provider: "mdns" }
        )));
    }

    #[test]
    fn a_composite_discovery_document_parses() {
        // The shape `architecture/config/examples/composite-discovery.yaml`
        // uses, minus the Kademlia entry this build cannot run.
        // The shape the canonical examples use: provider settings nest
        // under `config`, per config.schema.yaml.
        let json = format!(
            r#"{{
            "providers": [
                {{ "type": "peer-cache", "enabled": true, "priority": 10,
                   "config": {{ "ttl": "7d", "max_entries": 1024 }} }},
                {{ "type": "mdns", "enabled": false, "priority": 20, "config": {{}} }},
                {{ "type": "static-bootstrap", "enabled": true, "priority": 30,
                   "config": {{ "peers": ["/dns4/bootstrap.example.net/tcp/4001/p2p/{P1}"] }} }}
            ]
        }}"#
        );
        let d: DiscoveryConfig = serde_json::from_str(&json).expect("the documented shape parses");
        assert_eq!(d.providers.len(), 3);
        assert_eq!(d.providers[2].config.peers.len(), 1);
        assert_eq!(d.providers[1].provider_type.as_str(), "mdns");
        assert_eq!(d.providers[0].config.ttl.as_deref(), Some("7d"));
        assert_eq!(d.providers[0].config.max_entries, Some(1024));
    }

    #[test]
    fn the_directory_cache_ttl_range_is_enforced() {
        for bad in [MIN_CACHE_TTL_MS - 1, MAX_CACHE_TTL_MS + 1] {
            let mut c = config(vec![endpoint("human")]);
            c.endpoints.directory.cache_ttl_ms = bad;
            assert!(
                c.validate()
                    .iter()
                    .any(|e| matches!(e, ConfigError::DirectoryCacheTtlOutOfRange { got_ms } if *got_ms == bad)),
                "{bad}ms should be rejected"
            );
        }
        // Both bounds are inclusive and accepted.
        for ok in [MIN_CACHE_TTL_MS, MAX_CACHE_TTL_MS] {
            let mut c = config(vec![endpoint("human")]);
            c.endpoints.directory.cache_ttl_ms = ok;
            assert!(
                !c.validate()
                    .iter()
                    .any(|e| matches!(e, ConfigError::DirectoryCacheTtlOutOfRange { .. }))
            );
        }
    }

    #[test]
    fn the_directory_inflight_range_is_enforced() {
        let mut c = config(vec![endpoint("human")]);
        c.endpoints.directory.max_inflight_queries = 0;
        assert!(
            c.validate()
                .iter()
                .any(|e| matches!(e, ConfigError::DirectoryInflightOutOfRange { got: 0 }))
        );
        let mut c = config(vec![endpoint("human")]);
        c.endpoints.directory.max_inflight_queries =
            interweave_transport_api::MAX_INFLIGHT_QUERIES as u32 + 1;
        assert!(
            c.validate()
                .iter()
                .any(|e| matches!(e, ConfigError::DirectoryInflightOutOfRange { got: 65 }))
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

    /// A profile that desires channels, built from names.
    fn with_channels(names: &[&str]) -> ProfileConfig {
        let mut profile = config(Vec::new());
        profile.channels.desired = names
            .iter()
            .map(|n| ChannelId::parse(*n).expect("valid channel id"))
            .collect();
        profile
    }

    #[test]
    fn a_profile_omitting_channels_desires_nothing() {
        // The compatibility case the `#[serde(default)]` exists for:
        // every profile document written before broadcast existed still
        // parses, and desires nothing.
        let doc = format!(
            r#"{{"schema_version":2,
                 "trust":{{"policy":"static-allowlist","allowed_peers":["{P1}"]}},
                 "endpoints":{{"entries":[]}}}}"#
        );
        let parsed: ProfileConfig = serde_json::from_str(&doc).expect("a v2 profile still parses");
        assert!(parsed.channels.desired.is_empty());
        assert!(parsed.validate().is_empty());
    }

    #[test]
    fn desired_channels_are_read_from_the_document() {
        let doc = format!(
            r#"{{"schema_version":2,
                 "trust":{{"policy":"static-allowlist","allowed_peers":["{P1}"]}},
                 "endpoints":{{"entries":[]}},
                 "channels":{{"desired":["general","team.eu:builds/nightly-1"]}}}}"#
        );
        let parsed: ProfileConfig = serde_json::from_str(&doc).expect("parses");
        assert_eq!(parsed.channels.desired.len(), 2);
        assert_eq!(parsed.channels.desired[0].as_str(), "general");
        assert!(parsed.validate().is_empty());
    }

    #[test]
    fn an_unknown_field_inside_channels_is_refused() {
        // `deny_unknown_fields` on the nested struct too: a typo like
        // `desire` must not read as "desires nothing".
        let doc = format!(
            r#"{{"schema_version":2,
                 "trust":{{"policy":"static-allowlist","allowed_peers":["{P1}"]}},
                 "endpoints":{{"entries":[]}},
                 "channels":{{"desire":["general"]}}}}"#
        );
        assert!(
            serde_json::from_str::<ProfileConfig>(&doc).is_err(),
            "a misspelled key must not silently desire nothing"
        );
    }

    #[test]
    fn an_over_length_channel_list_is_refused_while_reading_it() {
        // The ceiling must bind on the READ path, not only in `validate`:
        // a limit checked after the whole Vec exists has already paid for
        // the memory it was meant to refuse. The same reason
        // `wire_allowed_peers` counts the array rather than the set.
        let names: Vec<String> = (0..=MAX_DESIRED_CHANNELS)
            .map(|i| format!("\"c{i}\""))
            .collect();
        let doc = format!(
            r#"{{"schema_version":2,
                 "trust":{{"policy":"static-allowlist","allowed_peers":["{P1}"]}},
                 "endpoints":{{"entries":[]}},
                 "channels":{{"desired":[{}]}}}}"#,
            names.join(",")
        );
        let err = serde_json::from_str::<ProfileConfig>(&doc)
            .expect_err("an over-length list must be refused")
            .to_string();
        assert!(
            err.contains("at most"),
            "the read path must refuse it, not the validator: {err}"
        );

        // And exactly at the ceiling still parses, so the test above
        // failed for the length and not for the shape.
        let doc = format!(
            r#"{{"schema_version":2,
                 "trust":{{"policy":"static-allowlist","allowed_peers":["{P1}"]}},
                 "endpoints":{{"entries":[]}},
                 "channels":{{"desired":[{}]}}}}"#,
            names[..MAX_DESIRED_CHANNELS].join(",")
        );
        let parsed: ProfileConfig = serde_json::from_str(&doc).expect("at the ceiling");
        assert_eq!(parsed.channels.desired.len(), MAX_DESIRED_CHANNELS);
    }

    #[test]
    fn a_repeated_desired_channel_is_reported_and_names_itself() {
        // Not collapsed. The document shape is a LIST, and deduplicating
        // silently would let a profile claim two hundred channels while
        // holding one hundred and twenty-eight -- the ceiling reading as
        // satisfied when it is not.
        let errors = with_channels(&["general", "general"]).validate();
        assert_eq!(
            errors,
            vec![ConfigError::DuplicateDesiredChannel {
                id: ChannelId::parse("general").expect("valid")
            }]
        );
        assert!(
            errors[0].to_string().contains("general"),
            "an operator needs to know WHICH one: {}",
            errors[0]
        );
    }

    #[test]
    fn channels_differing_only_in_case_are_not_duplicates() {
        // ADR-0025 makes ChannelId case-sensitive, so these are two
        // channels and desiring both is legal.
        assert!(with_channels(&["general", "General"]).validate().is_empty());
    }

    #[test]
    fn more_desired_channels_than_the_ceiling_is_reported() {
        let names: Vec<String> = (0..=MAX_DESIRED_CHANNELS)
            .map(|i| format!("c{i}"))
            .collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let errors = with_channels(&refs).validate();
        assert!(
            errors.contains(&ConfigError::TooManyDesiredChannels {
                got: MAX_DESIRED_CHANNELS + 1
            }),
            "got {errors:?}"
        );
        // And exactly at the ceiling is fine.
        assert!(
            with_channels(&refs[..MAX_DESIRED_CHANNELS])
                .validate()
                .is_empty()
        );
    }
    #[test]
    fn an_oversized_static_peer_list_is_refused_while_reading() {
        // The bound has to apply to the INPUT. `validate` catches this
        // too, but only after every entry has been materialized, which is
        // the cost the ceiling exists to prevent.
        let peers: Vec<String> = (0..MAX_STATIC_BOOTSTRAP_PEERS + 1)
            .map(|i| format!("/ip4/10.0.0.1/tcp/{i}/p2p/{P1}"))
            .collect();
        let json = serde_json::json!({
            "providers": [{
                "type": "static-bootstrap",
                "priority": 10,
                "enabled": true,
                "config": { "peers": peers }
            }]
        });

        let err = serde_json::from_value::<DiscoveryConfig>(json)
            .expect_err("the list is refused as it is read");
        assert!(
            err.to_string().contains("static bootstrap peers"),
            "the error names the limit that refused it, got: {err}"
        );
    }

    #[test]
    fn an_oversized_static_peer_entry_is_refused_while_reading() {
        // A bounded count of unbounded strings is not a bound.
        let json = serde_json::json!({
            "providers": [{
                "type": "static-bootstrap",
                "priority": 10,
                "enabled": true,
                "config": { "peers": ["/ip4/10.0.0.1/".to_owned()
                    + &"x".repeat(MAX_STATIC_PEER_BYTES)] }
            }]
        });

        let err = serde_json::from_value::<DiscoveryConfig>(json)
            .expect_err("the entry is refused as it is read");
        assert!(
            err.to_string().contains("bytes"),
            "the error names the byte ceiling, got: {err}"
        );
    }

    #[test]
    fn a_static_peer_list_within_both_bounds_still_parses() {
        // The positive control: the visitor must not refuse legal input.
        let peers: Vec<String> = (0..MAX_STATIC_BOOTSTRAP_PEERS)
            .map(|i| format!("/ip4/10.0.0.1/tcp/{i}/p2p/{P1}"))
            .collect();
        let json = serde_json::json!({
            "providers": [{
                "type": "static-bootstrap",
                "priority": 10,
                "enabled": true,
                "config": { "peers": peers }
            }]
        });

        let parsed: DiscoveryConfig =
            serde_json::from_value(json).expect("exactly at the limit is legal");
        assert_eq!(
            parsed.providers[0].config.peers.len(),
            MAX_STATIC_BOOTSTRAP_PEERS
        );
    }
    /// A deserializer that records which string method the seed asked
    /// for, so the "refused before it is owned" claim is checked rather
    /// than asserted in a comment.
    ///
    /// `deserialize_str` is the borrowed path — the visitor sees a slice
    /// of the parser's buffer. `deserialize_string` is the owned one,
    /// which is what `next_element::<String>()` takes and what makes an
    /// oversized entry cost its full length before anything rejects it.
    struct Probe<'a> {
        asked: &'a std::cell::Cell<&'static str>,
        value: &'a str,
    }

    impl<'de> serde::Deserializer<'de> for Probe<'_> {
        type Error = serde::de::value::Error;

        fn deserialize_str<V: serde::de::Visitor<'de>>(
            self,
            v: V,
        ) -> Result<V::Value, Self::Error> {
            self.asked.set("str");
            v.visit_str(self.value)
        }

        fn deserialize_string<V: serde::de::Visitor<'de>>(
            self,
            v: V,
        ) -> Result<V::Value, Self::Error> {
            self.asked.set("string");
            v.visit_string(self.value.to_owned())
        }

        fn deserialize_any<V: serde::de::Visitor<'de>>(
            self,
            _v: V,
        ) -> Result<V::Value, Self::Error> {
            Err(serde::de::Error::custom("the probe only answers strings"))
        }

        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 char bytes byte_buf
            option unit unit_struct newtype_struct seq tuple tuple_struct
            map struct enum identifier ignored_any
        }
    }

    #[test]
    fn a_static_peer_entry_is_refused_before_it_is_owned() {
        use serde::de::DeserializeSeed;

        let asked = std::cell::Cell::new("");
        let oversized = "x".repeat(MAX_STATIC_PEER_BYTES + 1);
        let err = BoundedStr
            .deserialize(Probe {
                asked: &asked,
                value: &oversized,
            })
            .expect_err("over the ceiling");

        assert_eq!(
            asked.get(),
            "str",
            "the entry is read through the BORROWED path; asking for a \
             `String` is what allocates the oversized value in full"
        );
        assert!(err.to_string().contains("bytes"), "got: {err}");
    }

    #[test]
    fn a_legal_static_peer_entry_still_comes_through_the_probe() {
        use serde::de::DeserializeSeed;

        let asked = std::cell::Cell::new("");
        let got = BoundedStr
            .deserialize(Probe {
                asked: &asked,
                value: "/ip4/10.0.0.1/tcp/1",
            })
            .expect("within the ceiling");
        assert_eq!(got, "/ip4/10.0.0.1/tcp/1");
        assert_eq!(asked.get(), "str");
    }
    #[test]
    fn a_disabled_kademlia_entry_carrying_its_documented_config_parses() {
        // Staging a profile ahead of the build is legal and is how an
        // operator prepares for Stage 10. `deny_unknown_fields` made it a
        // parse error, which fires BEFORE validate — so the entry could
        // not even be present, let alone disabled.
        let json = serde_json::json!({
            "providers": [{
                "type": "kademlia",
                "priority": 10,
                "enabled": false,
                "priority": 40,
                "config": {
                    "config_version": 1,
                    "network_id": "example-private-network",
                    "mode": "client",
                    "routing_peer_policy": "data-plane-trusted",
                    "seed_sources": ["peer-cache", "static-bootstrap"],
                    "candidate_ttl": "30m",
                    "kbucket_size": 20,
                    "max_routing_peers": 256,
                    "query_timeout": "30s",
                    "parallelism": 3,
                    "disjoint_query_paths": true,
                    "max_concurrent_queries": 2,
                    "max_queries_per_minute": 6
                }
            }]
        });

        let parsed: DiscoveryConfig =
            serde_json::from_value(json).expect("the documented namespace is representable");
        assert_eq!(
            parsed.providers[0].config.network_id.as_deref(),
            Some("example-private-network")
        );
        assert_eq!(parsed.providers[0].config.seed_sources.len(), 2);
    }

    #[test]
    fn an_enabled_kademlia_entry_reaches_the_refusal_it_was_meant_to_get() {
        // The point of modelling the block: the operator reads the
        // reasoned refusal, not a serde error about an unknown key.
        let json = serde_json::json!({
            "providers": [{
                "type": "kademlia",
                "priority": 10,
                "enabled": true,
                "config": { "network_id": "n", "mode": "client" }
            }]
        });
        let parsed: DiscoveryConfig =
            serde_json::from_value(json).expect("parses, so validation can speak");

        let mut profile = config(vec![endpoint("chat")]);
        profile.discovery = parsed;
        let errors = profile.validate();
        assert!(
            errors
                .iter()
                .any(|e| format!("{e}").to_lowercase().contains("kademlia")),
            "the refusal names the provider: {errors:?}"
        );
    }
    #[test]
    fn an_oversized_provider_array_is_refused_while_reading() {
        // Each element is a whole provider config carrying its own nested
        // lists, so materializing the array before `validate` sees it is
        // the expensive version of the same mistake as the peer list.
        let providers: Vec<_> = (0..MAX_DISCOVERY_PROVIDERS + 1)
            .map(|_| serde_json::json!({ "type": "mdns", "enabled": false, "priority": 10 }))
            .collect();
        let err = serde_json::from_value::<DiscoveryConfig>(
            serde_json::json!({ "providers": providers }),
        )
        .expect_err("refused as it is read");
        assert!(
            err.to_string().contains("discovery providers"),
            "the error names the limit: {err}"
        );
    }

    #[test]
    fn a_provider_array_at_the_limit_still_parses() {
        let providers: Vec<_> = (0..MAX_DISCOVERY_PROVIDERS)
            .map(|_| serde_json::json!({ "type": "mdns", "enabled": false, "priority": 10 }))
            .collect();
        let parsed: DiscoveryConfig =
            serde_json::from_value(serde_json::json!({ "providers": providers }))
                .expect("exactly at the limit is legal");
        assert_eq!(parsed.providers.len(), MAX_DISCOVERY_PROVIDERS);
    }
    #[test]
    fn the_canonical_kademlia_profile_deserializes_in_full() {
        // Built from every key in the `kademlia` block of
        // `architecture/config/config.schema.yaml`. The previous round
        // modelled the block from a TRUNCATED read of the schema and
        // stopped at `max_queries_per_minute`, so the canonical profile
        // still failed on the first field past that point. Listing them
        // all here is what makes the next omission fail a test rather
        // than a profile.
        let json = serde_json::json!({
            "providers": [{
                "type": "kademlia",
                "priority": 10,
                "enabled": false,
                "priority": 40,
                "config": {
                    "config_version": 1,
                    "network_id": "example-private-network",
                    "mode": "client",
                    "routing_peer_policy": "data-plane-trusted",
                    "seed_sources": ["peer-cache", "static-bootstrap"],
                    "candidate_ttl": "30m",
                    "kbucket_size": 20,
                    "max_routing_peers": 256,
                    "query_timeout": "30s",
                    "parallelism": 3,
                    "disjoint_query_paths": true,
                    "max_concurrent_queries": 2,
                    "max_queries_per_minute": 6,
                    "exploration_interval": "60s",
                    "exploration_jitter_percent": 20,
                    "max_results_per_query": 20,
                    "target_routing_peers": 64,
                    "targeted_lookup_cooldown": "5m",
                    "bootstrap_min_interval": "5m",
                    "bootstrap_refresh_interval": "15m",
                    "record_mode": "disabled"
                }
            }]
        });

        let parsed: DiscoveryConfig =
            serde_json::from_value(json).expect("the whole documented namespace parses");
        let config = &parsed.providers[0].config;
        assert_eq!(config.record_mode.as_deref(), Some("disabled"));
        assert_eq!(config.target_routing_peers, Some(64));
        assert_eq!(config.bootstrap_refresh_interval.as_deref(), Some("15m"));
    }
    #[test]
    fn an_oversized_seed_source_list_is_refused_while_reading() {
        let json = serde_json::json!({
            "providers": [{
                "type": "kademlia",
                "priority": 10,
                "enabled": false,
                "config": { "seed_sources":
                    ["peer-cache", "mdns", "static-bootstrap", "peer-cache"] }
            }]
        });
        let err = serde_json::from_value::<DiscoveryConfig>(json)
            .expect_err("four is past the documented maximum of three");
        assert!(err.to_string().contains("seed sources"), "got: {err}");
    }

    #[test]
    fn an_unknown_seed_source_name_is_refused() {
        // A disabled entry is legal on this build and `validate` does not
        // look inside a provider it will not run, so without a check at
        // the parse an arbitrary name simply passed.
        let json = serde_json::json!({
            "providers": [{
                "type": "kademlia",
                "priority": 10,
                "enabled": false,
                "config": { "seed_sources": ["peer-cache", "not-a-provider"] }
            }]
        });
        let err = serde_json::from_value::<DiscoveryConfig>(json).expect_err("the enum is closed");
        assert!(
            err.to_string().contains("not-a-provider"),
            "the error names the offending value: {err}"
        );
    }

    #[test]
    fn the_documented_seed_sources_are_accepted() {
        // The control: every legal value parses, so the check cannot be
        // passing by refusing everything.
        let json = serde_json::json!({
            "providers": [{
                "type": "kademlia",
                "priority": 10,
                "enabled": false,
                "config": { "seed_sources": ["peer-cache", "mdns", "static-bootstrap"] }
            }]
        });
        let parsed: DiscoveryConfig =
            serde_json::from_value(json).expect("all three documented names are legal");
        assert_eq!(parsed.providers[0].config.seed_sources.len(), 3);
    }

    #[test]
    fn kademlia_settings_are_refused_on_every_other_provider() {
        // Every kademlia key, checked against every non-kademlia type.
        // Enumerated rather than sampled because the failure mode is a
        // key that was added to the struct and not to the check — a
        // sample of three would keep passing while the eighteenth went
        // back to being silently accepted.
        let keys: Vec<(&str, serde_json::Value)> = vec![
            ("config_version", serde_json::json!(1)),
            ("network_id", serde_json::json!("n")),
            ("mode", serde_json::json!("client")),
            (
                "routing_peer_policy",
                serde_json::json!("data-plane-trusted"),
            ),
            ("seed_sources", serde_json::json!(["peer-cache"])),
            ("candidate_ttl", serde_json::json!("30m")),
            ("kbucket_size", serde_json::json!(20)),
            ("max_routing_peers", serde_json::json!(256)),
            ("query_timeout", serde_json::json!("30s")),
            ("parallelism", serde_json::json!(3)),
            ("disjoint_query_paths", serde_json::json!(true)),
            ("max_concurrent_queries", serde_json::json!(2)),
            ("max_queries_per_minute", serde_json::json!(6)),
            ("exploration_interval", serde_json::json!("60s")),
            ("exploration_jitter_percent", serde_json::json!(20)),
            ("max_results_per_query", serde_json::json!(20)),
            ("target_routing_peers", serde_json::json!(64)),
            ("targeted_lookup_cooldown", serde_json::json!("5m")),
            ("bootstrap_min_interval", serde_json::json!("5m")),
            ("bootstrap_refresh_interval", serde_json::json!("15m")),
            ("record_mode", serde_json::json!("disabled")),
        ];

        for provider in ["peer-cache", "mdns", "static-bootstrap"] {
            for (key, value) in &keys {
                let json = serde_json::json!({
                    "providers": [{
                        "type": provider,
                        "enabled": false,
                        "priority": 10,
                        "config": { (*key): value }
                    }]
                });
                let parsed: DiscoveryConfig =
                    serde_json::from_value(json).expect("the shared struct parses it");
                let mut profile = config(vec![endpoint("chat")]);
                profile.discovery = parsed;

                assert!(
                    profile.validate().iter().any(|e| matches!(
                        e,
                        ConfigError::KademliaSettingsOnWrongProvider { field, .. }
                            if field == key
                    )),
                    "'{key}' on '{provider}' must be refused and named"
                );
            }
        }
    }

    #[test]
    fn kademlia_settings_are_accepted_on_kademlia() {
        // The control: the check must not refuse the keys where they
        // belong.
        let json = serde_json::json!({
            "providers": [{
                "type": "kademlia",
                "priority": 10,
                "enabled": false,
                "config": { "network_id": "n", "record_mode": "disabled" }
            }]
        });
        let mut profile = config(vec![endpoint("chat")]);
        profile.discovery = serde_json::from_value(json).expect("parses");
        assert!(
            !profile
                .validate()
                .iter()
                .any(|e| matches!(e, ConfigError::KademliaSettingsOnWrongProvider { .. })),
            "kademlia keys belong on kademlia"
        );
    }
    #[test]
    fn omitting_enabled_is_refused_for_the_providers_the_schema_requires_it_on() {
        // A blanket `default = true` turned a forgotten field into a
        // runtime behaviour change: a static-bootstrap entry with no
        // `enabled` started dialling the configured peers.
        for provider in ["peer-cache", "mdns", "static-bootstrap"] {
            let json = serde_json::json!({
                "providers": [{ "type": provider, "priority": 10 }]
            });
            let err = serde_json::from_value::<DiscoveryConfig>(json).unwrap_err();
            assert!(
                err.to_string().contains("requires `enabled`"),
                "'{provider}' must require the field: {err}"
            );
        }
    }

    #[test]
    fn kademlia_alone_defaults_enabled_because_the_schema_gives_it_one() {
        let parsed: DiscoveryConfig = serde_json::from_value(serde_json::json!({
            "providers": [{ "type": "kademlia", "priority": 40 }]
        }))
        .expect("kademlia carries `enabled: bool = true`");
        assert!(
            parsed.providers[0].enabled,
            "and the documented default is true"
        );
    }

    #[test]
    fn an_explicit_enabled_is_honoured_on_every_provider() {
        // The control: requiring the field must not stop it being read.
        for (provider, value) in [
            ("peer-cache", true),
            ("mdns", false),
            ("static-bootstrap", true),
            ("kademlia", false),
        ] {
            let parsed: DiscoveryConfig = serde_json::from_value(serde_json::json!({
                "providers": [{ "type": provider, "enabled": value, "priority": 10 }]
            }))
            .expect("an explicit value parses");
            assert_eq!(parsed.providers[0].enabled, value, "for '{provider}'");
        }
    }

    #[test]
    fn an_unknown_provider_field_is_still_refused() {
        // Hand-written `Deserialize` means `deny_unknown_fields` is now
        // spelled out rather than derived, so it owes its own test.
        let err = serde_json::from_value::<DiscoveryConfig>(serde_json::json!({
            "providers": [{ "type": "mdns", "enabled": true, "prioriti": 3 }]
        }))
        .unwrap_err();
        assert!(
            err.to_string().contains("prioriti"),
            "a misspelled key is named, not ignored: {err}"
        );
    }
    /// A disabled kademlia profile carrying one setting.
    fn kad(key: &str, value: serde_json::Value) -> ProfileConfig {
        let json = serde_json::json!({
            "providers": [{
                "type": "kademlia",
                "priority": 10,
                "enabled": false,
                "config": { key: value }
            }]
        });
        let mut profile = config(vec![endpoint("chat")]);
        profile.discovery = serde_json::from_value(json).expect("parses");
        profile
    }

    #[test]
    fn kademlia_values_are_checked_even_when_the_entry_is_disabled() {
        // Staging a profile ahead of the build is legal, so an invalid
        // value here would otherwise surface only after deployment on a
        // supporting build.
        let bad: Vec<(&str, serde_json::Value)> = vec![
            ("mode", serde_json::json!("typo")),
            ("config_version", serde_json::json!(99)),
            ("routing_peer_policy", serde_json::json!("trust-everyone")),
            ("record_mode", serde_json::json!("enabled")),
            ("network_id", serde_json::json!("Has-Capitals")),
            ("network_id", serde_json::json!("-leading-dash")),
            ("kbucket_size", serde_json::json!(1)),
            ("kbucket_size", serde_json::json!(21)),
            ("max_routing_peers", serde_json::json!(19)),
            ("max_routing_peers", serde_json::json!(1025)),
            ("parallelism", serde_json::json!(0)),
            ("parallelism", serde_json::json!(11)),
            ("max_concurrent_queries", serde_json::json!(9)),
            ("max_queries_per_minute", serde_json::json!(61)),
            ("exploration_jitter_percent", serde_json::json!(51)),
            ("max_results_per_query", serde_json::json!(21)),
            ("target_routing_peers", serde_json::json!(7)),
            ("target_routing_peers", serde_json::json!(257)),
            ("query_timeout", serde_json::json!("4s")),
            ("query_timeout", serde_json::json!("121s")),
            ("query_timeout", serde_json::json!("not-a-duration")),
            ("exploration_interval", serde_json::json!("29s")),
            ("exploration_interval", serde_json::json!("2h")),
            ("targeted_lookup_cooldown", serde_json::json!("29s")),
            ("bootstrap_min_interval", serde_json::json!("59s")),
            ("bootstrap_refresh_interval", serde_json::json!("25h")),
            ("candidate_ttl", serde_json::json!("garbage")),
        ];

        for (key, value) in bad {
            let profile = kad(key, value.clone());
            assert!(
                profile.validate().iter().any(|e| matches!(
                    e,
                    ConfigError::InvalidKademliaSetting { field, .. } if *field == key
                )),
                "'{key}' = {value} must be refused and named"
            );
        }
    }

    #[test]
    fn the_documented_kademlia_defaults_are_all_accepted() {
        // The control: every value the schema gives as a default, and the
        // bounds themselves, must pass — a checker that refuses the
        // canonical profile is worse than none.
        let good: Vec<(&str, serde_json::Value)> = vec![
            ("mode", serde_json::json!("client")),
            ("mode", serde_json::json!("server")),
            ("config_version", serde_json::json!(1)),
            (
                "routing_peer_policy",
                serde_json::json!("data-plane-trusted"),
            ),
            ("record_mode", serde_json::json!("disabled")),
            ("network_id", serde_json::json!("example-private-network")),
            ("network_id", serde_json::json!("0")),
            ("kbucket_size", serde_json::json!(8)),
            ("kbucket_size", serde_json::json!(20)),
            ("max_routing_peers", serde_json::json!(20)),
            ("max_routing_peers", serde_json::json!(1024)),
            ("parallelism", serde_json::json!(1)),
            ("parallelism", serde_json::json!(10)),
            ("max_concurrent_queries", serde_json::json!(2)),
            ("max_queries_per_minute", serde_json::json!(6)),
            ("exploration_jitter_percent", serde_json::json!(0)),
            ("exploration_jitter_percent", serde_json::json!(50)),
            ("max_results_per_query", serde_json::json!(20)),
            ("target_routing_peers", serde_json::json!(64)),
            ("query_timeout", serde_json::json!("5s")),
            ("query_timeout", serde_json::json!("120s")),
            ("exploration_interval", serde_json::json!("60s")),
            ("exploration_interval", serde_json::json!("1h")),
            ("targeted_lookup_cooldown", serde_json::json!("5m")),
            ("bootstrap_min_interval", serde_json::json!("5m")),
            ("bootstrap_refresh_interval", serde_json::json!("15m")),
            ("bootstrap_refresh_interval", serde_json::json!("24h")),
            ("candidate_ttl", serde_json::json!("30m")),
        ];

        for (key, value) in good {
            let profile = kad(key, value.clone());
            let offending: Vec<_> = profile
                .validate()
                .into_iter()
                .filter(|e| matches!(e, ConfigError::InvalidKademliaSetting { .. }))
                .collect();
            assert!(
                offending.is_empty(),
                "'{key}' = {value} is documented and must be accepted: {offending:?}"
            );
        }
    }
    #[test]
    fn a_malformed_peer_cache_ttl_is_refused() {
        for bad in ["garbage", "", "30x", "-5m"] {
            let json = serde_json::json!({
                "providers": [{
                    "type": "peer-cache",
                    "priority": 10,
                    "enabled": true,
                    "config": { "ttl": bad }
                }]
            });
            let mut profile = config(vec![endpoint("chat")]);
            profile.discovery = serde_json::from_value(json).expect("parses");
            assert!(
                profile
                    .validate()
                    .iter()
                    .any(|e| matches!(e, ConfigError::InvalidCacheSetting { field, .. } if *field == "ttl")),
                "ttl '{bad}' must be refused"
            );
        }
    }

    #[test]
    fn a_documented_peer_cache_ttl_is_accepted() {
        // The control, including the schema's own `7d` — which needed the
        // duration parser to learn `d`.
        for good in ["7d", "24h", "30m", "60s", "500ms", "1000"] {
            let json = serde_json::json!({
                "providers": [{
                    "type": "peer-cache",
                    "priority": 10,
                    "enabled": true,
                    "config": { "ttl": good }
                }]
            });
            let mut profile = config(vec![endpoint("chat")]);
            profile.discovery = serde_json::from_value(json).expect("parses");
            let offending: Vec<_> = profile
                .validate()
                .into_iter()
                .filter(|e| matches!(e, ConfigError::InvalidCacheSetting { .. }))
                .collect();
            assert!(offending.is_empty(), "ttl '{good}' is legal: {offending:?}");
        }
    }
    #[test]
    fn omitting_priority_is_refused_on_every_provider() {
        // Lower sorts first, so a defaulted 0 does not merely fill a gap
        // — it outranks every provider an operator actually thought
        // about. The schema gives no type a default.
        for provider in ["peer-cache", "mdns", "static-bootstrap", "kademlia"] {
            let json = serde_json::json!({
                "providers": [{ "type": provider, "enabled": false }]
            });
            let err = serde_json::from_value::<DiscoveryConfig>(json).unwrap_err();
            assert!(
                err.to_string().contains("requires `priority`"),
                "'{provider}' must require the field: {err}"
            );
        }
    }

    #[test]
    fn an_explicit_priority_is_honoured_including_zero() {
        // The control: requiring the field must not stop it being read,
        // and 0 is a legal value an operator may mean.
        for value in [-5i32, 0, 10, 40] {
            let parsed: DiscoveryConfig = serde_json::from_value(serde_json::json!({
                "providers": [{ "type": "mdns", "enabled": false, "priority": value }]
            }))
            .expect("an explicit priority parses");
            assert_eq!(parsed.providers[0].priority, value);
        }
    }
    #[test]
    fn a_long_address_is_not_rejected_merely_for_carrying_a_peer_id() {
        // A flat 256-byte ceiling on the whole value made a legal address
        // illegal the moment its required `/p2p/<PeerId>` suffix was
        // appended — a limit contradicting the API it feeds, since
        // `StaticEntry` accepts an address of 256 on its own.
        // Built from LEGAL labels: a single 200-byte label is over the
        // 63-byte DNS limit, and this test is about the byte ceiling
        // rather than DNS syntax.
        let name = std::iter::repeat_n("a".repeat(46), 4)
            .collect::<Vec<_>>()
            .join(".");
        let address = format!("/dns4/{name}/tcp/4001");
        assert!(
            address.len() <= MAX_ADDRESS_BYTES,
            "the address alone is within what a candidate address may be"
        );
        let entry = format!("{address}/p2p/{P1}");
        assert!(
            entry.len() > 256,
            "and the whole entry is past the old flat ceiling, or this \
             test proves nothing"
        );

        let json = serde_json::json!({
            "providers": [{
                "type": "static-bootstrap",
                "enabled": true,
                "priority": 20,
                "config": { "peers": [entry] }
            }]
        });
        let mut profile = config(vec![endpoint("chat")]);
        profile.discovery = serde_json::from_value(json).expect("the entry is readable");
        let offending: Vec<_> = profile
            .validate()
            .into_iter()
            .filter(|e| {
                matches!(
                    e,
                    ConfigError::StaticPeerNotPeerQualified { .. }
                        | ConfigError::InvalidStaticPeer { .. }
                )
            })
            .collect();
        assert!(offending.is_empty(), "it is a legal entry: {offending:?}");
    }

    #[test]
    fn an_address_longer_than_a_candidate_address_is_still_refused() {
        // The control: raising the wire ceiling must not widen what is
        // ACCEPTED, or the rejection simply moves to wiring, where it is
        // a startup failure with no line number in it.
        // 244 bytes: within the 253-byte DNS ceiling, so the grammar
        // passes and the ADDRESS limit is what refuses it.
        let name = std::iter::repeat_n("a".repeat(48), 5)
            .collect::<Vec<_>>()
            .join(".");
        let address = format!("/dns4/{name}/tcp/4001");
        let entry = format!("{address}/p2p/{P1}");
        let json = serde_json::json!({
            "providers": [{
                "type": "static-bootstrap",
                "enabled": true,
                "priority": 20,
                "config": { "peers": [entry] }
            }]
        });
        let mut profile = config(vec![endpoint("chat")]);
        profile.discovery = serde_json::from_value(json).expect("within the wire ceiling");
        assert!(
            profile.validate().iter().any(|e| matches!(
                e,
                ConfigError::StaticPeerNotPeerQualified { reason, .. }
                    if reason.contains("longer than a candidate address")
            )),
            "the address half is checked against its own limit"
        );
    }
    #[test]
    fn an_unknown_protocol_fails_config_validation() {
        // static-bootstrap.md: "Invalid PeerId/multiaddress syntax fails
        // config validation." A structural check let this validate,
        // report healthy, and fail at every dial.
        for bad in [
            "/nonsense/1/tcp/4001",
            "/ip4/10.0.0.1/udp/4001",
            "/ip4/999.0.0.1/tcp/4001",
            "/ip4/10.0.0.1/tcp/70000",
            "/ip4/10.0.0.1/tcp/http",
            "/ip4/10.0.0.1",
            "/dns4/host.example/tcp/4001/ws",
            "/ip6/nothex/tcp/4001",
            "/dns4/.leading/tcp/4001",
            "/dns4/a..b/tcp/4001",
            "/dns4/trailing./tcp/4001",
            "/dns4/has space/tcp/4001",
            "/dns4/-leading-hyphen.example/tcp/4001",
            "/dns4/trailing-hyphen-.example/tcp/4001",
            "/dns4/under_score.example/tcp/4001",
        ] {
            let entry = format!("{bad}/p2p/{P1}");
            assert!(
                split_peer_multiaddr(&entry).is_err(),
                "'{bad}' must fail config validation"
            );
        }
    }

    #[test]
    fn every_documented_address_shape_is_accepted() {
        // The control, drawn from the shapes the schema's own examples
        // and static-bootstrap.md use. A validator that refuses the
        // documented profiles is worse than none.
        for good in [
            "/ip4/10.0.0.1/tcp/4001",
            "/ip4/127.0.0.1/tcp/0",
            "/ip6/::1/tcp/4001",
            "/ip6/2001:db8::1/tcp/4001",
            "/dns4/bootstrap.example.net/tcp/4001",
            "/dns6/bootstrap.example.net/tcp/4001",
            "/dns4/host-with-hyphens.example.net/tcp/4001",
            "/dns4/localhost/tcp/4001",
            "/dns4/a1.b2.c3/tcp/4001",
        ] {
            let entry = format!("{good}/p2p/{P1}");
            let (address, id) = split_peer_multiaddr(&entry)
                .unwrap_or_else(|reason| panic!("'{good}' is documented: {reason}"));
            assert_eq!(address, good);
            assert_eq!(id, peer(P1));
        }
    }
    #[test]
    fn a_malformed_ip_literal_fails_config_validation() {
        // Checking the ALPHABET accepted `:::`, which no dial can use. A
        // check describing what a literal looks like will always have
        // another shape like that in it; `std::net` owns both grammars.
        for bad in [
            "/ip6/:::/tcp/4001",
            "/ip6/2001:db8:::1/tcp/4001",
            "/ip6/gggg::1/tcp/4001",
            "/ip4/10.0.0/tcp/4001",
            "/ip4/10.0.0.1.1/tcp/4001",
            "/ip4/010.0.0.1/tcp/4001",
        ] {
            let entry = format!("{bad}/p2p/{P1}");
            assert!(
                split_peer_multiaddr(&entry).is_err(),
                "'{bad}' is not a usable literal and must fail validation"
            );
        }
    }

    #[test]
    fn a_zero_cache_setting_is_refused_where_the_operator_can_read_it() {
        // `CacheLimitsBuilder` rejects both of these, so without a check
        // here the profile validates and the failure surfaces when the
        // cache is built — the wrong place to meet it.
        for (field, settings) in [
            ("ttl", serde_json::json!({ "ttl": "0" })),
            ("max_entries", serde_json::json!({ "max_entries": 0 })),
        ] {
            let json = serde_json::json!({
                "providers": [{
                    "type": "peer-cache",
                    "enabled": true,
                    "priority": 10,
                    "config": settings
                }]
            });
            let mut profile = config(vec![endpoint("chat")]);
            profile.discovery = serde_json::from_value(json).expect("parses");
            assert!(
                profile.validate().iter().any(|e| matches!(
                    e,
                    ConfigError::InvalidCacheSetting { field: f, .. } if *f == field
                )),
                "a zero `{field}` must be refused"
            );
        }
    }

    #[test]
    fn a_nonzero_cache_setting_is_accepted() {
        // The control: only ZERO is refused.
        let json = serde_json::json!({
            "providers": [{
                "type": "peer-cache",
                "enabled": true,
                "priority": 10,
                "config": { "ttl": "7d", "max_entries": 1024 }
            }]
        });
        let mut profile = config(vec![endpoint("chat")]);
        profile.discovery = serde_json::from_value(json).expect("parses");
        assert!(
            !profile
                .validate()
                .iter()
                .any(|e| matches!(e, ConfigError::InvalidCacheSetting { .. })),
            "the documented values are legal"
        );
    }
}
