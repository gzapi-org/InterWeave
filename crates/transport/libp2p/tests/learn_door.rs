// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The peer's door into the address book (plan §15, ADR-0052 rules 8 and
//! 9): `SwarmRuntime::learn`, where Stage 12's composition hands a
//! discovery candidate to the runtime.
//!
//! What must hold, each beside its control:
//!
//! - an address outside the boundary is refused before the book, counted
//!   by class where an operator reads the counts, and never becomes an
//!   operator address -- while `add_address`, the operator's door, takes
//!   the same address (the control that the two doors differ);
//! - a lawful address enters the book and `dial_peer` reaches the peer
//!   through it, over real sockets -- and without it `dial_peer` has no
//!   route (the control that the book entry is what the dial used);
//! - an operator's seed passes this door because the SET holds it, not
//!   because this door wrote it;
//! - every circuit is refused, the peer's own included: a candidate's
//!   circuit may be a third party's assertion (#135 review R1), and the
//!   operator's door taking the same circuit is the control.

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::TransportIdentity;
use interweave_transport_libp2p::store_refusals::store;
use interweave_transport_libp2p::{DialRefusal, SubstrateConfig, SwarmEvent, SwarmRuntime};
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

fn peer_id() -> TransportIdentity {
    ProfileIdentity::generate()
        .transport_identity()
        .expect("peer id")
}

#[tokio::test]
async fn a_discovered_address_outside_the_boundary_is_refused_counted_and_never_operator() {
    let subject = ProfileIdentity::generate();
    let peer = peer_id();
    let other = peer_id();
    let runtime =
        SwarmRuntime::start(&subject, SubstrateConfig::default(), trusting(&peer)).expect("starts");

    // One address per refusal class the book's door can give a
    // candidate, plus a string that is not a multiaddr at all.
    let refused = [
        "/ip4/127.0.0.1/tcp/4001".to_owned(), // special_use
        "/dns4/a-peers-choice.example/tcp/4001".to_owned(), // not_literal
        "not a multiaddr".to_owned(),         // not_literal
        "/ip4/192.168.7.7/tcp/4001".to_owned(), // no private listener
        format!(
            "/ip4/1.2.3.4/tcp/4001/p2p/{}/p2p-circuit/p2p/{}",
            other.as_str(),
            other.as_str()
        ), // a circuit to someone else
        format!(
            "/ip4/1.2.3.4/tcp/4001/p2p/{}/p2p-circuit/p2p/{}",
            other.as_str(),
            peer.as_str()
        ), // a circuit to THIS peer: Identify's clause, not discovery's
    ];
    let learned = runtime
        .learn(peer.clone(), refused.iter().cloned())
        .await
        .expect("the command reaches the task");
    assert_eq!(learned, 0, "nothing outside the boundary enters the book");

    let book = runtime
        .store_refusals()
        .get(store::ADDRESS_BOOK)
        .cloned()
        .unwrap_or_default();
    assert_eq!(book.admitted, 0);
    assert_eq!(book.refused.get("special_use"), Some(&1), "{book:?}");
    assert_eq!(book.refused.get("not_literal"), Some(&2), "{book:?}");
    assert_eq!(
        book.refused.get("private_without_private_listener"),
        Some(&1),
        "{book:?}"
    );
    // EVERY circuit, the peer's own included (#135 review R1): a
    // candidate's circuit may be a third party's assertion, so the
    // discovery predicate refuses the class Identify's admits for the
    // advertiser itself.
    assert_eq!(book.refused.get("relayed"), Some(&2), "{book:?}");

    let loopback: Multiaddr = refused[0].parse().expect("valid");
    assert!(
        !runtime.is_operator_address(&loopback),
        "the peer's door never writes the operator set"
    );
    assert!(
        matches!(
            runtime.dial_peer(peer.clone()).await.expect("delivered"),
            Err(DialRefusal::NoKnownAddress)
        ),
        "and the book holds nothing to dial"
    );

    // THE CONTROL: the same address through the operator's door is
    // remembered, so the refusal above was the door and not the book.
    assert!(
        runtime
            .add_address(peer.clone(), loopback.clone())
            .await
            .expect("delivered"),
        "the operator's door admits what the peer's door refused"
    );
    assert!(runtime.is_operator_address(&loopback));
    let own_circuit: Multiaddr = refused[5].parse().expect("valid");
    assert!(
        runtime
            .add_address(peer, own_circuit)
            .await
            .expect("delivered"),
        "and the peer's own circuit is a route the book holds when the operator gives it"
    );

    runtime.shutdown().await.expect("clean shutdown");
}

