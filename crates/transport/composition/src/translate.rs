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
    let connectivity = &profile.transport.connectivity;
    let discovery = discovery_plan(profile)?;

    let mut substrate = SubstrateConfig {
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
