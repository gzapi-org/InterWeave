// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! The `transport.connectivity` block: the mandatory reachability stack
//! as a profile states it.
//!
//! `config.schema.yaml` is normative for the shape and the ranges here,
//! and this module is the first code to read that section at all — the
//! profile document had no `transport` block before it.
//!
//! # Nothing here constructs a behaviour
//!
//! This is configuration only. Parsing `relay.client.enabled: true` does
//! not build a relay client, and the owner ruled on 2026-09-07 that the
//! connectivity behaviours ship gated off with
//! `BOTTOM-UP-IMPLEMENTATION-PLAN.md` §14's protocol isolation landing
//! first. That fix (`ClassGated<B>`) has landed; this block is the half
//! that was deferred with it.
//!
//! # Why `literal[...]` fields exist here at all
//!
//! The schema marks several values as `literal[true]`, `literal[false]`,
//! `literal[2]` and `literal[1]`. A profile may state them explicitly,
//! so they must PARSE — and it may not state anything else, so a wrong
//! value must be REFUSED. Omitting the fields would do neither: with
//! `deny_unknown_fields` a document spelling out `required: true` would
//! be rejected as carrying an unknown key, and one asking for
//! `required: false` would be rejected with a message about a typo
//! rather than about the rule it breaks.

use std::collections::BTreeSet;

use interweave_discovery_api::MAX_ADDRESS_BYTES;
use interweave_transport_api::TransportIdentity;
use interweave_trust_api::InfrastructureSet;
use serde::{Deserialize, Serialize};

use crate::{ConfigError, de_duration_ms, default_true, ser_duration_ms, split_peer_multiaddr};

/// Static candidate addresses a profile may list per role.
///
/// `list[multiaddr-with-peer-id, max=16]` in the schema, for both
/// `autonat.client.static_servers` and `relay.client.static_relays`.
pub const MAX_STATIC_CANDIDATES: usize = 16;

/// The `transport` block.
///
/// Defaulted as a whole, like `channels` and unlike `trust`: a profile
/// written before this section existed states no opinion about
/// reachability, and every value below has the schema's default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TransportConfig {
    /// The reachability stack.
    #[serde(default)]
    pub connectivity: ConnectivityConfig,
}

/// `transport.connectivity`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectivityConfig {
    /// `literal[true]`: the stack is mandatory for standard v1.
    ///
    /// A reduced build that omits a required client mechanism is
    /// non-standard and must declare that limitation, which is a
    /// statement about the BUILD rather than a value a profile may set.
    #[serde(default = "default_true")]
    pub required: bool,
    /// Protocol-scoped connectivity authorization.
    ///
    /// THE TYPE IS THE BLOCK. `InfrastructureSet` already deserializes
    /// from exactly `{ allowed_peers: [...] }` through its own checked
    /// constructor, with the bounded-sequence guard that judges the
    /// array as it arrives rather than after collecting it. A parallel
    /// `InfrastructureConfig` here would have meant a second ceiling and
    /// a second guard to keep in step with the first.
    ///
    /// So this field is also the answer to "nothing constructs an
    /// `InfrastructureSet`": ADR-0036's second class has been
    /// expressible in code since Stage 5 and in a profile document never.
    #[serde(default)]
    pub infrastructure: InfrastructureSet,
    /// What this profile is willing to advertise.
    #[serde(default)]
    pub address_advertisement: AddressAdvertisementConfig,
    /// AutoNAT v2.
    #[serde(default)]
    pub autonat: AutonatConfig,
    /// Circuit Relay v2.
    #[serde(default)]
    pub relay: RelayConfig,
    /// DCUtR.
    #[serde(default)]
    pub dcutr: DcutrConfig,
}

impl Default for ConnectivityConfig {
    fn default() -> Self {
        Self {
            required: true,
            infrastructure: InfrastructureSet::default(),
            address_advertisement: AddressAdvertisementConfig::default(),
            autonat: AutonatConfig::default(),
            relay: RelayConfig::default(),
            dcutr: DcutrConfig::default(),
        }
    }
}

/// `transport.connectivity.address_advertisement`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddressAdvertisementConfig {
    /// `literal[false]`: a direct address is advertised only once
    /// AutoNAT has verified it.
    #[serde(default)]
    pub advertise_unverified_public_direct: bool,
    /// `literal[true]`: a live reservation's address is advertised while
    /// it is live, and withdrawn when it is lost.
    #[serde(default = "default_true")]
    pub advertise_active_relay_addresses: bool,
}

impl Default for AddressAdvertisementConfig {
    fn default() -> Self {
        Self {
            advertise_unverified_public_direct: false,
            advertise_active_relay_addresses: true,
        }
    }
}

/// `transport.connectivity.autonat`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutonatConfig {
    /// `literal[2]`: v1 is not a thing this profile can ask for.
    #[serde(default = "default_autonat_version")]
    pub version: u32,
    /// The client role, which standard v1 requires.
    #[serde(default)]
    pub client: AutonatClientConfig,
    /// The server role, which is off unless a profile asks for it.
    #[serde(default)]
    pub server: AutonatServerConfig,
}

impl Default for AutonatConfig {
    fn default() -> Self {
        Self {
            version: AUTONAT_VERSION,
            client: AutonatClientConfig::default(),
            server: AutonatServerConfig::default(),
        }
    }
}

/// The only AutoNAT version standard v1 speaks.
pub const AUTONAT_VERSION: u32 = 2;

const fn default_autonat_version() -> u32 {
    AUTONAT_VERSION
}

