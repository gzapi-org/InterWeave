// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! A `kademlia` entry resolved against `kademlia-integration.md` §13, and
//! the rules the schema gates on `enabled: true`: `network_id` present,
//! the three cross-field limits over the RESOLVED values, and every seed
//! source an enabled provider. Each refusal beside the document that
//! passes.

#![allow(clippy::expect_used)]

use interweave_kademlia_control_api::KademliaMode;
use interweave_profile_config::{ConfigError, DiscoveryProviderType, ProfileConfig};

fn profile(kademlia_enabled: bool, kademlia_config: &str, others: &str) -> ProfileConfig {
    let doc = format!(
        "schema_version: 2
trust:
  policy: static-allowlist
  allowed_peers: []
endpoints:
  entries: []
discovery:
  providers:
    - type: kademlia
      enabled: {kademlia_enabled}
      priority: 40
      config:
{kademlia_config}{others}"
    );
    serde_norway::from_str(&doc).expect("the document parses")
}

fn kademlia_errors(profile: &ProfileConfig) -> Vec<(&'static str, String)> {
    profile
        .validate()
        .into_iter()
        .filter_map(|e| match e {
            ConfigError::InvalidKademliaSetting { field, reason } => Some((field, reason)),
            _ => None,
        })
        .collect()
}

fn entry(profile: &ProfileConfig) -> &interweave_profile_config::DiscoveryProviderSettings {
    &profile
        .discovery
        .providers
        .iter()
        .find(|p| p.provider_type == DiscoveryProviderType::Kademlia)
        .expect("the kademlia entry")
        .config
}

#[test]
fn a_minimal_entry_resolves_to_the_section_13_defaults() {
    let p = profile(true, "        network_id: interweave-test\n", "");
    let k = entry(&p).kademlia_profile().expect("resolves");
    assert_eq!(k.network_id, "interweave-test");
    assert_eq!(k.mode, KademliaMode::Client);
    assert_eq!(
        (
            k.candidate_ttl_ms,
            k.kbucket_size,
            k.max_routing_peers,
            k.query_timeout_ms,
            k.parallelism,
            k.disjoint_query_paths,
            k.max_concurrent_queries,
            k.max_queries_per_minute,
        ),
        (1_800_000, 20, 256, 30_000, 3, true, 2, 6)
    );
    assert_eq!(
        (
            k.exploration_interval_ms,
            k.exploration_jitter_percent,
            k.max_results_per_query,
            k.target_routing_peers,
            k.targeted_lookup_cooldown_ms,
            k.bootstrap_min_interval_ms,
            k.bootstrap_refresh_interval_ms,
        ),
        (60_000, 20, 20, 64, 300_000, 300_000, 900_000)
    );
    assert!(kademlia_errors(&p).is_empty(), "{:?}", kademlia_errors(&p));
}

#[test]
fn written_values_replace_the_defaults() {
    let p = profile(
        true,
        "        network_id: interweave-test\n        mode: server\n        kbucket_size: 10\n        max_results_per_query: 10\n        query_timeout: 45s\n",
        "",
    );
    let k = entry(&p).kademlia_profile().expect("resolves");
    assert_eq!(
        (
            k.mode,
            k.kbucket_size,
            k.max_results_per_query,
            k.query_timeout_ms
        ),
        (KademliaMode::Server, 10, 10, 45_000)
    );
}

#[test]
fn an_enabled_entry_needs_a_network_id_and_a_disabled_one_does_not() {
    let enabled = profile(true, "        mode: client\n", "");
    assert_eq!(entry(&enabled).kademlia_profile(), None);
    assert!(
        kademlia_errors(&enabled)
            .iter()
            .any(|(field, _)| *field == "network_id"),
        "{:?}",
        kademlia_errors(&enabled)
    );
    let disabled = profile(false, "        mode: client\n", "");
    assert!(
        kademlia_errors(&disabled).is_empty(),
        "the control: a staged, disabled entry runs nothing"
    );
}

#[test]
fn a_default_breaks_a_cross_field_limit_as_surely_as_a_written_value() {
    // kbucket_size 8 against max_results_per_query's default of 20.
    let p = profile(
        true,
        "        network_id: interweave-test\n        kbucket_size: 8\n",
        "",
    );
    assert_eq!(
        kademlia_errors(&p)
            .into_iter()
            .map(|(field, _)| field)
            .collect::<Vec<_>>(),
        vec!["max_results_per_query"]
    );
    let fixed = profile(
        true,
        "        network_id: interweave-test\n        kbucket_size: 8\n        max_results_per_query: 8\n",
        "",
    );
    assert!(kademlia_errors(&fixed).is_empty(), "the control");
}

#[test]
fn the_other_two_limits_are_refused_over_resolved_values() {
    for (config, field) in [
        (
            "        network_id: interweave-test\n        max_routing_peers: 32\n",
            "target_routing_peers",
        ),
        (
            "        network_id: interweave-test\n        bootstrap_min_interval: 30m\n",
            "bootstrap_refresh_interval",
        ),
    ] {
        let fields: Vec<_> = kademlia_errors(&profile(true, config, ""))
            .into_iter()
            .map(|(f, _)| f)
            .collect();
        assert_eq!(fields, vec![field], "{config}");
        assert!(
            kademlia_errors(&profile(false, config, "")).is_empty(),
            "the control: disabled, {field} is not judged"
        );
    }
}

#[test]
fn every_seed_source_must_be_an_enabled_provider() {
    let seeded = "        network_id: interweave-test\n        seed_sources: [static-bootstrap]\n";
    let absent = profile(true, seeded, "");
    assert!(
        kademlia_errors(&absent)
            .iter()
            .any(|(field, _)| *field == "seed_sources")
    );
    let disabled = profile(
        true,
        seeded,
        "    - type: static-bootstrap\n      enabled: false\n      priority: 10\n      config:\n        peers: []\n",
    );
    assert!(
        kademlia_errors(&disabled)
            .iter()
            .any(|(field, _)| *field == "seed_sources"),
        "a configured but disabled source is refused too"
    );
    let enabled = profile(
        true,
        seeded,
        "    - type: static-bootstrap\n      enabled: true\n      priority: 10\n      config:\n        peers: []\n",
    );
    assert!(kademlia_errors(&enabled).is_empty(), "the control");
}
