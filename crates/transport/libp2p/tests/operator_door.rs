// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! ADR-0052 rule 9 (A 2026-09-25): the operator's door, on the real
//! runtime.
//!
//! Two writes open that door -- the profile's own configuration at
//! start, and `SwarmRuntime::add_address` afterwards -- and every place
//! that applies the peer-supplied boundary reads what they wrote. Those
//! readers are tested where they live (the root funnel, the Kademlia
//! stash, the Identify and mDNS learn sites). What only a running
//! runtime can show is that the WRITES happen: delete either one and
//! the operator's `/dns4` seed would be refused at the doors that
//! consult the set, with every reader's own test still green.

#![allow(clippy::expect_used)]

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_libp2p::runtime::autonat_driver::{AutonatClientSettings, StaticServer};
use interweave_transport_libp2p::{SubstrateConfig, SwarmRuntime};
use interweave_transport_runtime::TrustSources;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::Multiaddr;

fn trusting(peer: &interweave_transport_api::TransportIdentity) -> TrustSources {
    TrustSources::new(
        PeerTrustPolicy::new([peer.clone()]).expect("one peer"),
        InfrastructureSet::default(),
    )
}

/// `add_address` is the operator's command, and what it records is an
/// operator address -- judged on the route, so the suffix the operator
/// wrote does not change the answer.
#[tokio::test]
async fn an_address_the_operator_adds_is_an_operator_address() {
    let subject = ProfileIdentity::generate();
    let peer = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id");
    let runtime =
        SwarmRuntime::start(&subject, SubstrateConfig::default(), trusting(&peer)).expect("starts");

    let bare: Multiaddr = "/dns4/boot.example/tcp/4001".parse().expect("valid");
    let suffixed: Multiaddr = format!("{bare}/p2p/{}", peer.as_str())
        .parse()
        .expect("valid");

    // THE CONTROL: nothing has come in by the operator's door yet.
    assert!(
        !runtime.is_operator_address(&bare),
        "a fresh runtime's operator set holds nothing it was not given"
    );

    runtime
        .add_address(peer, suffixed)
        .await
        .expect("the command reaches the task");

    assert!(
        runtime.is_operator_address(&bare),
        "the address the operator added, WITH its suffix, must be found BARE: one route \
         is one key at both ends of the pair"
    );
    assert!(
        !runtime.is_operator_address(
            &"/dns4/a-peers-choice.example/tcp/4001"
                .parse()
                .expect("valid")
        ),
        "and an address the operator never gave is still a peer's"
    );

    runtime.shutdown().await.expect("clean shutdown");
}

/// The profile's configuration is the other half of the door: a static
/// AutoNAT server the operator configured is an operator address from
/// the moment the runtime starts.
#[tokio::test]
async fn a_configured_static_server_is_an_operator_address_from_the_start() {
    let subject = ProfileIdentity::generate();
    let server = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id");
    let configured: Multiaddr = format!("/ip4/192.168.1.20/tcp/4001/p2p/{}", server.as_str())
        .parse()
        .expect("valid");
    let config = SubstrateConfig {
        autonat_client: Some(AutonatClientSettings {
            static_servers: vec![StaticServer {
                peer: server.clone(),
                address: configured.to_string(),
            }],
            use_authorized_identify_servers: false,
            required_distinct_successes: 2,
            success_evidence_ttl_ms: 15 * 60 * 1000,
            refresh_interval_ms: 5 * 60 * 1000,
            max_candidate_addresses_per_cycle: 4,
        }),
        ..SubstrateConfig::default()
    };
    let runtime = SwarmRuntime::start(&subject, config, trusting(&server)).expect("starts");

    assert!(
        runtime.is_operator_address(&"/ip4/192.168.1.20/tcp/4001".parse().expect("valid")),
        "the operator's own LAN server is admitted whatever its class, so it must be \
         recorded at start rather than met later as a stranger's private address"
    );

    runtime.shutdown().await.expect("clean shutdown");
}

/// The static bootstrap seed, the case rule 9 was written for: a
/// `/dns4` name the profile configures reaches Kademlia as a discovery
/// hint, and it is admitted only because configuration recorded it here
/// at start (#111 DNS review P2-2). The unconfigured name beside it is
/// the control.
#[tokio::test]
async fn a_configured_bootstrap_seed_is_an_operator_address_from_the_start() {
    let subject = ProfileIdentity::generate();
    let peer = ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id");
    let config = SubstrateConfig {
        operator_addresses: vec![format!("/dns4/boot.example/tcp/4001/p2p/{}", peer.as_str())],
        ..SubstrateConfig::default()
    };
    let runtime = SwarmRuntime::start(&subject, config, trusting(&peer)).expect("starts");

    assert!(
        runtime.is_operator_address(&"/dns4/boot.example/tcp/4001".parse().expect("valid")),
        "the operator's configured seed is recorded at start, judged on its route"
    );
    assert!(
        !runtime.is_operator_address(
            &"/dns4/not-configured.example/tcp/4001"
                .parse()
                .expect("valid")
        ),
        "and a name the configuration never gave is still a peer's"
    );

    runtime.shutdown().await.expect("clean shutdown");
}

/// Configuration is refused whole, never seeded in part: an entry that
/// does not parse, or more entries than the set holds.
#[test]
fn operator_addresses_the_set_cannot_hold_are_refused_at_validation() {
    let unparsed = SubstrateConfig {
        operator_addresses: vec!["not a multiaddr".to_owned()],
        ..SubstrateConfig::default()
    };
    assert!(unparsed.validate().is_err());
    let too_many = SubstrateConfig {
        operator_addresses: (0..=interweave_transport_libp2p::operator_set::MAX_OPERATOR_ADDRESSES)
            .map(|i| format!("/ip4/10.0.{}.{}/tcp/1", i / 256, i % 256))
            .collect(),
        ..SubstrateConfig::default()
    };
    assert!(too_many.validate().is_err());
    let at_the_bound = SubstrateConfig {
        operator_addresses: too_many.operator_addresses[1..].to_vec(),
        ..SubstrateConfig::default()
    };
    assert!(
        at_the_bound.validate().is_ok(),
        "the control: the bound itself is legal"
    );
}