/// `transport.connectivity.autonat.client`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutonatClientConfig {
    /// `literal[true]`: the client is mandatory.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Servers this profile will probe, by address.
    #[serde(default, deserialize_with = "de_static_candidates")]
    pub static_servers: Vec<String>,
    /// Whether Identify-learned servers may be used as well.
    ///
    /// Off by default, and that is a trust decision rather than a
    /// conservative default: a peer that claims a protocol in Identify
    /// has asserted it, and promoting an assertion to an authorization
    /// is what `CONNECTIVITY.md` makes an explicit opt-in.
    #[serde(default)]
    pub use_authorized_identify_servers: bool,
    /// Distinct authorized servers that must agree before a direct
    /// address counts as verified.
    #[serde(default = "default_required_successes")]
    pub required_distinct_successes: u32,
    /// How long one success stands.
    #[serde(
        rename = "success_evidence_ttl",
        default = "default_evidence_ttl_ms",
        deserialize_with = "de_duration_ms",
        serialize_with = "ser_duration_ms"
    )]
    pub success_evidence_ttl_ms: u32,
    /// Delay before retrying a failed probe.
    #[serde(
        rename = "retry_interval",
        default = "default_autonat_retry_ms",
        deserialize_with = "de_duration_ms",
        serialize_with = "ser_duration_ms"
    )]
    pub retry_interval_ms: u32,
    /// How often verified state is refreshed.
    #[serde(
        rename = "refresh_interval",
        default = "default_refresh_ms",
        deserialize_with = "de_duration_ms",
        serialize_with = "ser_duration_ms"
    )]
    pub refresh_interval_ms: u32,
    /// Probes in flight at once.
    #[serde(default = "default_max_inflight_probes")]
    pub max_inflight_probes: u32,
    /// Candidate addresses offered per cycle.
    #[serde(default = "default_max_candidates_per_cycle")]
    pub max_candidate_addresses_per_cycle: u32,
    /// Per-probe timeout.
    #[serde(
        rename = "timeout",
        default = "default_probe_timeout_ms",
        deserialize_with = "de_duration_ms",
        serialize_with = "ser_duration_ms"
    )]
    pub timeout_ms: u32,
}

impl Default for AutonatClientConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            static_servers: Vec::new(),
            use_authorized_identify_servers: false,
            required_distinct_successes: 2,
            success_evidence_ttl_ms: 15 * 60_000,
            retry_interval_ms: 30_000,
            refresh_interval_ms: 5 * 60_000,
            max_inflight_probes: 2,
            max_candidate_addresses_per_cycle: 4,
            timeout_ms: 15_000,
        }
    }
}

const fn default_required_successes() -> u32 {
    2
}
const fn default_evidence_ttl_ms() -> u32 {
    15 * 60_000
}
const fn default_autonat_retry_ms() -> u32 {
    30_000
}
const fn default_refresh_ms() -> u32 {
    5 * 60_000
}
const fn default_max_inflight_probes() -> u32 {
    2
}
const fn default_max_candidates_per_cycle() -> u32 {
    4
}
const fn default_probe_timeout_ms() -> u32 {
    15_000
}

/// `transport.connectivity.autonat.server`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutonatServerConfig {
    /// Whether this profile answers probes for others.
    #[serde(default)]
    pub enabled: bool,
    /// Probes answered at once.
    #[serde(default = "default_server_concurrent")]
    pub max_concurrent_probes: u32,
    /// Probes one client may ask for per minute.
    #[serde(default = "default_per_client_per_minute")]
    pub max_probes_per_peer_per_minute: u32,
    /// Probes answered per minute across all clients.
    #[serde(default = "default_global_per_minute")]
    pub max_probes_global_per_minute: u32,
    /// Per-probe timeout.
    #[serde(
        rename = "timeout",
        default = "default_probe_timeout_ms",
        deserialize_with = "de_duration_ms",
        serialize_with = "ser_duration_ms"
    )]
    pub timeout_ms: u32,
}

impl Default for AutonatServerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_concurrent_probes: 8,
            max_probes_per_peer_per_minute: 2,
            max_probes_global_per_minute: 60,
            timeout_ms: 15_000,
        }
    }
}

const fn default_server_concurrent() -> u32 {
    8
}
const fn default_per_client_per_minute() -> u32 {
    2
}
const fn default_global_per_minute() -> u32 {
    60
}

/// `transport.connectivity.relay`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RelayConfig {
    /// The client role, which standard v1 requires.
    #[serde(default)]
    pub client: RelayClientConfig,
    /// The server role, which is off unless a profile asks for it.
    #[serde(default)]
    pub server: RelayServerConfig,
}

/// `transport.connectivity.relay.client`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayClientConfig {
    /// `literal[true]`: reservation management is mandatory.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Relays this profile will hold reservations on, by address.
    #[serde(default, deserialize_with = "de_static_candidates")]
    pub static_relays: Vec<String>,
    /// Whether Identify-learned relays may be used as well.
    #[serde(default)]
    pub use_authorized_identify_relays: bool,
    /// Reservations wanted while reachability is private or unknown.
    #[serde(default = "default_targets_private")]
    pub target_reservations_private_or_unknown: u32,
    /// Reservations wanted once reachability is verified public.
    ///
    /// May be zero: a node that knows it is reachable needs no relay,
    /// and saying so is a coherent posture rather than a mistake.
    #[serde(default = "default_targets_public")]
    pub target_reservations_public: u32,
    /// Reservations held at once, whatever the targets say.
    #[serde(default = "default_max_reservations")]
    pub max_reservations: u32,
    /// Shortest wait before retrying a lost reservation.
    #[serde(
        rename = "retry_min",
        default = "default_relay_retry_min_ms",
        deserialize_with = "de_duration_ms",
        serialize_with = "ser_duration_ms"
    )]
    pub retry_min_ms: u32,
    /// Longest wait before retrying a lost reservation.
    #[serde(
        rename = "retry_max",
        default = "default_relay_retry_max_ms",
        deserialize_with = "de_duration_ms",
        serialize_with = "ser_duration_ms"
    )]
    pub retry_max_ms: u32,
    /// How long a direct path is given before a relayed one is raced.
    ///
    /// Zero is legal and means no head start. `DCUTR.md` calls this
    /// spike-tunable rather than a wire invariant.
    #[serde(
        rename = "direct_head_start",
        default = "default_head_start_ms",
        deserialize_with = "de_duration_ms",
        serialize_with = "ser_duration_ms"
    )]
    pub direct_head_start_ms: u32,
}

impl Default for RelayClientConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            static_relays: Vec::new(),
            use_authorized_identify_relays: false,
            target_reservations_private_or_unknown: 2,
            target_reservations_public: 1,
            max_reservations: 4,
            retry_min_ms: 5_000,
            retry_max_ms: 5 * 60_000,
            direct_head_start_ms: 750,
        }
    }
}

