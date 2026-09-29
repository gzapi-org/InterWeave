// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! Two profile limits the running substrate takes (#145, architect-cto's
//! ruling that a default an operator reads is the one the runtime runs):
//! `transport.limits.max_subscriptions` through `configure_broadcast`, and
//! `transport.limits.max_addresses_per_peer` through `SubstrateConfig` --
//! each beside the default it replaces.

#![allow(clippy::expect_used, clippy::panic)]

use interweave_profile_config::ProfileConfig;
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{ChannelId, TransportIdentity};
use interweave_transport_libp2p::{BroadcastChannels, SubstrateConfig, SwarmRuntime};
use interweave_transport_runtime::TrustSources;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};

fn peer() -> TransportIdentity {
    ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id")
}

fn trusting(peer: &TransportIdentity) -> TrustSources {
    TrustSources::new(
        PeerTrustPolicy::new([peer.clone()]).expect("one peer"),
        InfrastructureSet::default(),
    )
}

fn profile(limits: &str) -> ProfileConfig {
    ProfileConfig::parse_yaml(&format!(
        "schema_version: 2
trust:
  policy: static-allowlist
  allowed_peers: []
endpoints:
  entries: []
transport:
  limits: {{{limits}}}
"
    ))
    .expect("parses")
}

/// Joins stop at the profile's ceiling; the default ceiling takes them.
#[tokio::test]
async fn the_profiles_subscription_ceiling_binds_joins() {
    let channels = |n: u32| -> Vec<ChannelId> {
        (0..n)
            .map(|i| ChannelId::parse(format!("c{i}")).expect("legal"))
            .collect()
    };
    for (limits, accepted) in [("max_subscriptions: 2", 2), ("", 3)] {
        let other = peer();
        let runtime = SwarmRuntime::start(
            &ProfileIdentity::generate(),
            SubstrateConfig::default(),
            trusting(&other),
        )
        .expect("starts");
        runtime
            .configure_broadcast(
                BroadcastChannels::from_profile(&profile(limits), 16).expect("configuration"),
            )
            .await
            .expect("installed");
        let mut joined = 0;
        for channel in channels(3) {
            if runtime.join(channel, "s").await.expect("answered").is_ok() {
                joined += 1;
            }
        }
        assert_eq!(joined, accepted, "limits {{{limits}}}");
        runtime.shutdown().await.expect("clean shutdown");
    }
}

/// A peer's remembered addresses stop at the configured bound; the
/// default bound is the connection manager's eight.
#[tokio::test]
async fn the_profiles_address_bound_binds_the_book() {
    for (configured, expected) in [(Some(16), 16), (None, 8)] {
        let other = peer();
        let config = SubstrateConfig {
            max_addresses_per_peer: configured
                .unwrap_or(SubstrateConfig::default().max_addresses_per_peer),
            ..SubstrateConfig::default()
        };
        let runtime = SwarmRuntime::start(&ProfileIdentity::generate(), config, trusting(&other))
            .expect("starts");
        let mut kept = 0;
        for i in 0..20 {
            let address = format!("/ip4/198.51.100.{i}/tcp/4001")
                .parse()
                .expect("multiaddr");
            if runtime
                .add_address(other.clone(), address)
                .await
                .expect("answered")
            {
                kept += 1;
            }
        }
        assert_eq!(kept, expected, "configured {configured:?}");
        runtime.shutdown().await.expect("clean shutdown");
    }
}
