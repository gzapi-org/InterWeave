// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! A `kademlia` provider entry, resolved: every field the schema gives a
//! default filled with it (`kademlia-integration.md` §13), and the
//! cross-field rules that hold once the entry is enabled.
//!
//! ONE RESOLUTION FOR BOTH HALVES. The provider (`crates/discovery/
//! kademlia`) and the Swarm-owned driver (`crates/transport/libp2p`) each
//! read part of this block, so composition translates both from one
//! [`KademliaProfile`] rather than each half applying its own defaults,
//! which is how two halves come to disagree about a value nobody wrote.

use std::collections::BTreeSet;

use interweave_kademlia_control_api::{KademliaMode, LimitViolation, validate_limits};

use crate::{ConfigError, DiscoveryProviderSettings, parse_duration_ms, parse_duration_ms_u64};

/// §13's defaults, in the schema's units.
mod defaults {
    pub(super) const CANDIDATE_TTL_MS: u64 = 30 * 60 * 1_000;
    pub(super) const KBUCKET_SIZE: u32 = 20;
    pub(super) const MAX_ROUTING_PEERS: u32 = 256;
    pub(super) const QUERY_TIMEOUT_MS: u32 = 30_000;
    pub(super) const PARALLELISM: u32 = 3;
    pub(super) const DISJOINT_QUERY_PATHS: bool = true;
    pub(super) const MAX_CONCURRENT_QUERIES: u32 = 2;
    pub(super) const MAX_QUERIES_PER_MINUTE: u32 = 6;
    pub(super) const EXPLORATION_INTERVAL_MS: u32 = 60_000;
    pub(super) const EXPLORATION_JITTER_PERCENT: u32 = 20;
    pub(super) const MAX_RESULTS_PER_QUERY: u32 = 20;
    pub(super) const TARGET_ROUTING_PEERS: u32 = 64;
    pub(super) const TARGETED_LOOKUP_COOLDOWN_MS: u32 = 5 * 60 * 1_000;
    pub(super) const BOOTSTRAP_MIN_INTERVAL_MS: u32 = 5 * 60 * 1_000;
    pub(super) const BOOTSTRAP_REFRESH_INTERVAL_MS: u32 = 15 * 60 * 1_000;
}

/// A `kademlia` entry with every default applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KademliaProfile {
    /// The deployment namespace (required once enabled).
    pub network_id: String,
    /// Client or server, `client` by default.
    pub mode: KademliaMode,
    /// Providers whose candidates seed the routing table.
    pub seed_sources: Vec<String>,
    /// `candidate_ttl`.
    pub candidate_ttl_ms: u64,
    /// `kbucket_size`.
    pub kbucket_size: u32,
    /// `max_routing_peers`.
    pub max_routing_peers: u32,
    /// `query_timeout`.
    pub query_timeout_ms: u32,
    /// `parallelism`.
    pub parallelism: u32,
    /// `disjoint_query_paths`.
    pub disjoint_query_paths: bool,
    /// `max_concurrent_queries`.
    pub max_concurrent_queries: u32,
    /// `max_queries_per_minute`.
    pub max_queries_per_minute: u32,
    /// `exploration_interval`.
    pub exploration_interval_ms: u32,
    /// `exploration_jitter_percent`.
    pub exploration_jitter_percent: u32,
    /// `max_results_per_query`.
    pub max_results_per_query: u32,
    /// `target_routing_peers`.
    pub target_routing_peers: u32,
    /// `targeted_lookup_cooldown`.
    pub targeted_lookup_cooldown_ms: u32,
    /// `bootstrap_min_interval`.
    pub bootstrap_min_interval_ms: u32,
    /// `bootstrap_refresh_interval`.
    pub bootstrap_refresh_interval_ms: u32,
}

