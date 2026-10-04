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
use interweave_transport_api::{ChannelId, EndpointId, TransportError, TransportIdentity};

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

// --- what the fake produces itself (its README's "network's own outcomes")

/// A direct send to `b_peer` from a session on `a`, and `a`'s query of
/// its directory, as a client would make them.
async fn send_and_query(
    a: &FakeNode,
    b_peer: &TransportIdentity,
) -> (TransportError, TransportError) {
    use interweave_local_client_api::{DataCapability, SessionRequest};
    use interweave_local_client_api::{DataSessionBinding, DataSessionPort};
    use interweave_transport_api::{DirectDestination, MessageId};
    let request = SessionRequest::new(
        "conformance",
        Some(agent()),
        [
            DataCapability::Commands,
            DataCapability::Events,
            DataCapability::EndpointsQuery,
        ],
    )
    .expect("in bounds");
    let session = a.open(request).await.expect("opens");
    let sent = session
        .send_direct(
            DirectDestination {
                peer: b_peer.clone(),
                endpoint: None,
            },
            MessageId::from_bytes([7; 16]),
            suite::text("hello"),
        )
        .await
        .expect_err("refused");
    let queried = session
        .query_endpoints(b_peer.clone())
        .await
        .expect_err("refused");
    (sent, queried)
}

/// A stopped far end is the PEER unreachable -- not the sender's own
/// `BackendUnavailable`, which means its own runtime stopped. The control:
/// the same calls with the far end running are not refused that way.
#[tokio::test]
async fn a_stopped_far_end_is_peer_unreachable_not_backend_unavailable() {
    let p = pair();
    {
        use interweave_local_client_api::{DataSessionBinding, DataSessionPort};
        // Control: b holds its default endpoint, so the send is accepted.
        let _held = p.b.open(suite::full(Some(&human()))).await.expect("opens");
        let session = p.a.open(suite::full(Some(&agent()))).await.expect("opens");
        session
            .send_direct(
                interweave_transport_api::DirectDestination {
                    peer: p.b_peer.clone(),
                    endpoint: None,
                },
                interweave_transport_api::MessageId::from_bytes([6; 16]),
                suite::text("hello"),
            )
            .await
            .expect("accepted while b runs");
    }
    p.b.stop();
    assert_eq!(
        send_and_query(&p.a, &p.b_peer).await,
        (
            TransportError::PeerUnreachable,
            TransportError::PeerUnreachable
        )
    );
}

/// A dropped far end is unreachable too, from the fake's own state.
#[tokio::test]
async fn a_dropped_far_end_is_peer_unreachable() {
    let Pair { a, b, b_peer, .. } = pair();
    drop(b);
    assert_eq!(
        send_and_query(&a, &b_peer).await,
        (
            TransportError::PeerUnreachable,
            TransportError::PeerUnreachable
        )
    );
}

/// A queue bound no session may carry is refused when the pair is built,
/// never by an `open` that has already taken its lease.
#[test]
#[should_panic(expected = "a session queue holds 1 to 1024 events")]
fn a_queue_bound_over_the_ceiling_is_refused_at_pair() {
    let big = FakeConfig {
        queue_bound: interweave_local_client_api::MAX_EVENT_QUEUE + 1,
        ..config()
    };
    let _ = FakeNetwork::pair(config(), big);
}

#[tokio::test]
async fn item_9_ready_resolves_on_what_waits_and_takes_nothing() {
    let p = pair();
    suite::ready_resolves_on_what_waits_and_takes_nothing(
        &p.a,
        &p.b,
        &p.b_peer,
        &agent(),
        &human(),
    )
    .await;
}

#[tokio::test]
async fn item_10_the_runtimes_state_is_owed_once_at_open() {
    let p = pair();
    suite::the_runtimes_state_is_owed_once_at_open(&p.b).await;
}

/// Item 10's coalescing, which only a binding can drive: three changes
/// unread are one pending state, the last; an unchanged health owes
/// nothing; and `ready` wakes on the change.
#[tokio::test]
async fn the_fakes_state_changes_coalesce_to_the_newest() {
    use interweave_local_client_api::{
        DataSessionBinding as _, DataSessionPort as _, LocalSessionEvent, SessionEvent,
    };
    use interweave_transport_api::Health;
    let p = pair();
    let session = p.b.open(suite::full(None)).await.expect("opens");
    session
        .events(usize::MAX)
        .await
        .expect("the open-time state");
    let health = |events: &[SessionEvent]| -> Vec<Health> {
        events
            .iter()
            .filter_map(|e| match e {
                SessionEvent::Local(LocalSessionEvent::ServerState { health, .. }) => Some(*health),
                _ => None,
            })
            .collect()
    };
    // A second session, drained of its open-time state BEFORE the
    // changes, waiting in `ready` while they happen.
    let other = p.b.open(suite::full(None)).await.expect("opens");
    other.events(usize::MAX).await.expect("its open-time state");
    let waiting = tokio::spawn(async move { other.ready().await });
    tokio::task::yield_now().await;
    p.b.set_health(Health::Healthy);
    assert!(
        session.events(usize::MAX).await.expect("events").is_empty(),
        "unchanged: nothing owed"
    );
    for h in [Health::Degraded, Health::Unavailable, Health::Degraded] {
        p.b.set_health(h);
    }
    assert_eq!(
        health(&session.events(usize::MAX).await.expect("events")),
        [Health::Degraded]
    );
    tokio::time::timeout(suite::PATIENCE, waiting)
        .await
        .expect("a change wakes a waiting session")
        .expect("joins")
        .expect("ready");
}

