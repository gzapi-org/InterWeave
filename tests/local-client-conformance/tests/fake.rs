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
async fn item_10_a_route_begin_is_owed_the_peers_path() {
    let p = pair();
    suite::a_route_begin_is_owed_the_peers_path(
        &p.a,
        &p.b,
        &p.a_peer,
        &p.b_peer,
        &agent(),
        &human(),
        interweave_transport_api::PeerPath::Direct,
    )
    .await;
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
async fn a_session_reports_its_profile_peer() {
    let p = pair();
    suite::a_session_reports_its_profile_peer(&p.a, &p.a_peer, &p.b_peer).await;
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
async fn item_9_an_ended_session_answers_events_with_its_end() {
    let p = pair();
    let session = suite::a_session_to_end(&p.a, &human()).await;
    p.a.stop();
    suite::an_ended_session_answers_events_with_its_end(&session).await;
}

/// The case with something waiting at the end, degenerate here: the
/// fake's queues are its runtime's, and go with it.
#[tokio::test]
async fn item_9_a_message_waiting_at_the_end_goes_with_the_fakes_runtime() {
    let p = pair();
    let (session, _sender) =
        suite::a_session_to_end_with_a_message_waiting(&p.b, &p.a, &p.a_peer, &agent(), &human())
            .await;
    p.a.stop();
    suite::an_ended_session_answers_events_with_its_end(&session).await;
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
    // nothing as a change: a queued message is not yet a route. Taking it
    // begins the route, owed the path then with nothing before it.
    send(4).await.expect("accepted");
    p.b.path_changed(&p.a_peer, PeerPath::Relayed, PeerPath::Direct, "dcutr", 0);
    let first = routed.events(usize::MAX).await.expect("events");
    assert!(
        matches!(
            first.as_slice(),
            [
                SessionEvent::Direct(_),
                SessionEvent::Local(LocalSessionEvent::PeerPathChanged {
                    previous: None,
                    current: PeerPath::Direct,
                    reason_class,
                    ..
                }),
            ] if reason_class == interweave_local_client_api::ROUTE_ESTABLISHED
        ),
        "the message, then its route's begin -- not the dcutr change {first:?}"
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
        [(
            p.a_peer.clone(),
            Some(PeerPath::Relayed),
            PeerPath::Direct,
            3
        )],
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

/// The broadcast half of route-on-take: a broadcast the session has not
/// yet TAKEN is no route, so a change before the take is owed nothing;
/// once taken, the next change is.
#[tokio::test]
async fn a_broadcast_is_a_route_once_taken() {
    use interweave_local_client_api::{
        DataSessionBinding as _, DataSessionPort as _, LocalSessionEvent, SessionEvent,
    };
    use interweave_transport_api::{BroadcastMessageV1, MessageId, PeerPath};
    let p = pair();
    let channel = ChannelId::parse("general").expect("channel");
    let publisher = p.a.open(suite::full(None)).await.expect("opens");
    let listener = p.b.open(suite::full(None)).await.expect("opens");
    listener
        .events(usize::MAX)
        .await
        .expect("the open-time state");
    publisher.join(channel.clone()).await.expect("joins");
    listener.join(channel.clone()).await.expect("joins");
    let publish = |n: u8| {
        publisher.broadcast(
            channel.clone(),
            BroadcastMessageV1 {
                message_id: MessageId::from_bytes([n; 16]),
                sent_at_ms: 1,
                payload: suite::text("to the channel"),
            },
        )
    };
    let is_path = |e: &SessionEvent| {
        matches!(
            e,
            SessionEvent::Local(LocalSessionEvent::PeerPathChanged { .. })
        )
    };

    publish(1).await.expect("published");
    p.b.path_changed(&p.a_peer, PeerPath::Relayed, PeerPath::Direct, "dcutr", 1);
    let first = listener.events(usize::MAX).await.expect("events");
    assert!(
        matches!(
            first.as_slice(),
            [
                SessionEvent::Broadcast(_),
                SessionEvent::Local(LocalSessionEvent::PeerPathChanged {
                    previous: None,
                    reason_class,
                    ..
                }),
            ] if reason_class == interweave_local_client_api::ROUTE_ESTABLISHED
        ),
        "the broadcast, then its route's begin -- the change before the take \
         is not owed as a change {first:?}"
    );
    p.b.path_changed(
        &p.a_peer,
        PeerPath::Direct,
        PeerPath::Relayed,
        "direct_lost",
        2,
    );
    assert!(
        listener
            .events(usize::MAX)
            .await
            .expect("events")
            .iter()
            .any(is_path),
        "taken, it is a route"
    );
}

/// Item 12's return (A 2026-10-09): a routed peer that disconnects and
/// connects again is owed its path with no `previous` (`reconnected`),
/// after the `PeerDisconnected`; the change pending at the disconnect is
/// withdrawn, not delivered after it (the control). A session with no
/// route is told the disconnect and nothing else.
#[tokio::test]
async fn a_routed_peer_connecting_again_is_owed_its_path_with_no_previous() {
    use interweave_local_client_api::{
        DataSessionBinding as _, DataSessionPort as _, LocalSessionEvent, RECONNECTED, SessionEvent,
    };
    use interweave_transport_api::{DirectDestination, MessageId, PeerPath};
    let p = pair();
    let from = p.a.open(suite::full(Some(&agent()))).await.expect("leases");
    let routed = p.b.open(suite::full(Some(&human()))).await.expect("leases");
    let stranger = p.b.open(suite::full(None)).await.expect("opens");
    from.send_direct(
        DirectDestination {
            peer: p.b_peer.clone(),
            endpoint: Some(human()),
        },
        MessageId::from_bytes([1; 16]),
        suite::text("a route"),
    )
    .await
    .expect("accepted");
    routed
        .events(usize::MAX)
        .await
        .expect("the message and its route");
    stranger
        .events(usize::MAX)
        .await
        .expect("the open-time state");

    p.b.path_changed(
        &p.a_peer,
        PeerPath::Direct,
        PeerPath::Relayed,
        "direct_lost",
        1,
    );
    p.b.disconnected(&p.a_peer);
    p.b.connected(&p.a_peer, PeerPath::Relayed);
    let got: Vec<_> = routed
        .events(usize::MAX)
        .await
        .expect("events")
        .into_iter()
        .filter(|e| {
            !matches!(
                e,
                SessionEvent::Local(LocalSessionEvent::ServerState { .. })
            )
        })
        .collect();
    assert!(
        matches!(
            got.as_slice(),
            [
                SessionEvent::Local(LocalSessionEvent::PeerDisconnected { .. }),
                SessionEvent::Local(LocalSessionEvent::PeerPathChanged {
                    previous: None,
                    current: PeerPath::Relayed,
                    reason_class,
                    ..
                }),
            ] if reason_class == RECONNECTED
        ),
        "the disconnect, then the return with nothing before it: {got:?}"
    );
    let told = stranger.events(usize::MAX).await.expect("events");
    assert!(
        matches!(
            told.as_slice(),
            [SessionEvent::Local(
                LocalSessionEvent::PeerDisconnected { .. }
            )]
        ),
        "no route: the disconnect only {told:?}"
    );
}

/// A session opened before the fake's restart belonged to the runtime
/// before it, and ends with it, as a real runtime's does: its `events`
/// and `close` answer `BackendUnavailable` (#215 review P3) -- before,
/// `events` answered empty, so a ready/events loop spun on nothing.
/// `ready` returns at once, as it already did for a session the
/// restart cleared: pinned, not changed. A session opened after the
/// restart is the control.
#[tokio::test]
async fn a_session_from_before_a_restart_ends_with_it() {
    use interweave_local_client_api::{DataSessionBinding as _, DataSessionPort as _};
    let p = pair();
    let old = p.a.open(suite::full(None)).await.expect("opens");
    assert!(old.events(8).await.is_ok(), "live before the restart");
    p.a.restart();
    assert_eq!(
        old.events(8).await.map(|_| ()),
        Err(TransportError::BackendUnavailable)
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), old.ready())
        .await
        .expect("ready returns at once")
        .expect("and says nothing waits");
    assert_eq!(old.close().await, Err(TransportError::BackendUnavailable));

    let new = p.a.open(suite::full(None)).await.expect("opens after it");
    assert!(new.events(8).await.is_ok(), "the control");
    new.close().await.expect("closes");
}

/// The fake's restart keeps its trust and returns its endpoints to the
/// configuration, as the real runtime does: an endpoint disabled over the
/// admin port is enabled again after it, the default as configured.
#[tokio::test]
async fn a_restart_returns_the_fakes_endpoints_to_the_configuration() {
    use interweave_local_client_api::{AdminBinding as _, AdminCapability, AdminPort as _};
    let p = pair();
    let enabled = |views: &[interweave_local_client_api::EndpointAdminView]| {
        views
            .iter()
            .find(|v| v.endpoint == human())
            .map(|v| v.enabled)
    };
    let admin =
        p.a.admin([AdminCapability::Endpoints].into())
            .await
            .expect("a port");
    let configured_default = admin
        .leases()
        .await
        .expect("rows")
        .iter()
        .any(|v| v.default);
    assert!(
        configured_default,
        "the pair's configuration names a default"
    );
    admin
        .set_endpoint_enabled(human(), false)
        .await
        .expect("disabled");
    let rows = admin.leases().await.expect("rows");
    assert_eq!(enabled(&rows), Some(false));
    p.a.restart();
    let admin =
        p.a.admin([AdminCapability::Endpoints].into())
            .await
            .expect("a port");
    let rows = admin.leases().await.expect("rows");
    assert_eq!(enabled(&rows), Some(true), "as configured again");
    assert_eq!(rows.iter().any(|v| v.default), configured_default);
}

#[tokio::test]
async fn a_revocation_survives_a_restart_of_the_runtime() {
    let p = pair();
    suite::a_revocation_is_made_before_a_restart(&p.a, &p.b_peer).await;
    p.a.restart();
    suite::the_revocation_outlived_the_restart(&p.a, &p.b_peer).await;
}

#[tokio::test]
async fn trust_administration_revokes_as_policy() {
    let p = pair();
    suite::trust_administration_revokes_as_policy(&p.a, &p.a_peer, &p.b_peer).await;
}

#[tokio::test]
async fn peer_rows_answer_under_admin_status() {
    let p = pair();
    suite::peer_rows_answer_under_admin_status(&p.a, &p.a_peer, &p.b_peer).await;
}

/// One direct message from `from` to `endpoint` at `to`, its outcome
/// only.
async fn direct_to(
    from: &interweave_local_client_fake::FakeSession,
    to: &TransportIdentity,
    endpoint: EndpointId,
    n: u8,
) -> Result<(), TransportError> {
    use interweave_local_client_api::DataSessionPort as _;
    use interweave_transport_api::{DirectDestination, MessageId};
    from.send_direct(
        DirectDestination {
            peer: to.clone(),
            endpoint: Some(endpoint),
        },
        MessageId::from_bytes([n; 16]),
        suite::text("trust"),
    )
    .await
    .map(|_| ())
}

/// The broadcasts `session` has received since it was last read.
async fn broadcasts(session: &interweave_local_client_fake::FakeSession) -> usize {
    use interweave_local_client_api::{DataSessionPort as _, SessionEvent};
    session
        .events(64)
        .await
        .expect("reads")
        .into_iter()
        .filter(|e| matches!(e, SessionEvent::Broadcast(_)))
        .count()
}

/// Every way across the pair, as `allowed` says it should be: directs and
/// queries both ways, and a broadcast from each side to the other.
async fn across_the_pair(
    p: &Pair,
    a: &interweave_local_client_fake::FakeSession,
    b: &interweave_local_client_fake::FakeSession,
    allowed: bool,
) {
    use interweave_local_client_api::DataSessionPort as _;
    use interweave_transport_api::{BroadcastMessageV1, MessageId};
    let (a_to_b, b_to_a) = if allowed {
        (Ok(()), Ok(()))
    } else {
        (
            Err(TransportError::UnauthorizedPeer),
            Err(TransportError::PeerUnreachable),
        )
    };
    assert_eq!(direct_to(a, &p.b_peer, human(), 1).await, a_to_b, "a to b");
    assert_eq!(
        a.query_endpoints(p.b_peer.clone()).await.map(|_| ()),
        a_to_b,
        "a queries b"
    );
    assert_eq!(direct_to(b, &p.a_peer, agent(), 2).await, b_to_a, "b to a");
    assert_eq!(
        b.query_endpoints(p.a_peer.clone()).await.map(|_| ()),
        b_to_a,
        "b queries a"
    );
    let general = ChannelId::parse("general").expect("valid");
    // What arrived before is set aside: only the two below are counted.
    let _ = (broadcasts(a).await, broadcasts(b).await);
    for (from, n) in [(a, 3), (b, 4)] {
        from.broadcast(
            general.clone(),
            BroadcastMessageV1 {
                message_id: MessageId::from_bytes([n; 16]),
                sent_at_ms: 1_786_600_000_000,
                payload: suite::text("to the channel"),
            },
        )
        .await
        .expect("published");
    }
    let crossed = usize::from(allowed);
    assert_eq!(broadcasts(b).await, crossed, "a's broadcast at b");
    assert_eq!(broadcasts(a).await, crossed, "b's broadcast at a");
}

/// The fake's own half of a revocation, which the shared check cannot ask
/// of a real runtime without a race: it cuts the pair BOTH ways, as the
/// runtime's closing of the connections does. A's send and query to the
/// revoked B are `UnauthorizedPeer`, B's to A `PeerUnreachable`, and no
/// broadcast crosses in either direction; allowing B again restores all
/// of it. The pass before the revocation is the control.
#[tokio::test]
async fn a_revocation_cuts_the_pair_both_ways_until_allowed_again() {
    use interweave_local_client_api::{
        AdminBinding as _, AdminCapability, AdminPort as _, DataCapability,
        DataSessionBinding as _, DataSessionPort as _, SessionRequest,
    };
    let p = pair();
    let request = |endpoint: EndpointId| {
        SessionRequest::new(
            "conformance",
            Some(endpoint),
            [
                DataCapability::Commands,
                DataCapability::Events,
                DataCapability::EndpointsQuery,
            ],
        )
        .expect("in bounds")
    };
    let general = ChannelId::parse("general").expect("valid");
    let a = p.a.open(request(agent())).await.expect("leases");
    let b = p.b.open(request(human())).await.expect("leases");
    a.join(general.clone()).await.expect("joins");
    b.join(general).await.expect("joins");
    let admin =
        p.a.admin([AdminCapability::Trust].into())
            .await
            .expect("a port");
    across_the_pair(&p, &a, &b, true).await;
    admin
        .set_trust(p.b_peer.clone(), false)
        .await
        .expect("revoked");
    across_the_pair(&p, &a, &b, false).await;
    admin
        .set_trust(p.b_peer.clone(), true)
        .await
        .expect("allowed");
    across_the_pair(&p, &a, &b, true).await;
}
