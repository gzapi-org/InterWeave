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
    /// ending in `/p2p/<PeerId>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
}

/// One configured discovery provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryProviderConfig {
    /// Which provider.
    #[serde(rename = "type")]
    pub provider_type: DiscoveryProviderType,
    /// Whether it runs.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Composition guidance for address selection, never trust
    /// (ADR-0007). Lower sorts first.
    #[serde(default)]
    pub priority: i32,
    /// The provider-specific block.
    #[serde(default)]
    pub config: DiscoveryProviderSettings,
}

/// The `discovery` block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryConfig {
    /// The composed providers.
    #[serde(default = "default_providers")]
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
/// Maximum static bootstrap entries.
pub const MAX_STATIC_BOOTSTRAP_PEERS: usize = 64;

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
    // STRUCTURAL, not the multiaddr grammar itself: the protocol table is
    // a backend concept, and a configuration crate importing libp2p to
    // spell it would invert the layering `crates/api` exists to hold.
    // This rejects `garbage/p2p/<id>` and `/ip4//tcp/1/p2p/<id>` — the
    // shapes an operator actually typos — while `/nonsense/1/p2p/<id>`
    // reaches the dial path, which owns that vocabulary.
    if !address.starts_with('/') {
        return Err("the address does not start with '/'");
    }
    if address
        .split('/')
        .skip(1)
        .any(|component| component.is_empty())
    {
        return Err("the address has an empty component");
    }
    if peer.is_empty() || peer.contains('/') {
        return Err("the /p2p/ component is not a single PeerId");
    }
    let identity = TransportIdentity::parse(peer.to_owned())
        .map_err(|_| "the PeerId is not a valid identity")?;
    Ok((address, identity))
}
/// Longest single static bootstrap entry, in bytes.
///
/// The same ceiling `discovery-api` puts on an opaque address, because
/// that is what the entry becomes.
pub const MAX_STATIC_PEER_BYTES: usize = 256;
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
/// The config documents durations as `<integer><unit>`; this is the only
/// duration a profile carries. A bare integer is also accepted and read as
/// milliseconds, so a JSON producer that emits a number round-trips.
fn parse_duration_ms(text: &str) -> Result<u32, String> {
    let text = text.trim();
    let (digits, unit): (&str, u64) = if let Some(n) = text.strip_suffix("ms") {
        (n, 1)
    } else if let Some(n) = text.strip_suffix('s') {
        (n, 1_000)
    } else if let Some(n) = text.strip_suffix('m') {
        (n, 60_000)
    } else {
        (text, 1)
    };
    let value: u64 = digits
        .trim()
        .parse()
        .map_err(|_| format!("'{text}' is not a duration like 60s, 5m, or 500ms"))?;
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
                    if let Err(reason) = split_peer_multiaddr(peer) {
                        errors.push(ConfigError::StaticPeerNotPeerQualified {
                            entry: peer.clone(),
                            reason,
                        });
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
}
