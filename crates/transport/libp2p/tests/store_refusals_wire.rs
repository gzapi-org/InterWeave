// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! ADR-0052 rule 8's store counts, read where an operator reads them:
//! from `SwarmRuntime::store_refusals()`, OUTSIDE the Swarm task.
//!
//! The #111 re-review found every refusal count write-only in a running
//! node -- each lived inside the task, read only by its own file's unit
//! tests -- while the docs said the counts were how a refusal becomes
//! visible at all. Unit tests cannot show that the fix holds, because
//! they read the counts from inside. This drives a real Identify
//! exchange between two runtimes and reads the result through the
//! public handle.
//!
//! The subject listens on this host's private address. The peer
//! listens there too AND on loopback, so its Identify advertises both.
//! The subject's address-book hook must refuse the loopback address
//! (ADR-0052's floor) and admit the private one (rule 3, beside the
//! subject's own private listener). The admission is the control: a
//! refusal count means nothing unless the same hook is seen admitting
//! too.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::store_refusals::store;
use interweave_transport_libp2p::{SubstrateConfig, SwarmRuntime};
use interweave_transport_runtime::TrustSources;
use interweave_trust_api::{InfrastructureSet, PeerTrustPolicy};
use libp2p::Multiaddr;

/// Bounded: a hung exchange fails the suite with a reason rather than
/// holding CI until the job timeout.
const PATIENCE: Duration = Duration::from_secs(20);

fn trusting(peer: &TransportIdentity) -> TrustSources {
    TrustSources::new(
        PeerTrustPolicy::new([peer.clone()]).expect("one peer"),
        InfrastructureSet::default(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_peers_advertised_loopback_is_refused_and_the_refusal_is_readable_outside_the_task() {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let subject_id = ProfileIdentity::generate();
    let peer_id = ProfileIdentity::generate();
    let subject_peer = subject_id.transport_identity().expect("peer id");
    let peer_peer = peer_id.transport_identity().expect("peer id");

    let mut subject = SwarmRuntime::start(
        &subject_id,
        SubstrateConfig::default(),
        trusting(&peer_peer),
    )
    .expect("subject starts");
    let mut peer = SwarmRuntime::start(
        &peer_id,
        SubstrateConfig::default(),
        trusting(&subject_peer),
    )
    .expect("peer starts");

    let subject_addr = subject
        .listen(format!("/ip4/{ip}/tcp/0").parse().expect("valid"))
        .await
        .expect("subject listens on the private address");
    let _ = peer
        .listen(format!("/ip4/{ip}/tcp/0").parse().expect("valid"))
        .await
        .expect("peer listens on the private address");
    // THE ADDRESS UNDER TEST: advertised through Identify, refused by
    // the subject's floor.
    let _: Multiaddr = peer
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("peer listens on loopback too");

    peer.dial(subject_peer.clone(), subject_addr)
        .await
        .expect("the command reaches the task")
        .expect("the gate admits it");

    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let book = subject
            .store_refusals()
            .get(store::ADDRESS_BOOK)
            .cloned()
            .unwrap_or_default();
        if book.refused.get("special_use").copied().unwrap_or(0) >= 1 && book.admitted >= 1 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the subject never reported, through SwarmRuntime::store_refusals, both the \
             refusal of the peer's advertised loopback address and the admission of its \
             private one; last seen: {book:?}"
        );
        // Drain both runtimes, so neither stalls on a full outbox while
        // the exchange completes.
        tokio::select! {
            _ = subject.next_event() => {}
            _ = peer.next_event() => {}
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }

    subject.shutdown().await.expect("clean shutdown");
    peer.shutdown().await.expect("clean shutdown");
}

#[derive(libp2p::swarm::NetworkBehaviour)]
struct BareRelay {
    identify: libp2p::identify::Behaviour,
    relay: libp2p::relay::Behaviour,
}

/// A bare relay that ADVERTISES hop: listening on `ip` and on loopback,
/// with its private address confirmed as external so the crate's own
/// status logic enables hop. The runtime's relay server cannot stand in
/// for it here -- `RELAY.md` §8 offers hop only while the server holds
/// an AutoNAT-verified address, which no private or loopback listener
/// ever yields -- and what this test reads is the SUBJECT's learned
/// list, not the server. Driven on its own task.
async fn advertising_relay(ip: std::net::Ipv4Addr) -> (TransportIdentity, Multiaddr) {
    use futures::StreamExt as _;
    use libp2p::swarm::SwarmEvent;

    let keys = libp2p::identity::Keypair::generate_ed25519();
    let peer = TransportIdentity::parse(keys.public().to_peer_id().to_base58()).expect("canonical");
    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keys)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("tcp")
        .with_behaviour(|k| BareRelay {
            identify: libp2p::identify::Behaviour::new(libp2p::identify::Config::new(
                "/interweave-store-refusals-relay/1".to_owned(),
                k.public(),
            )),
            relay: libp2p::relay::Behaviour::new(
                k.public().to_peer_id(),
                libp2p::relay::Config::default(),
            ),
        })
        .expect("behaviour")
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(600)))
        .build();
    swarm
        .listen_on(format!("/ip4/{ip}/tcp/0").parse().expect("valid"))
        .expect("listens on the private address");
    swarm
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .expect("listens on loopback");
    let private = loop {
        if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await
            && address.to_string().contains(&ip.to_string())
        {
            break address;
        }
    };
    swarm.add_external_address(private.clone());
    tokio::spawn(async move {
        loop {
            let _ = swarm.select_next_some().await;
        }
    });
    (peer, private)
}

