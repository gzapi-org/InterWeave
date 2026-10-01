// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The conformance suite against the in-memory fake (plan §17 (2)): the
//! THIRD runner, beside `in_process.rs` and `over_ipc.rs` -- the same
//! generic functions, no binding-specific branch. A client built against
//! `interweave-local-client-fake` is built against a binding that passes
//! what the real ones pass; what a fake cannot honour (the network's own
//! outcomes, the identity Noise proves) is that crate's README's to say.

#![allow(clippy::expect_used, clippy::panic)]

use interweave_local_client_conformance_tests as suite;
use interweave_local_client_fake::{FakeConfig, FakeEndpoint, FakeNetwork, FakeNode};
use interweave_profile_identity::ProfileIdentity;
use interweave_transport_api::{ChannelId, EndpointId, TransportIdentity};

/// The bound the real fixture runs with (`common::QUEUE_BOUND`), so the
/// queue item fills to the same number on every runner.
const QUEUE_BOUND: usize = 2;

fn human() -> EndpointId {
    EndpointId::parse("human").expect("valid")
}

fn agent() -> EndpointId {
    EndpointId::parse("agent").expect("valid")
}

/// One node configured as the real fixture's profile is: `human` the
/// unadvertised default, `agent` advertised.
fn config() -> FakeConfig {
    FakeConfig {
        peer: ProfileIdentity::generate()
            .transport_identity()
            .expect("peer id"),
        endpoints: vec![
            FakeEndpoint::open(human(), false),
            FakeEndpoint::open(agent(), true),
        ],
        default_endpoint: Some(human()),
        queue_bound: QUEUE_BOUND,
    }
}

struct Pair {
    a: FakeNode,
    b: FakeNode,
    a_peer: TransportIdentity,
    b_peer: TransportIdentity,
}

fn pair() -> Pair {
    let (a, b) = FakeNetwork::pair(config(), config());
    Pair {
        a_peer: a.peer().clone(),
        b_peer: b.peer().clone(),
        a,
        b,
    }
}

#[tokio::test]
async fn item_1_the_source_endpoint_is_the_senders_lease() {
    let p = pair();
    suite::the_source_endpoint_is_the_senders_lease(
        &p.a,
        &p.b,
        &p.a_peer,
        &p.b_peer,
        &agent(),
        &human(),
    )
    .await;
}

#[tokio::test]
async fn items_2_and_5_a_lease_is_exclusive_and_released_on_close() {
    let p = pair();
    suite::a_lease_is_exclusive_and_released_on_close(&p.a, &human()).await;
}

#[tokio::test]
async fn item_5_a_dropped_session_releases_its_lease() {
    let p = pair();
    suite::a_dropped_session_releases_its_lease(&p.a, &human()).await;
    assert_eq!(p.a.open_sessions(), 0, "and leaves nothing behind");
}

#[tokio::test]
async fn items_3_and_6_the_queue_is_bounded_and_acceptance_follows_admission() {
    let p = pair();
    suite::the_queue_is_bounded_and_acceptance_follows_admission(&p.a, &p.b, &p.b_peer, &human())
        .await;
}

#[tokio::test]
async fn a_bounded_take_leaves_the_rest_queued_in_order() {
    let p = pair();
    suite::a_bounded_take_leaves_the_rest_queued_in_order(
        &p.a,
        &p.b,
        &p.b_peer,
        &human(),
        &ChannelId::parse("general").expect("valid"),
    )
    .await;
}

#[tokio::test]
async fn item_4_local_refusals_map_exactly() {
    let p = pair();
    suite::local_refusals_map_exactly(
        &p.a,
        &p.b_peer,
        &EndpointId::parse("nowhere").expect("valid"),
    )
    .await;
}

#[tokio::test]
async fn item_8_nothing_is_kept_for_an_unleased_endpoint() {
    let p = pair();
    suite::nothing_is_kept_for_an_unleased_endpoint(&p.a, &p.b, &p.b_peer, &human()).await;
}

#[tokio::test]
async fn broadcast_reaches_joined_sessions_only() {
    let p = pair();
    suite::broadcast_reaches_joined_sessions_only(
        &p.a,
        &p.b,
        &p.a_peer,
        &ChannelId::parse("general").expect("valid"),
    )
    .await;
}

#[tokio::test]
async fn item_7_administration_is_a_separate_authority() {
    let p = pair();
    suite::administration_is_a_separate_authority(&p.a, &human(), &p.b_peer).await;
}

#[tokio::test]
async fn disabling_an_endpoint_revokes_and_never_rebinds() {
    let p = pair();
    suite::disabling_revokes_and_never_rebinds(&p.a, &human()).await;
}

#[tokio::test]
async fn the_admin_view_and_the_default_overlay() {
    let p = pair();
    suite::the_admin_view_and_the_default_overlay(&p.a, &p.a_peer, &human(), &agent()).await;
}

#[tokio::test]
async fn a_directory_query_needs_its_capability() {
    let p = pair();
    suite::a_directory_query_needs_its_capability(&p.a, &p.b, &p.b_peer, &agent()).await;
}