const fn default_targets_private() -> u32 {
    2
}
const fn default_targets_public() -> u32 {
    1
}
const fn default_max_reservations() -> u32 {
    4
}
const fn default_relay_retry_min_ms() -> u32 {
    5_000
}
const fn default_relay_retry_max_ms() -> u32 {
    5 * 60_000
}
const fn default_head_start_ms() -> u32 {
    750
}

/// `transport.connectivity.relay.server`.
///
/// The defaults here are `RELAY.md` §8's table, and SPIKE-004 measured
/// that the pinned crate's own defaults do not match it in any row that
/// matters — so these numbers have to be configured onto the behaviour
/// rather than inherited from it. The spike also measured that every
/// per-peer ceiling in that crate refuses on `>` rather than `>=`, so a
/// ceiling of one admits two; subtracting that belongs where the
/// behaviour is configured, not here, and a profile's `1` means one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayServerConfig {
    /// Whether this profile relays for others.
    #[serde(default)]
    pub enabled: bool,
    /// Reservations held for others at once.
    #[serde(default = "default_server_reservations")]
    pub max_reservations: u32,
    /// Reservations one PeerId may hold.
    #[serde(default = "default_server_reservations_per_peer")]
    pub max_reservations_per_peer: u32,
    /// How long one reservation lasts.
    #[serde(
        rename = "reservation_duration",
        default = "default_reservation_duration_ms",
        deserialize_with = "de_duration_ms",
        serialize_with = "ser_duration_ms"
    )]
    pub reservation_duration_ms: u32,
    /// Circuits carried at once.
    #[serde(default = "default_max_circuits")]
    pub max_circuits: u32,
    /// Circuits one source may open.
    #[serde(default = "default_max_circuits_per_peer")]
    pub max_circuits_per_peer: u32,
    /// How long one circuit may live.
    #[serde(
        rename = "max_circuit_duration",
        default = "default_circuit_duration_ms",
        deserialize_with = "de_duration_ms",
        serialize_with = "ser_duration_ms"
    )]
    pub max_circuit_duration_ms: u32,
    /// Bytes one circuit may carry.
    #[serde(default = "default_circuit_bytes")]
    pub max_circuit_bytes: u64,
    /// Control-protocol operations queued at once.
    ///
    /// SPIKE-004 measured that `libp2p-relay`'s `Config` has no field
    /// for this at all, so it cannot be expressed by configuring that
    /// behaviour. A profile may still state it; enforcing it is the
    /// server role's work when it lands.
    #[serde(default = "default_max_pending_control")]
    pub max_pending_control: u32,
}

impl Default for RelayServerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_reservations: 64,
            max_reservations_per_peer: 1,
            reservation_duration_ms: 60 * 60_000,
            max_circuits: 128,
            max_circuits_per_peer: 4,
            max_circuit_duration_ms: 60 * 60_000,
            max_circuit_bytes: 64 * 1024 * 1024,
            max_pending_control: 64,
        }
    }
}

const fn default_server_reservations() -> u32 {
    64
}
const fn default_server_reservations_per_peer() -> u32 {
    1
}
const fn default_reservation_duration_ms() -> u32 {
    60 * 60_000
}
const fn default_max_circuits() -> u32 {
    128
}
const fn default_max_circuits_per_peer() -> u32 {
    4
}
const fn default_circuit_duration_ms() -> u32 {
    60 * 60_000
}
const fn default_circuit_bytes() -> u64 {
    64 * 1024 * 1024
}
const fn default_max_pending_control() -> u32 {
    64
}

/// `transport.connectivity.dcutr`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DcutrConfig {
    /// `literal[true]`: hole punching is mandatory for standard v1.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Attempts in flight across all peers.
    #[serde(default = "default_dcutr_inflight")]
    pub max_inflight: u32,
    /// `literal[1]`: attempts in flight toward ONE peer.
    #[serde(default = "default_dcutr_inflight_per_peer")]
    pub max_inflight_per_peer: u32,
    /// How long a peer waits after a failed attempt.
    #[serde(
        rename = "retry_cooldown",
        default = "default_cooldown_ms",
        deserialize_with = "de_duration_ms",
        serialize_with = "ser_duration_ms"
    )]
    pub retry_cooldown_ms: u32,
    /// How long a new direct path must hold before it is preferred.
    #[serde(
        rename = "direct_stability_period",
        default = "default_stability_ms",
        deserialize_with = "de_duration_ms",
        serialize_with = "ser_duration_ms"
    )]
    pub direct_stability_period_ms: u32,
}

impl Default for DcutrConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_inflight: 4,
            max_inflight_per_peer: DCUTR_INFLIGHT_PER_PEER,
            retry_cooldown_ms: 5 * 60_000,
            direct_stability_period_ms: 10_000,
        }
    }
}

/// The only per-peer attempt ceiling standard v1 allows.
pub const DCUTR_INFLIGHT_PER_PEER: u32 = 1;

const fn default_dcutr_inflight() -> u32 {
    4
}
const fn default_dcutr_inflight_per_peer() -> u32 {
    DCUTR_INFLIGHT_PER_PEER
}
const fn default_cooldown_ms() -> u32 {
    5 * 60_000
}
const fn default_stability_ms() -> u32 {
    10_000
}

/// One static candidate, refused before this crate owns it.
///
/// Bounded like `CandidatePeer::addresses`, for the same reason and with
/// the same constant: the address grammar is libp2p's and a neutral
/// configuration crate does not parse it, but an unbounded string is a
/// resource question rather than a grammar one.
fn de_static_candidates<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<String>::deserialize(deserializer)?;
    if raw.len() > MAX_STATIC_CANDIDATES {
        return Err(serde::de::Error::custom(format!(
            "at most {MAX_STATIC_CANDIDATES} static candidates, got {}",
            raw.len()
        )));
    }
    for address in &raw {
        if address.len() > MAX_ADDRESS_BYTES {
            return Err(serde::de::Error::custom(format!(
                "a static candidate address may be at most {MAX_ADDRESS_BYTES} bytes"
            )));
        }
    }
    Ok(raw)
}