/// The two learned lists dialled EXPLICITLY, whose hooks are their only
/// enforcement (ADR-0052 rule 8), read through the same public handle
/// (#111 review P2-4). The peer serves AutoNAT probes and the bare relay
/// relays, so their Identify qualifies them for one list each; each
/// advertises a private and a loopback address. Each list must refuse
/// the loopback one and admit the private one -- the admission is the
/// control, as above.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_autonat_and_relay_learned_lists_count_through_the_runtime() {
    use interweave_transport_libp2p::runtime::autonat_driver::AutonatClientSettings;
    use interweave_transport_libp2p::runtime::autonat_server_driver::AutonatServerSettings;
    use interweave_transport_libp2p::runtime::relay_driver::RelayClientSettings;

    let ip = interweave_test_support::net::require_private_interface_v4();
    let subject_id = ProfileIdentity::generate();
    let peer_id = ProfileIdentity::generate();
    let subject_peer = subject_id.transport_identity().expect("peer id");
    let peer_peer = peer_id.transport_identity().expect("peer id");
    let (relay_peer, relay_addr) = advertising_relay(ip).await;

    let mut subject = SwarmRuntime::start(
        &subject_id,
        SubstrateConfig {
            autonat_client: Some(AutonatClientSettings {
                static_servers: Vec::new(),
                use_authorized_identify_servers: true,
                required_distinct_successes: 2,
                success_evidence_ttl_ms: 15 * 60 * 1000,
                refresh_interval_ms: 5 * 60 * 1000,
                max_candidate_addresses_per_cycle: 4,
            }),
            relay_client: Some(RelayClientSettings {
                use_authorized_identify_relays: true,
                ..RelayClientSettings::default()
            }),
            ..SubstrateConfig::default()
        },
        TrustSources::new(
            PeerTrustPolicy::new([peer_peer.clone(), relay_peer.clone()]).expect("two peers"),
            InfrastructureSet::default(),
        ),
    )
    .expect("subject starts");
    let mut peer = SwarmRuntime::start(
        &peer_id,
        SubstrateConfig {
            autonat_server: Some(AutonatServerSettings::default()),
            ..SubstrateConfig::default()
        },
        trusting(&subject_peer),
    )
    .expect("peer starts");

    let _ = subject
        .listen(format!("/ip4/{ip}/tcp/0").parse().expect("valid"))
        .await
        .expect("subject listens on the private address");
    let peer_addr = peer
        .listen(format!("/ip4/{ip}/tcp/0").parse().expect("valid"))
        .await
        .expect("peer listens on the private address");
    let _: Multiaddr = peer
        .listen("/ip4/127.0.0.1/tcp/0".parse().expect("valid"))
        .await
        .expect("peer listens on loopback too");

    // The SUBJECT dials: a server offers its protocol on the asker's
    // inbound, and the client only asks servers it dialled.
    subject
        .dial(peer_peer.clone(), peer_addr)
        .await
        .expect("the command reaches the task")
        .expect("the gate admits it");
    subject
        .dial(relay_peer, relay_addr)
        .await
        .expect("the command reaches the task")
        .expect("the gate admits it");

    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let seen = subject.store_refusals();
        let done = [store::AUTONAT_SERVERS, store::RELAY_RESERVATIONS]
            .iter()
            .all(|name| {
                seen.get(name).is_some_and(|c| {
                    c.admitted >= 1 && c.refused.get("special_use").copied().unwrap_or(0) >= 1
                })
            });
        if done {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the subject never reported, through SwarmRuntime::store_refusals, both lists \
             refusing the peer's loopback address and admitting its private one; last seen: \
             {seen:?}"
        );
        tokio::select! {
            _ = subject.next_event() => {}
            _ = peer.next_event() => {}
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }

    subject.shutdown().await.expect("clean shutdown");
    peer.shutdown().await.expect("clean shutdown");
}
