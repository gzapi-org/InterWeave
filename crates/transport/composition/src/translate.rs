// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! A validated profile, translated: the substrate's configuration with
//! each behaviour switched on by its block, the trust sources, the direct
//! and broadcast installations, and the discovery providers to compose.
//!
//! THIS IS WHERE A PROFILE'S BLOCK BECOMES A `Some`. Every connectivity
//! behaviour and every discovery provider is `None` in
//! `SubstrateConfig::default()` (the owner's 2026-09-07 ruling), and this
//! module is the one production site that switches one on -- from the
//! block the profile validated, through the translator the libp2p crate
//! already tests against it.

use interweave_discovery_cache::{CacheLimits, CacheLimitsBuilder};
use interweave_discovery_static::StaticEntry;
use interweave_profile_config::kademlia::KademliaProfile;
use interweave_profile_config::{
    ConfigError, DiscoveryProviderType, ProfileConfig, split_peer_multiaddr,
};
use interweave_transport_api::{ChannelId, EndpointId, TransportCapabilities, TransportIdentity};
use interweave_transport_libp2p::runtime::DirectEndpoints;
use interweave_transport_libp2p::runtime::autonat_driver::AutonatClientSettings;
use interweave_transport_libp2p::runtime::autonat_server_driver::AutonatServerSettings;
use interweave_transport_libp2p::runtime::dcutr_driver::DcutrSettings;
use interweave_transport_libp2p::runtime::kademlia_driver::KademliaSettings;
use interweave_transport_libp2p::runtime::mdns_driver::MdnsSettings;
use interweave_transport_libp2p::runtime::relay_driver::RelayClientSettings;
use interweave_transport_libp2p::runtime::relay_server_driver::RelayServerSettings;
use interweave_transport_libp2p::{BroadcastChannels, SubstrateConfig};
use interweave_transport_runtime::TrustSources;
use interweave_transport_runtime::preauth::PreAuthLimitsBuilder;
use interweave_trust_api::PeerTrustPolicy;

/// Why a profile did not compose.
#[derive(Debug)]
pub enum CompositionError {
    /// The profile fails its own validation; nothing is built from it.
    InvalidProfile(Vec<ConfigError>),
    /// A block validated and its translation refused it -- a defect in
    /// one of the two, named by the translator.
    Translation(&'static str),
    /// The substrate refused to start or to install a block.
    Substrate(interweave_transport_libp2p::SubstrateError),
    /// A discovery provider could not be built or started.
    Discovery(String),
    /// The profile sets a value this runtime cannot yet honour, to
    /// something other than the schema's default: refused, naming the
    /// field, rather than silently run at the default (plan §16 (13),
    /// §15 (5)'s shape).
    Unhonoured {
        /// Dotted path of the field.
        field: &'static str,
    },
}

impl core::fmt::Display for CompositionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidProfile(errors) => {
                write!(f, "the profile is invalid: ")?;
                for (i, e) in errors.iter().enumerate() {
                    if i > 0 {
                        write!(f, "; ")?;
                    }
                    write!(f, "{e}")?;
                }
                Ok(())
            }
            Self::Translation(why) => write!(f, "translation: {why}"),
            Self::Substrate(e) => write!(f, "substrate: {e}"),
            Self::Discovery(why) => write!(f, "discovery: {why}"),
            Self::Unhonoured { field } => write!(
                f,
                "{field} is set to a value this build cannot honour yet; only the schema's default is accepted"
            ),
        }
    }
}

impl std::error::Error for CompositionError {}

/// The Kademlia protocol's wire major (`/interweave/kad/1.0.0/...`,
/// `kademlia-integration.md` §4): the only one this build speaks.
pub const KADEMLIA_WIRE_MAJOR: u32 = 1;

/// The providers a profile enables, each with its configured priority.
#[derive(Debug, Default)]
pub struct DiscoveryPlan {
    /// `static-bootstrap`: its entries.
    pub static_bootstrap: Option<(Vec<StaticEntry>, i32)>,
    /// `peer-cache`: its limits.
    pub peer_cache: Option<(CacheLimits, i32)>,
    /// `mdns`.
    pub mdns: Option<i32>,
    /// `kademlia`: the resolved entry.
    pub kademlia: Option<(KademliaProfile, i32)>,
}