impl DiscoveryProviderSettings {
    /// This entry resolved against §13's defaults, or `None` when a value
    /// is malformed or `network_id` is absent -- the reasons
    /// [`crate::ProfileConfig::validate`] reports. Composition reads a
    /// validated profile, so `None` there is a profile it refuses.
    #[must_use]
    pub fn kademlia_profile(&self) -> Option<KademliaProfile> {
        let ms32 = |text: Option<&String>, default: u32| {
            text.map_or(Some(default), |t| parse_duration_ms(t).ok())
        };
        Some(KademliaProfile {
            network_id: self.network_id.clone()?,
            mode: match self.mode.as_deref() {
                None | Some("client") => KademliaMode::Client,
                Some("server") => KademliaMode::Server,
                Some(_) => return None,
            },
            seed_sources: self.seed_sources.clone(),
            candidate_ttl_ms: self
                .candidate_ttl
                .as_ref()
                .map_or(Some(defaults::CANDIDATE_TTL_MS), |t| {
                    parse_duration_ms_u64(t).ok()
                })?,
            kbucket_size: self.kbucket_size.unwrap_or(defaults::KBUCKET_SIZE),
            max_routing_peers: self
                .max_routing_peers
                .unwrap_or(defaults::MAX_ROUTING_PEERS),
            query_timeout_ms: ms32(self.query_timeout.as_ref(), defaults::QUERY_TIMEOUT_MS)?,
            parallelism: self.parallelism.unwrap_or(defaults::PARALLELISM),
            disjoint_query_paths: self
                .disjoint_query_paths
                .unwrap_or(defaults::DISJOINT_QUERY_PATHS),
            max_concurrent_queries: self
                .max_concurrent_queries
                .unwrap_or(defaults::MAX_CONCURRENT_QUERIES),
            max_queries_per_minute: self
                .max_queries_per_minute
                .unwrap_or(defaults::MAX_QUERIES_PER_MINUTE),
            exploration_interval_ms: ms32(
                self.exploration_interval.as_ref(),
                defaults::EXPLORATION_INTERVAL_MS,
            )?,
            exploration_jitter_percent: self
                .exploration_jitter_percent
                .unwrap_or(defaults::EXPLORATION_JITTER_PERCENT),
            max_results_per_query: self
                .max_results_per_query
                .unwrap_or(defaults::MAX_RESULTS_PER_QUERY),
            target_routing_peers: self
                .target_routing_peers
                .unwrap_or(defaults::TARGET_ROUTING_PEERS),
            targeted_lookup_cooldown_ms: ms32(
                self.targeted_lookup_cooldown.as_ref(),
                defaults::TARGETED_LOOKUP_COOLDOWN_MS,
            )?,
            bootstrap_min_interval_ms: ms32(
                self.bootstrap_min_interval.as_ref(),
                defaults::BOOTSTRAP_MIN_INTERVAL_MS,
            )?,
            bootstrap_refresh_interval_ms: ms32(
                self.bootstrap_refresh_interval.as_ref(),
                defaults::BOOTSTRAP_REFRESH_INTERVAL_MS,
            )?,
        })
    }
}

/// The rules the schema gates on `enabled: true`: `network_id` present,
/// §13's three cross-field limits over the RESOLVED values (a default can
/// break one as surely as a written value -- `kbucket_size: 8` alone
/// breaks `max_results_per_query <= kbucket_size` against its default of
/// 20), and every seed source an enabled provider of this profile.
///
/// Per-field errors are [`DiscoveryProviderSettings::kademlia_value_errors`]'s;
/// this adds nothing when the entry does not resolve, so one malformed
/// value is reported once.
pub(crate) fn enabled_entry_errors(
    settings: &DiscoveryProviderSettings,
    enabled_providers: &BTreeSet<&str>,
) -> Vec<ConfigError> {
    let mut errors = Vec::new();
    if settings.network_id.is_none() {
        errors.push(ConfigError::InvalidKademliaSetting {
            field: "network_id",
            reason: "is required when the kademlia provider is enabled".to_owned(),
        });
    }
    for source in &settings.seed_sources {
        if !enabled_providers.contains(source.as_str()) {
            errors.push(ConfigError::InvalidKademliaSetting {
                field: "seed_sources",
                reason: format!("'{source}' is not an enabled provider in this profile"),
            });
        }
    }
    if let Some(k) = settings.kademlia_profile()
        && let Err(violation) = validate_limits(
            k.target_routing_peers,
            k.max_routing_peers,
            u64::from(k.bootstrap_min_interval_ms),
            u64::from(k.bootstrap_refresh_interval_ms),
            k.max_results_per_query,
            k.kbucket_size,
        )
    {
        let (field, reason) = match violation {
            LimitViolation::TargetAboveMax => (
                "target_routing_peers",
                format!(
                    "{} exceeds max_routing_peers {}",
                    k.target_routing_peers, k.max_routing_peers
                ),
            ),
            LimitViolation::RefreshBelowMinimum => (
                "bootstrap_refresh_interval",
                format!(
                    "{}ms is below bootstrap_min_interval {}ms",
                    k.bootstrap_refresh_interval_ms, k.bootstrap_min_interval_ms
                ),
            ),
            LimitViolation::ResultsAboveBucket => (
                "max_results_per_query",
                format!(
                    "{} exceeds kbucket_size {}",
                    k.max_results_per_query, k.kbucket_size
                ),
            ),
        };
        errors.push(ConfigError::InvalidKademliaSetting { field, reason });
    }
    errors
}