impl ConnectivityConfig {
    /// Every range and cross-field rule the schema states for this block.
    ///
    /// ACCUMULATES, like `ProfileConfig::validate`: an operator fixing a
    /// configuration should see every complaint at once rather than
    /// discover the second after fixing the first.
    ///
    /// `trusted` is the data-plane allowlist, passed in because the rule
    /// about static candidates spans two sections of the document and
    /// this block cannot see the other one.
    pub(crate) fn validate_into(
        &self,
        trusted: &BTreeSet<TransportIdentity>,
        errors: &mut Vec<ConfigError>,
    ) {
        self.check_literals(errors);
        self.check_ranges(errors);
        self.check_cross_fields(errors);
        self.check_static_candidate_trust(trusted, errors);
    }

    /// The values the schema pins to one possibility.
    fn check_literals(&self, errors: &mut Vec<ConfigError>) {
        let pinned: [(&'static str, bool); 5] = [
            ("connectivity.required", self.required),
            (
                "connectivity.address_advertisement.advertise_unverified_public_direct",
                !self
                    .address_advertisement
                    .advertise_unverified_public_direct,
            ),
            (
                "connectivity.address_advertisement.advertise_active_relay_addresses",
                self.address_advertisement.advertise_active_relay_addresses,
            ),
            (
                "connectivity.autonat.client.enabled",
                self.autonat.client.enabled,
            ),
            (
                "connectivity.relay.client.enabled",
                self.relay.client.enabled,
            ),
        ];
        for (field, holds) in pinned {
            if !holds {
                errors.push(ConfigError::ConnectivityLiteralViolated { field });
            }
        }
        if !self.dcutr.enabled {
            errors.push(ConfigError::ConnectivityLiteralViolated {
                field: "connectivity.dcutr.enabled",
            });
        }
        if self.autonat.version != AUTONAT_VERSION {
            errors.push(ConfigError::ConnectivityOutOfRange {
                field: "connectivity.autonat.version",
                got: u64::from(self.autonat.version),
                allowed: (u64::from(AUTONAT_VERSION), u64::from(AUTONAT_VERSION)),
            });
        }
        if self.dcutr.max_inflight_per_peer != DCUTR_INFLIGHT_PER_PEER {
            errors.push(ConfigError::ConnectivityOutOfRange {
                field: "connectivity.dcutr.max_inflight_per_peer",
                got: u64::from(self.dcutr.max_inflight_per_peer),
                allowed: (
                    u64::from(DCUTR_INFLIGHT_PER_PEER),
                    u64::from(DCUTR_INFLIGHT_PER_PEER),
                ),
            });
        }
    }

    /// Every `integer[a..b]`, `duration[a..b]` and `bytes[a..b]` range.
    ///
    /// Table-driven rather than thirty near-identical blocks, so a row
    /// added to the schema is a row added here and the shape of the
    /// check cannot drift between fields.
    fn check_ranges(&self, errors: &mut Vec<ConfigError>) {
        let autonat_client = &self.autonat.client;
        let autonat_server = &self.autonat.server;
        let relay_client = &self.relay.client;
        let relay_server = &self.relay.server;
        let dcutr = &self.dcutr;
        let rows: [(&'static str, u64, u64, u64); 26] = [
            (
                "connectivity.autonat.client.required_distinct_successes",
                u64::from(autonat_client.required_distinct_successes),
                1,
                4,
            ),
            (
                "connectivity.autonat.client.success_evidence_ttl",
                u64::from(autonat_client.success_evidence_ttl_ms),
                60_000,
                3_600_000,
            ),
            (
                "connectivity.autonat.client.retry_interval",
                u64::from(autonat_client.retry_interval_ms),
                10_000,
                300_000,
            ),
            (
                "connectivity.autonat.client.refresh_interval",
                u64::from(autonat_client.refresh_interval_ms),
                60_000,
                1_800_000,
            ),
            (
                "connectivity.autonat.client.max_inflight_probes",
                u64::from(autonat_client.max_inflight_probes),
                1,
                8,
            ),
            (
                "connectivity.autonat.client.max_candidate_addresses_per_cycle",
                u64::from(autonat_client.max_candidate_addresses_per_cycle),
                1,
                16,
            ),
            (
                "connectivity.autonat.client.timeout",
                u64::from(autonat_client.timeout_ms),
                5_000,
                60_000,
            ),
            (
                "connectivity.autonat.server.max_concurrent_probes",
                u64::from(autonat_server.max_concurrent_probes),
                1,
                64,
            ),
            (
                "connectivity.autonat.server.max_probes_per_peer_per_minute",
                u64::from(autonat_server.max_probes_per_peer_per_minute),
                1,
                30,
            ),
            (
                "connectivity.autonat.server.max_probes_global_per_minute",
                u64::from(autonat_server.max_probes_global_per_minute),
                1,
                600,
            ),
            (
                "connectivity.autonat.server.timeout",
                u64::from(autonat_server.timeout_ms),
                5_000,
                60_000,
            ),
            (
                "connectivity.relay.client.target_reservations_private_or_unknown",
                u64::from(relay_client.target_reservations_private_or_unknown),
                1,
                4,
            ),
            (
                "connectivity.relay.client.target_reservations_public",
                u64::from(relay_client.target_reservations_public),
                0,
                4,
            ),
            (
                "connectivity.relay.client.max_reservations",
                u64::from(relay_client.max_reservations),
                1,
                8,
            ),
            (
                "connectivity.relay.client.retry_min",
                u64::from(relay_client.retry_min_ms),
                1_000,
                60_000,
            ),
            (
                "connectivity.relay.client.retry_max",
                u64::from(relay_client.retry_max_ms),
                30_000,
                1_800_000,
            ),
            (
                "connectivity.relay.client.direct_head_start",
                u64::from(relay_client.direct_head_start_ms),
                0,
                5_000,
            ),
            (
                "connectivity.relay.server.max_reservations",
                u64::from(relay_server.max_reservations),
                1,
                512,
            ),
            (
                "connectivity.relay.server.max_reservations_per_peer",
                u64::from(relay_server.max_reservations_per_peer),
                1,
                4,
            ),
            (
                "connectivity.relay.server.reservation_duration",
                u64::from(relay_server.reservation_duration_ms),
                300_000,
                86_400_000,
            ),
            (
                "connectivity.relay.server.max_circuits",
                u64::from(relay_server.max_circuits),
                1,
                1_024,
            ),
            (
                "connectivity.relay.server.max_circuits_per_peer",
                u64::from(relay_server.max_circuits_per_peer),
                1,
                16,
            ),
            (
                "connectivity.relay.server.max_circuit_duration",
                u64::from(relay_server.max_circuit_duration_ms),
                60_000,
                86_400_000,
            ),
            (
                "connectivity.relay.server.max_circuit_bytes",
                relay_server.max_circuit_bytes,
                1024 * 1024,
                1024 * 1024 * 1024,
            ),
            (
                "connectivity.relay.server.max_pending_control",
                u64::from(relay_server.max_pending_control),
                1,
                512,
            ),
            (
                "connectivity.dcutr.max_inflight",
                u64::from(dcutr.max_inflight),
                1,
                32,
            ),
        ];
        for (field, got, min, max) in rows {
            if got < min || got > max {
                errors.push(ConfigError::ConnectivityOutOfRange {
                    field,
                    got,
                    allowed: (min, max),
                });
            }
        }
        // DURATIONS TOO, and separately, because `retry_cooldown` and
        // `direct_stability_period` are the two the DCUtR bounds rest on
        // and the spike measured that the crate exposes neither.
        let durations: [(&'static str, u64, u64, u64); 2] = [
            (
                "connectivity.dcutr.retry_cooldown",
                u64::from(dcutr.retry_cooldown_ms),
                30_000,
                3_600_000,
            ),
            (
                "connectivity.dcutr.direct_stability_period",
                u64::from(dcutr.direct_stability_period_ms),
                1_000,
                120_000,
            ),
        ];
        for (field, got, min, max) in durations {
            if got < min || got > max {
                errors.push(ConfigError::ConnectivityOutOfRange {
                    field,
                    got,
                    allowed: (min, max),
                });
            }
        }
    }

    /// The schema's own "Cross-field validation" list.
    fn check_cross_fields(&self, errors: &mut Vec<ConfigError>) {
        let relay_client = &self.relay.client;
        let relay_server = &self.relay.server;
        let pairs: [(&'static str, u64, &'static str, u64); 6] = [
            (
                "connectivity.relay.client.target_reservations_private_or_unknown",
                u64::from(relay_client.target_reservations_private_or_unknown),
                "connectivity.relay.client.max_reservations",
                u64::from(relay_client.max_reservations),
            ),
            (
                "connectivity.relay.client.target_reservations_public",
                u64::from(relay_client.target_reservations_public),
                "connectivity.relay.client.max_reservations",
                u64::from(relay_client.max_reservations),
            ),
            (
                "connectivity.relay.client.target_reservations_public",
                u64::from(relay_client.target_reservations_public),
                "connectivity.relay.client.target_reservations_private_or_unknown",
                u64::from(relay_client.target_reservations_private_or_unknown),
            ),
            (
                "connectivity.relay.client.retry_min",
                u64::from(relay_client.retry_min_ms),
                "connectivity.relay.client.retry_max",
                u64::from(relay_client.retry_max_ms),
            ),
            (
                "connectivity.relay.server.max_reservations_per_peer",
                u64::from(relay_server.max_reservations_per_peer),
                "connectivity.relay.server.max_reservations",
                u64::from(relay_server.max_reservations),
            ),
            (
                "connectivity.relay.server.max_circuits_per_peer",
                u64::from(relay_server.max_circuits_per_peer),
                "connectivity.relay.server.max_circuits",
                u64::from(relay_server.max_circuits),
            ),
        ];
        for (lesser, lesser_got, greater, greater_got) in pairs {
            if lesser_got > greater_got {
                errors.push(ConfigError::ConnectivityOrderViolated {
                    lesser,
                    lesser_got,
                    greater,
                    greater_got,
                });
            }
        }
        // `dcutr.max_inflight_per_peer <= dcutr.max_inflight` is the
        // seventh rule. It is checked rather than assumed even though
        // the per-peer value is pinned to one: the pin is a literal
        // check that ACCUMULATES, so a document violating both arrives
        // here with `max_inflight_per_peer` still whatever it said.
        if u64::from(self.dcutr.max_inflight_per_peer) > u64::from(self.dcutr.max_inflight) {
            errors.push(ConfigError::ConnectivityOrderViolated {
                lesser: "connectivity.dcutr.max_inflight_per_peer",
                lesser_got: u64::from(self.dcutr.max_inflight_per_peer),
                greater: "connectivity.dcutr.max_inflight",
                greater_got: u64::from(self.dcutr.max_inflight),
            });
        }
    }

    /// Every static candidate must be peer-qualified and authorized.
    ///
    /// The schema's rule: a static relay or AutoNAT server PeerId is in
    /// `trust.allowed_peers` or in
    /// `transport.connectivity.infrastructure.allowed_peers`. A peer in
    /// both is data-plane trusted, which needs no check here — the UNION
    /// is what authorizes a candidate, and this walks each one against
    /// it rather than asking whether either set is non-empty.
    ///
    /// FAILS CLOSED, which is the point of the rule rather than a
    /// convenience: a configured relay the profile authorized nowhere
    /// would be dialled under a reachability origin and refused by the
    /// gate, and SPIKE-004 measured that a gate refusal of a
    /// behaviour-originated dial surfaces as NOTHING — the Swarm
    /// discards it and only the originating behaviour is told. The
    /// operator would see a relay that never connects and no reason
    /// anywhere. A configuration error with a field name is the only
    /// place that is legible.
    ///
    /// PEER-QUALIFIED FIRST, reusing [`split_peer_multiaddr`] rather
    /// than parsing here: a bare `/ip4/.../tcp/4001` names no peer, so
    /// there is nothing to check membership for, and the schema's type
    /// is `multiaddr-with-peer-id`. The same function the static
    /// bootstrap provider's entries go through, with the same grammar
    /// check, because a candidate that cannot be dialled is a
    /// configuration fault either way.
    fn check_static_candidate_trust(
        &self,
        trusted: &BTreeSet<TransportIdentity>,
        errors: &mut Vec<ConfigError>,
    ) {
        for (role, candidates) in [
            (
                "autonat.client.static_servers",
                &self.autonat.client.static_servers,
            ),
            (
                "relay.client.static_relays",
                &self.relay.client.static_relays,
            ),
        ] {
            for candidate in candidates {
                match split_peer_multiaddr(candidate) {
                    Err(reason) => errors.push(ConfigError::StaticCandidateNotPeerQualified {
                        role,
                        entry: candidate.clone(),
                        reason,
                    }),
                    Ok((_, peer)) => {
                        if !trusted.contains(&peer)
                            && !self.infrastructure.permits_control_connection(&peer)
                        {
                            errors.push(ConfigError::StaticCandidateUnauthorized { role, peer });
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProfileConfig;

    const P1: &str = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    const P2: &str = "12D3KooWK99VoVxNE7XzyBwXEzW7xhK7Gpv85r9F3V3fyKSUKPH5";

    /// A document with the reachability block spelled `body`.
    ///
    /// `trust.allowed_peers` is EMPTY here on purpose: the static
    /// candidate rule reads the union of the two sets, and a test that
    /// pre-trusted everything could not see it fail.
    fn profile_with(body: &str) -> Result<ProfileConfig, serde_json::Error> {
        serde_json::from_str::<ProfileConfig>(&format!(
            r#"{{"schema_version":2,
                 "trust":{{"policy":"static-allowlist","allowed_peers":[]}},
                 "endpoints":{{"entries":[]}},
                 "transport":{{"connectivity":{body}}}}}"#
        ))
    }

    fn errors_for(body: &str) -> Vec<ConfigError> {
        profile_with(body).expect("the document parses").validate()
    }

    #[test]
    fn a_profile_with_no_transport_block_gets_standard_v1_clients_and_no_servers() {
        // THE DEFAULT POSTURE, and it is the one that matters most:
        // every profile written before this section existed takes this
        // path, so a wrong default here changes the meaning of documents
        // nobody edited.
        let config = serde_json::from_str::<ProfileConfig>(
            r#"{"schema_version":2,
                "trust":{"policy":"static-allowlist","allowed_peers":[]},
                "endpoints":{"entries":[]}}"#,
        )
        .expect("a document without a transport block still parses");
        let connectivity = &config.transport.connectivity;

        assert!(connectivity.required, "standard v1 requires the stack");
        assert!(connectivity.autonat.client.enabled);
        assert!(connectivity.relay.client.enabled);
        assert!(connectivity.dcutr.enabled);
        assert!(
            !connectivity.autonat.server.enabled,
            "the AutoNAT server role is opt-in"
        );
        assert!(
            !connectivity.relay.server.enabled,
            "the relay server role is opt-in"
        );
        assert!(
            connectivity.infrastructure.is_empty(),
            "no peer is authorized for reachability control by default"
        );
        assert!(
            config.is_valid(),
            "the defaults must satisfy every rule they are checked against: {:?}",
            config.validate()
        );
    }

    #[test]
    fn the_defaults_are_the_schemas_defaults() {
        // Each number read from `config.schema.yaml`'s
        // `transport.connectivity` block rather than from the struct, so
        // a default changed in code without changing the schema fails
        // here.
        let c = ConnectivityConfig::default();
        assert_eq!(c.autonat.version, 2);
        assert_eq!(c.autonat.client.required_distinct_successes, 2);
        assert_eq!(c.autonat.client.success_evidence_ttl_ms, 900_000);
        assert_eq!(c.autonat.client.retry_interval_ms, 30_000);
        assert_eq!(c.autonat.client.refresh_interval_ms, 300_000);
        assert_eq!(c.autonat.client.max_inflight_probes, 2);
        assert_eq!(c.autonat.client.max_candidate_addresses_per_cycle, 4);
        assert_eq!(c.autonat.client.timeout_ms, 15_000);
        assert_eq!(c.autonat.server.max_concurrent_probes, 8);
        assert_eq!(c.autonat.server.max_probes_per_peer_per_minute, 2);
        assert_eq!(c.autonat.server.max_probes_global_per_minute, 60);
        assert_eq!(c.relay.client.target_reservations_private_or_unknown, 2);
        assert_eq!(c.relay.client.target_reservations_public, 1);
        assert_eq!(c.relay.client.max_reservations, 4);
        assert_eq!(c.relay.client.retry_min_ms, 5_000);
        assert_eq!(c.relay.client.retry_max_ms, 300_000);
        assert_eq!(c.relay.client.direct_head_start_ms, 750);
        assert_eq!(c.relay.server.max_reservations, 64);
        assert_eq!(c.relay.server.max_reservations_per_peer, 1);
        assert_eq!(c.relay.server.reservation_duration_ms, 3_600_000);
        assert_eq!(c.relay.server.max_circuits, 128);
        assert_eq!(c.relay.server.max_circuits_per_peer, 4);
        assert_eq!(c.relay.server.max_circuit_duration_ms, 3_600_000);
        assert_eq!(c.relay.server.max_circuit_bytes, 67_108_864);
        assert_eq!(c.relay.server.max_pending_control, 64);
        assert_eq!(c.dcutr.max_inflight, 4);
        assert_eq!(c.dcutr.max_inflight_per_peer, 1);
        assert_eq!(c.dcutr.retry_cooldown_ms, 300_000);
        assert_eq!(c.dcutr.direct_stability_period_ms, 10_000);
    }

    #[test]
    fn every_pinned_value_is_refused_when_a_profile_changes_it() {
        // The five booleans and the two numbers the schema pins. Each is
        // asserted SEPARATELY rather than in one document, because a
        // single document would pass with only one check working.
        for (body, field) in [
            (r#"{"required":false}"#, "connectivity.required"),
            (
                r#"{"address_advertisement":{"advertise_unverified_public_direct":true}}"#,
                "connectivity.address_advertisement.advertise_unverified_public_direct",
            ),
            (
                r#"{"address_advertisement":{"advertise_active_relay_addresses":false}}"#,
                "connectivity.address_advertisement.advertise_active_relay_addresses",
            ),
            (
                r#"{"autonat":{"client":{"enabled":false}}}"#,
                "connectivity.autonat.client.enabled",
            ),
            (
                r#"{"relay":{"client":{"enabled":false}}}"#,
                "connectivity.relay.client.enabled",
            ),
            (
                r#"{"dcutr":{"enabled":false}}"#,
                "connectivity.dcutr.enabled",
            ),
        ] {
            let errors = errors_for(body);
            assert!(
                errors.iter().any(|e| matches!(
                    e,
                    ConfigError::ConnectivityLiteralViolated { field: f } if *f == field
                )),
                "{field} must be refused, got {errors:?}"
            );
        }

        // The two pinned NUMBERS report a range rather than a literal,
        // because a number invites a different number.
        for (body, field) in [
            (
                r#"{"autonat":{"version":1}}"#,
                "connectivity.autonat.version",
            ),
            (
                r#"{"dcutr":{"max_inflight_per_peer":2}}"#,
                "connectivity.dcutr.max_inflight_per_peer",
            ),
        ] {
            let errors = errors_for(body);
            assert!(
                errors.iter().any(|e| matches!(
                    e,
                    ConfigError::ConnectivityOutOfRange { field: f, .. } if *f == field
                )),
                "{field} must be refused, got {errors:?}"
            );
        }
    }

    #[test]
    fn every_range_is_refused_at_both_edges_and_accepted_inside() {
        // TABLE-DRIVEN LIKE THE CHECK, and deliberately so: a row the
        // check gained without a row here would be untested, and the
        // pairing is visible when both are lists.
        //
        // Each row is (json body template, field, below, inside, above).
        // `{}` is where the value goes, so one row exercises all three.
        let rows: [(&str, &str, i64, i64, i64); 8] = [
            (
                r#"{"autonat":{"client":{"required_distinct_successes":{}}}}"#,
                "connectivity.autonat.client.required_distinct_successes",
                0,
                1,
                5,
            ),
            (
                r#"{"autonat":{"client":{"max_inflight_probes":{}}}}"#,
                "connectivity.autonat.client.max_inflight_probes",
                0,
                8,
                9,
            ),
            (
                r#"{"autonat":{"server":{"max_probes_per_peer_per_minute":{}}}}"#,
                "connectivity.autonat.server.max_probes_per_peer_per_minute",
                0,
                30,
                31,
            ),
            (
                r#"{"relay":{"client":{"max_reservations":{}}}}"#,
                "connectivity.relay.client.max_reservations",
                0,
                8,
                9,
            ),
            (
                r#"{"relay":{"server":{"max_circuits":{}}}}"#,
                "connectivity.relay.server.max_circuits",
                0,
                1_024,
                1_025,
            ),
            (
                r#"{"relay":{"server":{"max_circuit_bytes":{}}}}"#,
                "connectivity.relay.server.max_circuit_bytes",
                1_048_575,
                1_073_741_824,
                1_073_741_825,
            ),
            (
                r#"{"dcutr":{"max_inflight":{}}}"#,
                "connectivity.dcutr.max_inflight",
                0,
                32,
                33,
            ),
            (
                r#"{"autonat":{"client":{"timeout":{}}}}"#,
                "connectivity.autonat.client.timeout",
                4_999,
                60_000,
                60_001,
            ),
        ];
        for (template, field, below, inside, above) in rows {
            for bad in [below, above] {
                let body = template.replace("{}", &bad.to_string());
                let errors = errors_for(&body);
                assert!(
                    errors.iter().any(|e| matches!(
                        e,
                        ConfigError::ConnectivityOutOfRange { field: f, .. } if *f == field
                    )),
                    "{field} = {bad} must be refused, got {errors:?}"
                );
            }
            // THE CONTROL. Without it a check that refused everything
            // would pass every assertion above.
            let body = template.replace("{}", &inside.to_string());
            let errors = errors_for(&body);
            assert!(
                !errors.iter().any(|e| matches!(
                    e,
                    ConfigError::ConnectivityOutOfRange { field: f, .. } if *f == field
                )),
                "{field} = {inside} is inside the range and must be accepted, got {errors:?}"
            );
        }
    }

    #[test]
    fn a_duration_is_read_as_text_or_as_milliseconds() {
        // The schema writes `15m`; a bare integer is milliseconds. Both
        // have to reach the same value or the ranges mean two things.
        let text = profile_with(r#"{"autonat":{"client":{"success_evidence_ttl":"30m"}}}"#)
            .expect("a text duration parses");
        let millis = profile_with(r#"{"autonat":{"client":{"success_evidence_ttl":1800000}}}"#)
            .expect("a bare millisecond count parses");
        assert_eq!(
            text.transport
                .connectivity
                .autonat
                .client
                .success_evidence_ttl_ms,
            1_800_000
        );
        assert_eq!(
            text.transport.connectivity.autonat.client,
            millis.transport.connectivity.autonat.client
        );
    }

    #[test]
    fn each_of_the_schemas_seven_cross_field_rules_is_enforced() {
        // Every rule from the schema's own "Cross-field validation"
        // list, each with the ORDER REVERSED so the rule is the only
        // thing that can fail the document.
        let cases: [(&str, &str, &str); 7] = [
            (
                r#"{"relay":{"client":{"target_reservations_private_or_unknown":4,"max_reservations":2,"target_reservations_public":1}}}"#,
                "connectivity.relay.client.target_reservations_private_or_unknown",
                "connectivity.relay.client.max_reservations",
            ),
            (
                r#"{"relay":{"client":{"target_reservations_public":4,"max_reservations":2,"target_reservations_private_or_unknown":4}}}"#,
                "connectivity.relay.client.target_reservations_public",
                "connectivity.relay.client.max_reservations",
            ),
            (
                r#"{"relay":{"client":{"target_reservations_public":3,"target_reservations_private_or_unknown":2}}}"#,
                "connectivity.relay.client.target_reservations_public",
                "connectivity.relay.client.target_reservations_private_or_unknown",
            ),
            (
                r#"{"relay":{"client":{"retry_min":"60s","retry_max":"30s"}}}"#,
                "connectivity.relay.client.retry_min",
                "connectivity.relay.client.retry_max",
            ),
            (
                r#"{"relay":{"server":{"max_reservations_per_peer":4,"max_reservations":2}}}"#,
                "connectivity.relay.server.max_reservations_per_peer",
                "connectivity.relay.server.max_reservations",
            ),
            (
                r#"{"relay":{"server":{"max_circuits_per_peer":16,"max_circuits":8}}}"#,
                "connectivity.relay.server.max_circuits_per_peer",
                "connectivity.relay.server.max_circuits",
            ),
            (
                r#"{"dcutr":{"max_inflight_per_peer":1,"max_inflight":0}}"#,
                "connectivity.dcutr.max_inflight_per_peer",
                "connectivity.dcutr.max_inflight",
            ),
        ];
        for (body, lesser, greater) in cases {
            let errors = errors_for(body);
            assert!(
                errors.iter().any(|e| matches!(
                    e,
                    ConfigError::ConnectivityOrderViolated { lesser: l, greater: g, .. }
                        if *l == lesser && *g == greater
                )),
                "{lesser} must not be allowed to exceed {greater}, got {errors:?}"
            );
        }
    }

    #[test]
    fn a_static_candidate_must_be_authorized_by_one_of_the_two_sets() {
        let entry = |p: &str| format!("/ip4/203.0.113.7/tcp/4001/p2p/{p}");

        // REFUSED: neither set names the peer.
        for (body, role) in [
            (
                format!(
                    r#"{{"autonat":{{"client":{{"static_servers":["{}"]}}}}}}"#,
                    entry(P1)
                ),
                "autonat.client.static_servers",
            ),
            (
                format!(
                    r#"{{"relay":{{"client":{{"static_relays":["{}"]}}}}}}"#,
                    entry(P1)
                ),
                "relay.client.static_relays",
            ),
        ] {
            let errors = errors_for(&body);
            assert!(
                errors.iter().any(|e| matches!(
                    e,
                    ConfigError::StaticCandidateUnauthorized { role: r, peer }
                        if *r == role && peer.as_str() == P1
                )),
                "an unauthorized {role} candidate must be refused, got {errors:?}"
            );
        }

        // ACCEPTED through the INFRASTRUCTURE set, which is the whole
        // point of ADR-0036's second class: a relay is authorized for
        // reachability control without any data-plane trust.
        let by_infrastructure = format!(
            r#"{{"infrastructure":{{"allowed_peers":["{P1}"]}},
                 "relay":{{"client":{{"static_relays":["{}"]}}}}}}"#,
            entry(P1)
        );
        assert!(
            !errors_for(&by_infrastructure)
                .iter()
                .any(|e| matches!(e, ConfigError::StaticCandidateUnauthorized { .. })),
            "the infrastructure set must authorize a static relay"
        );

        // AND through the data-plane set, which the schema also allows:
        // a peer in both is data-plane trusted because that policy says
        // so, so the union is what this rule reads.
        let by_trust = serde_json::from_str::<ProfileConfig>(&format!(
            r#"{{"schema_version":2,
                 "trust":{{"policy":"static-allowlist","allowed_peers":["{P1}"]}},
                 "endpoints":{{"entries":[]}},
                 "transport":{{"connectivity":{{"relay":{{"client":{{"static_relays":["{}"]}}}}}}}}}}"#,
            entry(P1)
        ))
        .expect("the document parses");
        assert!(
            !by_trust
                .validate()
                .iter()
                .any(|e| matches!(e, ConfigError::StaticCandidateUnauthorized { .. })),
            "profile trust must authorize a static relay too"
        );

        // AND THE WRONG PEER IS STILL REFUSED with one authorized, which
        // is the control for the two acceptances above: without it a
        // check that accepted any candidate once any peer was authorized
        // would pass them both.
        let other = format!(
            r#"{{"infrastructure":{{"allowed_peers":["{P1}"]}},
                 "relay":{{"client":{{"static_relays":["{}"]}}}}}}"#,
            entry(P2)
        );
        assert!(
            errors_for(&other).iter().any(|e| matches!(
                e,
                ConfigError::StaticCandidateUnauthorized { peer, .. } if peer.as_str() == P2
            )),
            "authorizing one peer must not authorize another"
        );
    }

    #[test]
    fn a_candidate_without_a_peer_id_is_refused_rather_than_skipped() {
        // Without the `/p2p/` half there is no identity to authorize, so
        // the membership rule has nothing to read -- and silently
        // skipping it would let an unauthorizable candidate through the
        // check that exists to authorize candidates.
        let errors =
            errors_for(r#"{"relay":{"client":{"static_relays":["/ip4/203.0.113.7/tcp/4001"]}}}"#);
        assert!(
            errors.iter().any(|e| matches!(
                e,
                ConfigError::StaticCandidateNotPeerQualified { role, .. }
                    if *role == "relay.client.static_relays"
            )),
            "a bare multiaddr must be refused, got {errors:?}"
        );
    }

    #[test]
    fn the_block_round_trips_through_serde() {
        // Durations are written back as text, so a profile read and
        // written is not silently rewritten into milliseconds -- and the
        // ROUND TRIP is what proves the serializer and deserializer
        // agree about the unit.
        let config = profile_with(
            r#"{"autonat":{"client":{"retry_interval":"45s"}},
                "relay":{"client":{"retry_max":"10m"}},
                "dcutr":{"retry_cooldown":"90s"}}"#,
        )
        .expect("parses");
        let text = serde_json::to_string(&config).expect("serializes");
        assert!(
            text.contains(r#""retry_interval":"45s""#),
            "a duration must be written back in its own unit: {text}"
        );
        let again = serde_json::from_str::<ProfileConfig>(&text).expect("re-parses");
        assert_eq!(
            config.transport.connectivity, again.transport.connectivity,
            "a block that survives a round trip means the same thing"
        );
    }

    #[test]
    fn more_static_candidates_than_the_ceiling_are_refused_while_reading() {
        // The ARRAY is bounded, not the set it becomes: judged as it
        // arrives, like `CandidatePeer::addresses` one crate over, so an
        // input of any size is not parsed and allocated first.
        let many: Vec<String> = (0..=MAX_STATIC_CANDIDATES)
            .map(|i| format!(r#""/ip4/203.0.113.{i}/tcp/4001/p2p/{P1}""#))
            .collect();
        let body = format!(
            r#"{{"relay":{{"client":{{"static_relays":[{}]}}}}}}"#,
            many.join(",")
        );
        let err = profile_with(&body).expect_err("an oversized list must be refused");
        assert!(
            err.to_string().contains("at most 16 static candidates"),
            "the refusal must name the ceiling: {err}"
        );
    }
}