/// Everything [`crate::ComposedRuntime`] builds from one profile.
pub struct Composition {
    /// The substrate's configuration, behaviours switched on.
    pub substrate: SubstrateConfig,
    /// The data-plane allowlist with this profile's own identity bound,
    /// and the infrastructure set.
    pub trust: TrustSources,
    /// The data-plane allowlist alone, for discovery's trust filter and
    /// Kademlia's remote-trusted population.
    pub peer_trust: PeerTrustPolicy,
    /// The configured endpoints.
    pub direct: DirectEndpoints,
    /// The desired channels.
    pub broadcast: BroadcastChannels,
    /// The providers to compose.
    pub discovery: DiscoveryPlan,
    /// What this backend and profile support.
    pub capabilities: TransportCapabilities,
}

/// Translate a profile, refusing one that does not validate.
///
/// # Errors
/// [`CompositionError::InvalidProfile`] with every violation, or the
/// translator that refused a block.
pub fn translate(
    profile: &ProfileConfig,
    local: &TransportIdentity,
    queue_bound: usize,
) -> Result<Composition, CompositionError> {
    let errors = profile.validate();
    if !errors.is_empty() {
        return Err(CompositionError::InvalidProfile(errors));
    }
    refuse_unhonoured(profile)?;
    let connectivity = &profile.transport.connectivity;
    let limits = &profile.transport.limits;
    let pre_auth = &profile.transport.pre_auth;
    let discovery = discovery_plan(profile)?;

    let mut substrate = SubstrateConfig {
        // The limits and pre-authentication bounds the substrate takes,
        // from the profile rather than the substrate's own defaults.
        max_payload_bytes: usize_of(limits.max_payload_bytes),
        // THE PEER CEILING BINDS THROUGH THE CONNECTION CEILING: the
        // substrate counts connections and nothing counts peers, and a peer
        // holds at least one connection, so capping connections at the
        // smaller of the two keeps `max_connected_peers` true. Taking
        // `max_connections_total` alone raised a default profile from 256
        // peers to 384 while its accepted peer ceiling bound nothing (#145
        // review F3).
        max_connections: usize_of(limits.max_connections_total.min(limits.max_connected_peers)),
        max_addresses_per_peer: usize_of(limits.max_addresses_per_peer),
        preauth: PreAuthLimitsBuilder {
            max_pending_total: usize_of(pre_auth.max_pending_inbound_handshakes),
            max_pending_per_source: usize_of(pre_auth.max_pending_per_source_bucket),
            handshake_timeout_ms: u64::from(pre_auth.handshake_timeout_ms),
            // The schema states its attempt budgets per minute.
            rate_window_ms: 60_000,
            max_attempts_per_window: pre_auth.max_attempts_per_source_bucket_per_minute,
            max_global_attempts_per_window: pre_auth.max_attempts_global_per_minute,
            ..PreAuthLimitsBuilder::default()
        }
        .build()
        .map_err(|_| CompositionError::Translation("transport.pre_auth"))?,
        // The client roles are `literal[true]` in the schema: every valid
        // profile runs them.
        autonat_client: Some(
            AutonatClientSettings::from_profile(&connectivity.autonat.client)
                .map_err(CompositionError::Translation)?,
        ),
        relay_client: Some(
            RelayClientSettings::from_profile(&connectivity.relay.client)
                .map_err(CompositionError::Translation)?,
        ),
        dcutr: Some(DcutrSettings::from_profile(&connectivity.dcutr)),
        ..SubstrateConfig::default()
    };
    if connectivity.autonat.server.enabled {
        substrate.autonat_server = Some(AutonatServerSettings::from_profile(
            &connectivity.autonat.server,
        ));
    }
    if connectivity.relay.server.enabled {
        substrate.relay_server = Some(
            RelayServerSettings::from_profile(&connectivity.relay.server)
                .map_err(CompositionError::Translation)?,
        );
    }
    if let Some((kademlia, _)) = &discovery.kademlia {
        substrate.kademlia =
            Some(KademliaSettings::from_profile(kademlia).map_err(CompositionError::Translation)?);
    }
    // `config: {}` is the v1 choice for mDNS (plan §15's decisions,
    // 2026-09-27): ADR-0053's defaults, no profile field.
    if discovery.mdns.is_some() {
        substrate.mdns = Some(MdnsSettings::default());
    }
    // THE OPERATOR'S DOOR AT START (ADR-0052 rule 9): the static bootstrap
    // entries the operator configured. The static AutoNAT servers and
    // relays are seeded by the substrate from their own settings.
    if let Some((entries, _)) = &discovery.static_bootstrap {
        substrate.operator_addresses = entries
            .iter()
            .map(|e| format!("{}/p2p/{}", e.address, e.peer_id.as_str()))
            .collect();
    }
    substrate.validate().map_err(CompositionError::Substrate)?;

    let peer_trust = PeerTrustPolicy::new(profile.trust.allowed_peers.iter().cloned())
        .map_err(|_| CompositionError::Translation("trust.allowed_peers past its bound"))?;
    let trust = TrustSources::new(
        peer_trust.clone().with_local_peer(local.clone()),
        connectivity.infrastructure.clone(),
    );
    let direct =
        DirectEndpoints::from_profile(profile, queue_bound).map_err(CompositionError::Substrate)?;
    let broadcast = BroadcastChannels::from_profile(profile, queue_bound)
        .map_err(CompositionError::Substrate)?;
    let capabilities = TransportCapabilities {
        broadcast: true,
        direct_delivery: true,
        direct_endpoint_addressing: true,
        endpoint_directory: profile.endpoints.directory.enabled,
        internet_reachability: true,
        relayed_connectivity: true,
        direct_path_upgrade: true,
        durable_delivery: false,
        offline_mailbox: false,
        max_payload_bytes: substrate.max_payload_bytes,
        max_channel_id_bytes: ChannelId::MAX_BYTES,
        max_endpoint_id_bytes: EndpointId::MAX_BYTES,
    }
    .clamped();
    Ok(Composition {
        substrate,
        trust,
        peer_trust,
        direct,
        broadcast,
        discovery,
        capabilities,
    })
}

