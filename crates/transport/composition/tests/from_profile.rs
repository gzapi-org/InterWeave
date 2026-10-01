// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! What composition takes from the whole profile (plan §16 (13)): the
//! limits and pre-authentication bounds the substrate honours reach it,
//! every modelled value it cannot yet honour is refused by name when it
//! differs from the schema's default, and `CompositionOptions::from_profile`
//! carries the listen addresses, the peer cache and the client queue bound.

#![allow(clippy::expect_used, clippy::panic)]

use interweave_profile_config::{ProfileConfig, ProfilePaths, XdgRoots};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_composition::{CompositionError, CompositionOptions, translate};
use interweave_transport_runtime::preauth::PreAuthLimitsBuilder;

fn profile(extra: &str) -> ProfileConfig {
    ProfileConfig::parse_yaml(&format!(
        "schema_version: 2
trust:
  policy: static-allowlist
  allowed_peers: []
endpoints:
  entries:
    - id: human
      enabled: true
      advertise: false
{extra}"
    ))
    .expect("parses")
}

fn local() -> interweave_transport_api::TransportIdentity {
    ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id")
}

#[test]
fn the_honoured_limits_and_pre_auth_bounds_reach_the_substrate() {
    let composed = translate(
        &profile(
            "transport:
  limits: {max_payload_bytes: 2048, max_connections_total: 100, max_connected_peers: 255, max_connections_per_peer: 2}
  pre_auth: {handshake_timeout: 7s, max_pending_inbound_handshakes: 12, max_pending_per_source_bucket: 3, max_attempts_per_source_bucket_per_minute: 9, max_attempts_global_per_minute: 90}
",
        ),
        &local(),
        256,
    )
    .expect("composes");
    let s = &composed.substrate;
    assert_eq!((s.max_payload_bytes, s.max_connections), (2_048, 100));
    assert_eq!(
        (s.max_connected_peers, s.max_connections_per_peer),
        (255, 2),
        "the two peer ceilings, each its own field"
    );
    let defaults = translate(&profile(""), &local(), 256).expect("composes");
    assert_eq!(
        (
            defaults.substrate.max_connected_peers,
            defaults.substrate.max_connections_per_peer
        ),
        (256, 3),
        "the schema's defaults"
    );
    assert_eq!(composed.capabilities.max_payload_bytes, 2_048);
    // The whole funnel bound, compared as one value: every field the
    // profile states, the schema's per-minute window, and the substrate's
    // default for the one the schema does not model (the source count).
    assert_eq!(
        s.preauth,
        PreAuthLimitsBuilder {
            max_pending_total: 12,
            max_pending_per_source: 3,
            handshake_timeout_ms: 7_000,
            rate_window_ms: 60_000,
            max_attempts_per_window: 9,
            max_global_attempts_per_window: 90,
            ..PreAuthLimitsBuilder::default()
        }
        .build()
        .expect("in bounds")
    );

    // The control: the schema's defaults, not the substrate's own.
    // The two limits the substrate takes (architect-cto's ruling on
    // #145): the address bound here; the subscription ceiling reaches the
    // broadcast state, pinned in the libp2p crate's `profile_limits.rs`.
    for limits in ["max_addresses_per_peer: 12", "max_subscriptions: 64"] {
        let composed = translate(
            &profile(&format!("transport:\n  limits: {{{limits}}}\n")),
            &local(),
            256,
        )
        .unwrap_or_else(|e| panic!("{limits} composes: {e}"));
        if limits.starts_with("max_addresses") {
            assert_eq!(composed.substrate.max_addresses_per_peer, 12);
        }
    }
    let defaults = translate(&profile(""), &local(), 256).expect("composes");
    assert_eq!(
        defaults.substrate.max_addresses_per_peer, 16,
        "the schema's default"
    );
    assert_eq!(
        (
            defaults.substrate.max_payload_bytes,
            defaults.substrate.max_connections
        ),
        (49_152, 384),
        "the connection total the schema states (architect-cto's ruling on #145)"
    );
}