#[tokio::test]
async fn an_operators_seed_passes_the_peers_door_because_the_set_holds_it() {
    let subject = ProfileIdentity::generate();
    let peer = peer_id();
    let config = SubstrateConfig {
        operator_addresses: vec![format!("/dns4/boot.example/tcp/4001/p2p/{}", peer.as_str())],
        ..SubstrateConfig::default()
    };
    let runtime = SwarmRuntime::start(&subject, config, trusting(&peer)).expect("starts");

    let seeded = runtime
        .learn(
            peer.clone(),
            [format!("/dns4/boot.example/tcp/4001/p2p/{}", peer.as_str())],
        )
        .await
        .expect("delivered");
    assert_eq!(seeded, 1, "the configured seed is the operator's route");
    let unseeded = runtime
        .learn(peer, ["/dns4/not-configured.example/tcp/4001".to_owned()])
        .await
        .expect("delivered");
    assert_eq!(
        unseeded, 0,
        "the control: a name nobody configured is a peer's"
    );

    runtime.shutdown().await.expect("clean shutdown");
}

#[tokio::test]
async fn an_unclassified_peer_gets_no_book_entry_through_the_peers_door() {
    let subject = ProfileIdentity::generate();
    let trusted = peer_id();
    let stranger = peer_id();
    let runtime = SwarmRuntime::start(&subject, SubstrateConfig::default(), trusting(&trusted))
        .expect("starts");
    let address = "/ip4/1.2.3.4/tcp/4001".to_owned();

    assert_eq!(
        runtime
            .learn(stranger, [address.clone()])
            .await
            .expect("delivered"),
        0,
        "a candidate is not trust (ADR-0010): the book is keyed by the allowlist"
    );
    assert_eq!(
        runtime.learn(trusted, [address]).await.expect("delivered"),
        1,
        "the control: the same address for a trusted peer is remembered"
    );

    runtime.shutdown().await.expect("clean shutdown");
}

async fn wait_connected(runtime: &mut SwarmRuntime, peer: &TransportIdentity) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, runtime.next_event()).await {
            Ok(Some(SwarmEvent::Connected { peer: got, .. })) if &got == peer => return,
            Ok(Some(_)) => {}
            Ok(None) => panic!("the runtime stopped before connecting"),
            Err(elapsed) => {
                panic!("no connection to the learned peer within {PATIENCE:?} ({elapsed})")
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_learned_address_is_the_route_dial_peer_takes() {
    let ip = interweave_test_support::net::require_private_interface_v4();
    let subject_id = ProfileIdentity::generate();
    let target_id = ProfileIdentity::generate();
    let subject_peer = subject_id.transport_identity().expect("peer id");
    let target_peer = target_id.transport_identity().expect("peer id");

    let mut subject = SwarmRuntime::start(
        &subject_id,
        SubstrateConfig::default(),
        trusting(&target_peer),
    )
    .expect("subject starts");
    let target = SwarmRuntime::start(
        &target_id,
        SubstrateConfig::default(),
        trusting(&subject_peer),
    )
    .expect("target starts");

    // Rule 3: a private candidate is admitted only beside a private
    // listener of the same family, so the subject binds one first.
    let _ = subject
        .listen(format!("/ip4/{ip}/tcp/0").parse().expect("valid"))
        .await
        .expect("subject listens on the private address");
    let target_addr = target
        .listen(format!("/ip4/{ip}/tcp/0").parse().expect("valid"))
        .await
        .expect("target listens on the private address");

    // THE CONTROL: before the candidate is learned there is no route.
    assert!(matches!(
        subject
            .dial_peer(target_peer.clone())
            .await
            .expect("delivered"),
        Err(DialRefusal::NoKnownAddress)
    ));

    let learned = subject
        .learn(target_peer.clone(), [target_addr.to_string()])
        .await
        .expect("delivered");
    assert_eq!(learned, 1);
    subject
        .dial_peer(target_peer.clone())
        .await
        .expect("delivered")
        .expect("the learned route is admitted");
    wait_connected(&mut subject, &target_peer).await;
    assert!(
        !subject.is_operator_address(&target_addr),
        "a route learned and used is still the peer's"
    );

    subject.shutdown().await.expect("clean shutdown");
    target.shutdown().await.expect("clean shutdown");
}