/// A profile `u32` as a count; lossless on every platform this builds for.
fn usize_of(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// Refuse every modelled value the runtime does not take from the
/// profile, when it differs from the schema's default: the field is named
/// rather than the value dropped in silence.
///
/// ACCEPTING THE DEFAULT MEANS THE RUNTIME RUNS IT: for every row left
/// here the runtime's own constant is the schema's default, pinned by
/// `the_accepted_defaults_are_what_the_runtime_runs` (architect-cto's
/// ruling on #145, 2026-09-29: the default an operator reads is the one
/// run, or the field is refused). `max_connected_peers` binds through the
/// connection ceiling above; `max_subscriptions` and
/// `max_addresses_per_peer` are taken by the substrate. Two rows are
/// refused off their default until a later batch wires them:
/// `max_connections_per_peer` (nothing counts connections per peer) and
/// the address backoff, whose constants AutoNAT's re-test schedule shares
/// by `AUTONAT.md` §4's amendment, so making them configurable is a
/// contract question before it is wiring.
fn refuse_unhonoured(profile: &ProfileConfig) -> Result<(), CompositionError> {
    use interweave_profile_config::transport::{
        ConnectionPolicyConfig, DirectConfig, InboundRateLimitConfig, LimitsConfig,
    };
    let t = &profile.transport;
    let (limits, policy, direct) = (
        LimitsConfig::default(),
        ConnectionPolicyConfig::default(),
        DirectConfig::default(),
    );
    let rate = InboundRateLimitConfig::default();
    let rate_rows = |prefix: [&'static str; 4], r: &InboundRateLimitConfig| {
        [
            (prefix[0], r.per_peer_per_minute != rate.per_peer_per_minute),
            (prefix[1], r.per_peer_burst != rate.per_peer_burst),
            (prefix[2], r.global_per_minute != rate.global_per_minute),
            (prefix[3], r.global_burst != rate.global_burst),
        ]
    };
    let mut rows = vec![
        (
            "transport.limits.max_connected_peers",
            t.limits.max_connected_peers != limits.max_connected_peers,
        ),
        (
            "transport.limits.max_connections_per_peer",
            t.limits.max_connections_per_peer != limits.max_connections_per_peer,
        ),
        (
            "transport.limits.max_candidates",
            t.limits.max_candidates != limits.max_candidates,
        ),
        (
            "transport.connection_policy.address_backoff_min",
            t.connection_policy.address_backoff_min_ms != policy.address_backoff_min_ms,
        ),
        (
            "transport.connection_policy.address_backoff_max",
            t.connection_policy.address_backoff_max_ms != policy.address_backoff_max_ms,
        ),
        (
            "transport.connection_policy.identity_mismatch_quarantine",
            t.connection_policy.identity_mismatch_quarantine_ms
                != policy.identity_mismatch_quarantine_ms,
        ),
        (
            "transport.direct.timeout_ms",
            t.direct.timeout_ms != direct.timeout_ms,
        ),
        (
            "transport.direct.max_inflight_total",
            t.direct.max_inflight_total != direct.max_inflight_total,
        ),
        (
            "transport.direct.max_inflight_per_peer",
            t.direct.max_inflight_per_peer != direct.max_inflight_per_peer,
        ),
    ];
    rows.extend(rate_rows(
        [
            "transport.direct.inbound_rate_limit.per_peer_per_minute",
            "transport.direct.inbound_rate_limit.per_peer_burst",
            "transport.direct.inbound_rate_limit.global_per_minute",
            "transport.direct.inbound_rate_limit.global_burst",
        ],
        &t.direct.inbound_rate_limit,
    ));
    rows.extend(rate_rows(
        [
            "transport.pubsub.inbound_rate_limit.per_peer_per_minute",
            "transport.pubsub.inbound_rate_limit.per_peer_burst",
            "transport.pubsub.inbound_rate_limit.global_per_minute",
            "transport.pubsub.inbound_rate_limit.global_burst",
        ],
        &t.pubsub.inbound_rate_limit,
    ));
    match rows.into_iter().find(|(_, differs)| *differs) {
        Some((field, _)) => Err(CompositionError::Unhonoured { field }),
        None => Ok(()),
    }
}

fn discovery_plan(profile: &ProfileConfig) -> Result<DiscoveryPlan, CompositionError> {
    let mut plan = DiscoveryPlan::default();
    for entry in profile.discovery.providers.iter().filter(|p| p.enabled) {
        let priority = entry.priority;
        match entry.provider_type {
            DiscoveryProviderType::StaticBootstrap => {
                let mut entries = Vec::with_capacity(entry.config.peers.len());
                for peer in &entry.config.peers {
                    let (address, id) =
                        split_peer_multiaddr(peer).map_err(CompositionError::Translation)?;
                    entries.push(StaticEntry::new(id, address).map_err(|e| {
                        CompositionError::Discovery(format!("static entry {peer}: {e:?}"))
                    })?);
                }
                plan.static_bootstrap = Some((entries, priority));
            }
            DiscoveryProviderType::PeerCache => {
                let mut limits = CacheLimitsBuilder::default();
                if let Some(ttl) = entry.config.peer_cache_ttl_ms() {
                    limits.ttl_ms = ttl;
                }
                if let Some(max) = entry.config.max_entries {
                    limits.max_peers = usize::try_from(max)
                        .map_err(|_| CompositionError::Translation("peer-cache max_entries"))?;
                }
                let limits = limits
                    .build()
                    .map_err(|e| CompositionError::Discovery(format!("peer-cache: {e:?}")))?;
                plan.peer_cache = Some((limits, priority));
            }
            DiscoveryProviderType::Mdns => plan.mdns = Some(priority),
            DiscoveryProviderType::Kademlia => {
                let resolved =
                    entry
                        .config
                        .kademlia_profile()
                        .ok_or(CompositionError::Translation(
                            "a validated kademlia entry did not resolve",
                        ))?;
                plan.kademlia = Some((resolved, priority));
            }
        }
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use interweave_profile_config::transport::{
        ConnectionPolicyConfig, DirectConfig, InboundRateLimitConfig, LimitsConfig,
    };
    use interweave_transport_runtime::{connection_manager, connection_policy, discovery, ingress};

    /// Each default `refuse_unhonoured` accepts for a value the runtime
    /// runs by its own constant is that constant: a retuned constant, or
    /// a retuned schema default, fails here rather than drifting apart.
    #[test]
    fn the_accepted_defaults_are_what_the_runtime_runs() {
        let (limits, policy, direct, rate) = (
            LimitsConfig::default(),
            ConnectionPolicyConfig::default(),
            DirectConfig::default(),
            InboundRateLimitConfig::default(),
        );
        assert_eq!(
            usize::try_from(limits.max_candidates).ok(),
            Some(discovery::MAX_CANDIDATES)
        );
        assert_eq!(
            u64::from(policy.identity_mismatch_quarantine_ms),
            connection_policy::IDENTITY_MISMATCH_QUARANTINE_MS
        );
        assert_eq!(
            u64::from(policy.address_backoff_min_ms),
            connection_manager::RETRY_BASE_MS
        );
        assert_eq!(
            u64::from(policy.address_backoff_max_ms),
            connection_manager::RETRY_CEILING_MS
        );
        assert_eq!(
            u128::from(direct.timeout_ms),
            interweave_transport_libp2p::behaviour::DIRECT_TIMEOUT.as_millis()
        );
        assert_eq!(
            (
                rate.per_peer_per_minute,
                rate.per_peer_burst,
                rate.global_per_minute,
                rate.global_burst
            ),
            (
                ingress::DEFAULT_PER_PEER_PER_MINUTE,
                ingress::DEFAULT_PER_PEER_BURST,
                ingress::DEFAULT_GLOBAL_PER_MINUTE,
                ingress::DEFAULT_GLOBAL_BURST
            )
        );
    }
}