/// Every value the runtime cannot honour yet is refused by name once it
/// leaves the schema's default, and accepted at it.
#[test]
fn an_unhonoured_value_is_refused_by_name_and_the_default_is_not() {
    translate(&profile(""), &local(), 256).expect("the defaults compose");
    for (block, field) in [
        (
            "limits: {max_candidates: 4095}",
            "transport.limits.max_candidates",
        ),
        (
            "connection_policy: {address_backoff_min: 6s}",
            "transport.connection_policy.address_backoff_min",
        ),
        (
            "connection_policy: {address_backoff_max: 6m}",
            "transport.connection_policy.address_backoff_max",
        ),
        (
            "connection_policy: {identity_mismatch_quarantine: 31m}",
            "transport.connection_policy.identity_mismatch_quarantine",
        ),
        ("direct: {timeout_ms: 9000}", "transport.direct.timeout_ms"),
        (
            "direct: {max_inflight_total: 127}",
            "transport.direct.max_inflight_total",
        ),
        (
            "direct: {max_inflight_per_peer: 7}",
            "transport.direct.max_inflight_per_peer",
        ),
        (
            "direct: {inbound_rate_limit: {per_peer_per_minute: 119}}",
            "transport.direct.inbound_rate_limit.per_peer_per_minute",
        ),
        (
            "direct: {inbound_rate_limit: {per_peer_burst: 31}}",
            "transport.direct.inbound_rate_limit.per_peer_burst",
        ),
        (
            "direct: {inbound_rate_limit: {global_per_minute: 1199}}",
            "transport.direct.inbound_rate_limit.global_per_minute",
        ),
        (
            "direct: {inbound_rate_limit: {global_burst: 255}}",
            "transport.direct.inbound_rate_limit.global_burst",
        ),
        (
            "pubsub: {inbound_rate_limit: {per_peer_per_minute: 119}}",
            "transport.pubsub.inbound_rate_limit.per_peer_per_minute",
        ),
        (
            "pubsub: {inbound_rate_limit: {per_peer_burst: 31}}",
            "transport.pubsub.inbound_rate_limit.per_peer_burst",
        ),
        (
            "pubsub: {inbound_rate_limit: {global_per_minute: 1199}}",
            "transport.pubsub.inbound_rate_limit.global_per_minute",
        ),
        (
            "pubsub: {inbound_rate_limit: {global_burst: 255}}",
            "transport.pubsub.inbound_rate_limit.global_burst",
        ),
    ] {
        match translate(&profile(&format!("transport:\n  {block}\n")), &local(), 256) {
            Err(CompositionError::Unhonoured { field: got }) => assert_eq!(got, field, "{block}"),
            Err(other) => panic!("{block}: expected Unhonoured {{ {field} }}, got {other}"),
            Ok(_) => panic!("{block}: expected Unhonoured {{ {field} }}, it composed"),
        }
    }
}

#[test]
fn the_options_a_profile_states() {
    let dir = tempfile::tempdir().expect("tempdir");
    let roots = XdgRoots {
        config_home: dir.path().join("config"),
        data_home: dir.path().join("data"),
        state_home: dir.path().join("state"),
        cache_home: dir.path().join("cache"),
        runtime_dir: None,
    };
    let paths = ProfilePaths::resolve_offline("work", &roots).expect("paths");
    let options = CompositionOptions::from_profile(
        &profile(
            "transport:\n  listen: {addresses: [\"/ip4/0.0.0.0/tcp/4001\"]}\nipc:\n  client_event_queue: 64\n",
        ),
        &paths,
    );
    assert_eq!(options.listen, ["/ip4/0.0.0.0/tcp/4001"]);
    assert_eq!(options.peer_cache_file, Some(paths.peer_cache_file()));
    assert_eq!(options.queue_bound, 64);
    assert_eq!(
        (options.event_capacity, options.discovery_interval),
        (
            CompositionOptions::default().event_capacity,
            CompositionOptions::default().discovery_interval
        )
    );
}