/// Two tasks waiting in one session's `ready` both end when the node
/// stops: the fake keeps every waiter, not the last one alone.
#[tokio::test]
async fn every_concurrent_ready_ends_when_the_fake_stops() {
    use interweave_local_client_api::{DataSessionBinding as _, DataSessionPort as _};
    let p = pair();
    let session = std::sync::Arc::new(p.b.open(suite::full(None)).await.expect("opens"));
    session
        .events(usize::MAX)
        .await
        .expect("the open-time state");
    let waits: Vec<_> = (0..2)
        .map(|_| {
            let s = std::sync::Arc::clone(&session);
            tokio::spawn(async move { s.ready().await })
        })
        .collect();
    tokio::task::yield_now().await;
    p.b.stop();
    for wait in waits {
        tokio::time::timeout(suite::PATIENCE, wait)
            .await
            .expect("each wait ends")
            .expect("joins")
            .expect("ready");
    }
}

/// Item 10's path half, which only a binding can drive: a session is
/// owed a `PeerPathChanged` only for a peer it has a route to, one per
/// peer carrying the first `previous` and the newest `current`, taken
/// after its messages; a change back to where it started is withdrawn.
#[tokio::test]
async fn path_changes_reach_only_routed_sessions_coalesced_per_peer() {
    use interweave_local_client_api::{
        DataSessionBinding as _, DataSessionPort as _, LocalSessionEvent, SessionEvent,
    };
    use interweave_transport_api::{DirectDestination, MessageId, PeerPath};
    let p = pair();
    let from = p.a.open(suite::full(Some(&agent()))).await.expect("leases");
    let routed = p.b.open(suite::full(Some(&human()))).await.expect("leases");
    let stranger = p.b.open(suite::full(None)).await.expect("opens");
    for s in [&routed, &stranger] {
        s.events(usize::MAX).await.expect("the open-time state");
    }
    let send = |n: u8| {
        from.send_direct(
            DirectDestination {
                peer: p.b_peer.clone(),
                endpoint: Some(human()),
            },
            MessageId::from_bytes([n; 16]),
            suite::text("a route"),
        )
    };
    // A change before the session TAKES a message from the peer is owed
    // nothing: a queued message is not yet a route.
    send(4).await.expect("accepted");
    p.b.path_changed(&p.a_peer, PeerPath::Relayed, PeerPath::Direct, "dcutr", 0);
    let first = routed.events(usize::MAX).await.expect("events");
    assert!(
        matches!(first.as_slice(), [SessionEvent::Direct(_)]),
        "only the message: taking it makes the route {first:?}"
    );

    // A round trip is withdrawn.
    p.b.path_changed(&p.a_peer, PeerPath::Relayed, PeerPath::Direct, "dcutr", 1);
    p.b.path_changed(
        &p.a_peer,
        PeerPath::Direct,
        PeerPath::Relayed,
        "direct_lost",
        2,
    );
    assert!(
        routed.events(usize::MAX).await.expect("events").is_empty(),
        "relayed -> direct -> relayed is withdrawn"
    );

    // A message waiting, then a change: the message first.
    send(5).await.expect("accepted");
    p.b.path_changed(&p.a_peer, PeerPath::Relayed, PeerPath::Direct, "dcutr", 3);
    let got = routed.events(usize::MAX).await.expect("events");
    assert!(
        matches!(got.first(), Some(SessionEvent::Direct(_))),
        "the message first: {got:?}"
    );
    let paths: Vec<_> = got
        .iter()
        .filter_map(|e| match e {
            SessionEvent::Local(LocalSessionEvent::PeerPathChanged {
                peer,
                previous,
                current,
                observed_at,
                ..
            }) => Some((peer.clone(), *previous, *current, *observed_at)),
            _ => None,
        })
        .collect();
    assert_eq!(
        paths,
        [(p.a_peer.clone(), PeerPath::Relayed, PeerPath::Direct, 3)],
        "one per peer, the latest, after the message"
    );
    assert!(
        stranger
            .events(usize::MAX)
            .await
            .expect("events")
            .is_empty(),
        "no route, nothing owed"
    );
}
